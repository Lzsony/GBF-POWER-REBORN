package main

import (
	"context"
	"crypto/ed25519"
	"encoding/json"
	"errors"
	"io"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"golang.org/x/crypto/ssh"
)

type Rules struct {
	Domains []string `json:"domains,omitempty"`
	Hosts   []string `json:"hosts"`
	Ports   []int    `json:"ports"`
}

func (r Rules) valid() bool {
	if len(r.Hosts)+len(r.Domains) == 0 || len(r.Ports) == 0 {
		return false
	}
	for _, h := range append(append([]string{}, r.Hosts...), r.Domains...) {
		if !validRuleHost(h) {
			return false
		}
	}
	for _, p := range r.Ports {
		if p != 80 && p != 443 {
			return false
		}
	}
	return true
}
func validRuleHost(host string) bool {
	host = strings.TrimRight(strings.ToLower(host), ".")
	if host == "" || len(host) > 253 || net.ParseIP(host) != nil {
		return false
	}
	for _, label := range strings.Split(host, ".") {
		if len(label) == 0 || len(label) > 63 || strings.HasPrefix(label, "-") || strings.HasSuffix(label, "-") {
			return false
		}
		for _, r := range label {
			if !(r >= 'a' && r <= 'z' || r >= '0' && r <= '9' || r == '-') {
				return false
			}
		}
	}
	return true
}
func (r Rules) allows(host string, port uint32) bool {
	host = strings.ToLower(strings.TrimRight(host, "."))
	allowedPort := false
	for _, p := range r.Ports {
		if port == uint32(p) {
			allowedPort = true
		}
	}
	if !allowedPort || !validRuleHost(host) {
		return false
	}
	for _, h := range r.Hosts {
		if host == strings.ToLower(strings.TrimRight(h, ".")) {
			return true
		}
	}
	for _, d := range r.Domains {
		d = strings.ToLower(strings.TrimRight(d, "."))
		if host == d || strings.HasSuffix(host, "."+d) {
			return true
		}
	}
	return false
}
func publicIP(ip net.IP) bool {
	return ip.IsGlobalUnicast() && !ip.IsPrivate() && !ip.IsLoopback() && !ip.IsLinkLocalUnicast() && !ip.IsLinkLocalMulticast() && !ip.IsUnspecified() && !(ip.To4() != nil && ip.To4()[0] == 100 && ip.To4()[1] >= 64 && ip.To4()[1] <= 127) && !(ip.To4() != nil && ip.To4()[0] == 198 && (ip.To4()[1] == 18 || ip.To4()[1] == 19))
}

type Stats struct {
	mu   sync.Mutex
	Days map[string]DayTraffic `json:"days"`
}

func loadStats(path string) (*Stats, error) {
	s := &Stats{Days: map[string]DayTraffic{}}
	if b, err := os.ReadFile(path); err == nil {
		if err = json.Unmarshal(b, s); err != nil {
			return nil, errors.New("invalid gateway counters; restore or inspect before restarting")
		}
	} else if !os.IsNotExist(err) {
		return nil, err
	}
	if s.Days == nil {
		s.Days = map[string]DayTraffic{}
	}
	return s, nil
}
func (s *Stats) add(up bool, n int) {
	s.mu.Lock()
	defer s.mu.Unlock()
	day := time.Now().UTC().Format("2006-01-02")
	d := s.Days[day]
	d.Day = day
	if up {
		d.Up += uint64(n)
	} else {
		d.Down += uint64(n)
	}
	s.Days[day] = d
}
func (s *Stats) values() []DayTraffic {
	s.mu.Lock()
	defer s.mu.Unlock()
	days := []DayTraffic{}
	cutoff := time.Now().UTC().Add(-7 * 24 * time.Hour).Format("2006-01-02")
	for k, d := range s.Days {
		if k < cutoff {
			delete(s.Days, k)
		} else {
			days = append(days, d)
		}
	}
	return days
}
func (s *Stats) save(path string) error {
	days := s.values()
	m := map[string]DayTraffic{}
	for _, d := range days {
		m[d.Day] = d
	}
	return atomicJSON(path, map[string]any{"days": m})
}

type counted struct {
	io.Writer
	g  *Gateway
	up bool
}

func (w counted) Write(b []byte) (int, error) {
	n, e := w.Writer.Write(b)
	w.g.stats.add(w.up, n)
	if w.up {
		w.g.upload.Add(uint64(n))
	} else {
		w.g.download.Add(uint64(n))
	}
	return n, e
}

type cachedLease struct {
	Lease
	deadline time.Time
}
type gatewaySession struct {
	epoch   int
	conn    *ssh.ServerConn
	raw     net.Conn
	key     string
	device  string
	expires time.Time
	stop    context.CancelFunc
}
type Gateway struct {
	config           GatewayConfig
	rules            Rules
	identity         ed25519.PrivateKey
	sshConfig        *ssh.ServerConfig
	client           *http.Client
	mu               sync.Mutex
	sessions         map[string]*gatewaySession
	leases           map[string]cachedLease
	stats            *Stats
	upload, download atomic.Uint64
	healthy          atomic.Bool
	slots            chan struct{}
	channels         chan struct{}
	wg               sync.WaitGroup
	// Tests replace only the transport dialer; production always checks public DNS results.
	dial func(context.Context, string, uint32) (net.Conn, error)
}

func newGateway(c GatewayConfig) (*Gateway, error) {
	if err := c.Validate(); err != nil {
		return nil, err
	}
	var rules Rules
	if err := loadConfig(c.Rules, &rules); err != nil || !rules.valid() {
		return nil, errors.New("invalid target rules")
	}
	host, err := loadPrivate(c.HostKey)
	if err != nil {
		return nil, err
	}
	identity, err := loadPrivate(c.IdentityKey)
	if err != nil {
		return nil, err
	}
	signer, err := ssh.NewSignerFromKey(host)
	if err != nil {
		return nil, err
	}
	client := pinnedClient(c.ControlCA)
	if client == nil {
		return nil, errors.New("invalid control trust certificate")
	}
	stats, err := loadStats(c.StatsFile)
	if err != nil {
		return nil, err
	}
	g := &Gateway{config: c, rules: rules, identity: identity, client: client, stats: stats, sessions: map[string]*gatewaySession{}, leases: map[string]cachedLease{}, slots: make(chan struct{}, 256), channels: make(chan struct{}, 1024)}
	g.dial = g.dialPublic
	g.healthy.Store(true)
	g.sshConfig = &ssh.ServerConfig{MaxAuthTries: 3, ServerVersion: "SSH-2.0-Reborn", PublicKeyCallback: func(meta ssh.ConnMetadata, key ssh.PublicKey) (*ssh.Permissions, error) {
		if meta.User() != c.Username {
			return nil, denied
		}
		text := strings.TrimSpace(string(ssh.MarshalAuthorizedKey(key)))
		if _, err := parsePublic(text); err != nil {
			return nil, denied
		}
		lease, err := g.authorize(text)
		if err != nil {
			return nil, denied
		}
		return &ssh.Permissions{Extensions: map[string]string{"key": text, "device": lease.DeviceID}}, nil
	}}
	g.sshConfig.AddHostKey(signer)
	return g, nil
}
func (g *Gateway) fetch(keys []string) (map[string]cachedLease, error) {
	requested := time.Now()
	var r leaseReply
	err := postSigned(g.client, g.config.ControlURL, "/internal/v1/leases", leaseRequest{g.config.ID, keys}, g.identity, &r)
	if err != nil {
		return nil, err
	}
	if len(r.Leases) != len(keys) {
		return nil, denied
	}
	allowed := map[string]bool{}
	for _, k := range keys {
		allowed[k] = true
	}
	result := map[string]cachedLease{}
	for _, l := range r.Leases {
		if !allowed[l.Key] {
			return nil, denied
		}
		if _, ok := result[l.Key]; ok {
			return nil, denied
		}
		ttl := time.Until(time.Unix(l.Expires, 0))
		if l.Active && (l.DeviceID != digest(l.Key) || ttl <= 0 || ttl > LeaseDuration+time.Second) {
			return nil, denied
		}
		deadline := time.Now().Add(ttl)
		if limit := requested.Add(LeaseDuration); deadline.After(limit) {
			deadline = limit
		}
		result[l.Key] = cachedLease{l, deadline}
	}
	return result, nil
}
func (g *Gateway) authorize(key string) (cachedLease, error) {
	g.mu.Lock()
	cached, ok := g.leases[key]
	g.mu.Unlock()
	if ok && cached.Active && time.Now().Before(cached.deadline) {
		return cached, nil
	}
	leases, err := g.fetch([]string{key})
	if err != nil {
		return cachedLease{}, err
	}
	l := leases[key]
	if !l.Active {
		return l, denied
	}
	g.mu.Lock()
	if len(g.leases) < 512 {
		g.leases[key] = l
	}
	g.mu.Unlock()
	return l, nil
}
func (g *Gateway) refresh() {
	g.mu.Lock()
	keys := make([]string, 0, len(g.sessions))
	for _, s := range g.sessions {
		keys = append(keys, s.key)
	}
	g.mu.Unlock()
	if len(keys) == 0 {
		return
	}
	result, err := g.fetch(keys)
	if err != nil {
		g.healthy.Store(false)
		return
	}
	g.healthy.Store(true)
	g.mu.Lock()
	defer g.mu.Unlock()
	for key, l := range result {
		if !l.Active {
			delete(g.leases, key)
			for _, s := range g.sessions {
				if s.key == key {
					s.stop()
					_ = s.conn.Close()
				}
			}
		} else {
			g.leases[key] = l
			for _, s := range g.sessions {
				if s.key == key {
					if s.epoch != l.Epoch {
						s.stop()
						_ = s.conn.Close()
					} else {
						s.expires = l.deadline
					}
				}
			}
		}
	}
}
func (g *Gateway) expire(now time.Time) {
	g.mu.Lock()
	defer g.mu.Unlock()
	for key, l := range g.leases {
		if !now.Before(l.deadline) {
			delete(g.leases, key)
		}
	}
	for _, s := range g.sessions {
		if !now.Before(s.expires) {
			s.stop()
			_ = s.raw.Close()
		}
	}
}
func (g *Gateway) report(elapsed time.Duration, oldUp, oldDown *uint64) {
	up, down := g.upload.Load(), g.download.Load()
	g.mu.Lock()
	sessions := make([]string, 0, len(g.sessions))
	for id := range g.sessions {
		sessions = append(sessions, id)
	}
	g.mu.Unlock()
	q := NodeReport{NodeID: g.config.ID, Healthy: g.healthy.Load(), Sessions: sessions, UploadBPS: float64(up-*oldUp) * 8 / elapsed.Seconds(), DownloadBPS: float64(down-*oldDown) * 8 / elapsed.Seconds(), Days: g.stats.values()}
	*oldUp = up
	*oldDown = down
	statsOK := g.stats.save(g.config.StatsFile) == nil
	if !statsOK {
		g.healthy.Store(false)
	}
	var response struct {
		OK bool `json:"ok"`
	}
	if err := postSigned(g.client, g.config.ControlURL, "/internal/v1/report", q, g.identity, &response); err != nil || !response.OK {
		g.healthy.Store(false)
	} else {
		g.healthy.Store(statsOK)
	}
}
func (g *Gateway) serve(ctx context.Context, l net.Listener) error {
	defer l.Close()
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	// Separate workers ensure a stalled HTTPS request never stalls expiry enforcement.
	var workers sync.WaitGroup
	workers.Add(3)
	go func() {
		defer workers.Done()
		t := time.NewTicker(5 * time.Second)
		defer t.Stop()
		for {
			select {
			case <-ctx.Done():
				return
			case now := <-t.C:
				g.expire(now)
			}
		}
	}()
	go func() {
		defer workers.Done()
		t := time.NewTicker(15 * time.Second)
		defer t.Stop()
		for {
			select {
			case <-ctx.Done():
				return
			case <-t.C:
				g.refresh()
			}
		}
	}()
	go func() {
		defer workers.Done()
		t := time.NewTicker(10 * time.Second)
		defer t.Stop()
		last := time.Now().Add(-10 * time.Second)
		var up, down uint64
		for {
			now := time.Now()
			g.report(now.Sub(last), &up, &down)
			last = now
			select {
			case <-ctx.Done():
				return
			case <-t.C:
			}
		}
	}()
	go func() {
		<-ctx.Done()
		_ = l.Close()
		g.mu.Lock()
		for _, s := range g.sessions {
			s.stop()
			_ = s.conn.Close()
		}
		g.mu.Unlock()
	}()
	var result error
	for {
		raw, err := l.Accept()
		if err != nil {
			if ctx.Err() == nil {
				result = err
			}
			break
		}
		select {
		case g.slots <- struct{}{}:
			g.wg.Add(1)
			go func() { defer g.wg.Done(); defer func() { <-g.slots }(); defer raw.Close(); g.handle(ctx, raw) }()
		default:
			raw.Close()
		}
	}
	cancel()
	g.wg.Wait()
	workers.Wait()
	if err := g.stats.save(g.config.StatsFile); result == nil {
		result = err
	}
	return result
}
func (g *Gateway) handle(parent context.Context, raw net.Conn) {
	if tcp, ok := raw.(*net.TCPConn); ok {
		_ = tcp.SetNoDelay(true)
	}
	_ = raw.SetDeadline(time.Now().Add(10 * time.Second))
	conn, channels, requests, err := ssh.NewServerConn(raw, g.sshConfig)
	if err != nil {
		return
	}
	defer conn.Close()
	_ = raw.SetDeadline(time.Time{})
	if conn.Permissions == nil {
		return
	}
	key, id := conn.Permissions.Extensions["key"], conn.Permissions.Extensions["device"]
	ctx, cancel := context.WithCancel(parent)
	defer cancel()
	g.mu.Lock()
	lease, ok := g.leases[key]
	if !ok || !lease.Active || lease.DeviceID != id || !time.Now().Before(lease.deadline) {
		g.mu.Unlock()
		return
	}
	session := &gatewaySession{conn: conn, raw: raw, key: key, device: id, expires: lease.deadline, epoch: lease.Epoch, stop: cancel}
	if old := g.sessions[id]; old != nil {
		old.stop()
		_ = old.conn.Close()
	}
	g.sessions[id] = session
	g.mu.Unlock()
	defer func() {
		g.mu.Lock()
		if g.sessions[id] == session {
			delete(g.sessions, id)
		}
		g.mu.Unlock()
	}()
	go ssh.DiscardRequests(requests)
	var forwards sync.WaitGroup
	defer func() { cancel(); _ = conn.Close(); forwards.Wait() }()
	for {
		select {
		case <-ctx.Done():
			return
		case ch, ok := <-channels:
			if !ok {
				return
			}
			if ch.ChannelType() != "direct-tcpip" {
				_ = ch.Reject(ssh.Prohibited, "forwarding only")
				continue
			}
			select {
			case g.channels <- struct{}{}:
				forwards.Add(1)
				go func() { defer forwards.Done(); defer func() { <-g.channels }(); g.forward(ctx, ch) }()
			default:
				_ = ch.Reject(ssh.ResourceShortage, "busy")
			}
		}
	}
}
func (g *Gateway) dialPublic(ctx context.Context, host string, port uint32) (net.Conn, error) {
	addresses, err := net.DefaultResolver.LookupIPAddr(ctx, host)
	if err != nil || len(addresses) == 0 || len(addresses) > 64 {
		return nil, errors.New("DNS unavailable")
	}
	for _, a := range addresses {
		if a.Zone != "" || !publicIP(a.IP) {
			return nil, denied
		}
	}
	var last error
	for _, a := range addresses {
		conn, err := (&net.Dialer{Timeout: 5 * time.Second, KeepAlive: 30 * time.Second}).DialContext(ctx, "tcp", net.JoinHostPort(a.IP.String(), strconv.Itoa(int(port))))
		if err == nil {
			return conn, nil
		}
		last = err
	}
	return nil, last
}
func (g *Gateway) forward(ctx context.Context, newChannel ssh.NewChannel) {
	var target struct {
		Host       string
		Port       uint32
		Origin     string
		OriginPort uint32
	}
	if ssh.Unmarshal(newChannel.ExtraData(), &target) != nil || !g.rules.allows(target.Host, target.Port) {
		_ = newChannel.Reject(ssh.Prohibited, "destination denied")
		return
	}
	timeout, cancel := context.WithTimeout(ctx, 10*time.Second)
	remote, err := g.dial(timeout, strings.ToLower(strings.TrimSuffix(target.Host, ".")), target.Port)
	cancel()
	if err != nil {
		_ = newChannel.Reject(ssh.ConnectionFailed, "destination unavailable")
		return
	}
	defer remote.Close()
	channel, requests, err := newChannel.Accept()
	if err != nil {
		return
	}
	defer channel.Close()
	go ssh.DiscardRequests(requests)
	done := make(chan struct{})
	go func() {
		select {
		case <-ctx.Done():
			remote.Close()
			channel.Close()
		case <-done:
		}
	}()
	defer close(done)
	copied := make(chan struct{})
	go func() {
		_, _ = io.Copy(counted{remote, g, true}, channel)
		remote.Close()
		channel.Close()
		close(copied)
	}()
	_, _ = io.Copy(counted{channel, g, false}, remote)
	remote.Close()
	channel.Close()
	<-copied
}
func initGateway(c GatewayConfig) error {
	if err := c.Validate(); err != nil {
		return err
	}
	if err := os.MkdirAll(filepath.Dir(c.StatsFile), 0700); err != nil {
		return err
	}
	_, hostErr := os.Stat(c.HostKey)
	_, identityErr := os.Stat(c.IdentityKey)
	if (hostErr == nil) != (identityErr == nil) {
		return errors.New("incomplete gateway identity; restore the missing key")
	}
	if err := generateKey(c.HostKey); err != nil {
		return err
	}
	return generateKey(c.IdentityKey)
}
func joinGateway(c GatewayConfig, token string) error {
	identity, err := loadPrivate(c.IdentityKey)
	if err != nil {
		return err
	}
	host, err := loadPrivate(c.HostKey)
	if err != nil {
		return err
	}
	client := pinnedClient(c.ControlCA)
	if client == nil {
		return errors.New("invalid control CA")
	}
	var out struct {
		OK bool `json:"ok"`
	}
	return postSigned(client, c.ControlURL, "/internal/v1/join", joinRequest{c.ID, strings.TrimSpace(token), publicText(host.Public().(ed25519.PublicKey))}, identity, &out)
}
