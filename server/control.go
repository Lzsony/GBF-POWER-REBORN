package main

import (
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/json"
	"encoding/pem"
	"errors"
	"fmt"
	"math/big"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"
)

type activation struct {
	Code string `json:"code"`
	Name string `json:"name"`
}
type joinRequest struct {
	NodeID  string `json:"nodeId"`
	Token   string `json:"token"`
	HostKey string `json:"hostKey"`
}
type leaseRequest struct {
	NodeID string   `json:"nodeId"`
	Keys   []string `json:"keys"`
}
type leaseReply struct {
	Leases []Lease `json:"leases"`
}
type limitedIP struct {
	tokens float64
	at     time.Time
}
type Control struct {
	store *Store
	slots chan struct{}
	mu    sync.Mutex
	ips   map[string]limitedIP
}

func newControl(s *Store) *Control {
	return &Control{store: s, slots: make(chan struct{}, 64), ips: map[string]limitedIP{}}
}
func (c *Control) allow(addr string) bool {
	host, _, _ := net.SplitHostPort(addr)
	now := time.Now()
	c.mu.Lock()
	defer c.mu.Unlock()
	v, ok := c.ips[host]
	if !ok {
		if len(c.ips) > 2048 {
			for k, x := range c.ips {
				if now.Sub(x.at) > time.Minute {
					delete(c.ips, k)
				}
			}
			if len(c.ips) > 2048 {
				return false
			}
		}
		v = limitedIP{60, now}
	}
	v.tokens = min(60, v.tokens+now.Sub(v.at).Seconds()*2)
	v.at = now
	if v.tokens < 1 {
		c.ips[host] = v
		return false
	}
	v.tokens--
	c.ips[host] = v
	return true
}
func (c *Control) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	if r.URL.Path == "/health/live" && r.Method == "GET" {
		reply(w, 200, map[string]bool{"ok": true})
		return
	}
	if r.Method != "POST" {
		reply(w, 405, map[string]string{"error": "METHOD_NOT_ALLOWED"})
		return
	}
	select {
	case c.slots <- struct{}{}:
		defer func() { <-c.slots }()
	default:
		reply(w, 503, map[string]string{"error": "BUSY"})
		return
	}
	if !c.allow(r.RemoteAddr) {
		reply(w, 429, map[string]string{"error": "RATE_LIMITED"})
		return
	}
	var e Envelope
	if err := readJSON(r, &e); err != nil {
		fail(w, err)
		return
	}
	payload, err := verify(r.URL.Path, e, time.Now())
	if err != nil {
		fail(w, err)
		return
	}
	if err = c.store.consume(e); err != nil {
		fail(w, err)
		return
	}
	switch r.URL.Path {
	case "/v1/activate":
		var q activation
		if err = decode(payload, &q); err == nil {
			var out AuthStatus
			out, err = c.store.register(q.Code, e.Key, q.Name)
			if err == nil {
				reply(w, 200, out)
				return
			}
		}
	case "/v1/status":
		var q struct{}
		if err = decode(payload, &q); err == nil {
			var out AuthStatus
			out, err = c.store.authStatus(e.Key)
			if err == nil {
				reply(w, 200, out)
				return
			}
		}
	case "/v1/unbind":
		var q struct{}
		if err = decode(payload, &q); err == nil {
			var a AuthStatus
			a, err = c.store.authStatus(e.Key)
			if err == nil {
				err = c.store.revokeDevice(a.DeviceID)
			}
			if err == nil {
				reply(w, 200, map[string]bool{"ok": true})
				return
			}
		}
	case "/internal/v1/join":
		var q joinRequest
		if err = decode(payload, &q); err == nil {
			err = c.store.join(q.NodeID, q.Token, e.Key, q.HostKey)
			if err == nil {
				reply(w, 200, map[string]bool{"ok": true})
				return
			}
		}
	case "/internal/v1/leases":
		var q leaseRequest
		if err = decode(payload, &q); err == nil {
			var id string
			id, err = c.store.nodeForKey(e.Key)
			if err == nil && (id != q.NodeID || len(q.Keys) > 256) {
				err = denied
			}
			if err == nil {
				out := leaseReply{Leases: []Lease{}}
				now := time.Now()
				for _, key := range q.Keys {
					if _, er := parsePublic(key); er != nil {
						err = denied
						break
					}
					out.Leases = append(out.Leases, c.store.lease(id, key, now))
				}
				if err == nil {
					reply(w, 200, out)
					return
				}
			}
		}
	case "/internal/v1/report":
		var q NodeReport
		if err = decode(payload, &q); err == nil {
			var id string
			id, err = c.store.nodeForKey(e.Key)
			if err == nil && id != q.NodeID {
				err = denied
			}
			if err == nil {
				err = c.store.report(id, q, time.Unix(min(e.Timestamp, time.Now().Unix()), 0))
			}
			if err == nil {
				reply(w, 200, map[string]bool{"ok": true})
				return
			}
		}
	default:
		reply(w, 404, map[string]string{"error": "NOT_FOUND"})
		return
	}
	if err == nil {
		err = fault("INVALID_REQUEST")
	}
	fail(w, err)
}

type AdminRequest struct {
	NodeIDs     []string `json:"nodeIds,omitempty"`
	Action      string   `json:"action"`
	ID          string   `json:"id,omitempty"`
	Name        string   `json:"name,omitempty"`
	MaxDevices  int      `json:"maxDevices,omitempty"`
	Enabled     *bool    `json:"enabled,omitempty"`
	NodeID      string   `json:"nodeId,omitempty"`
	Host        string   `json:"host,omitempty"`
	Port        int      `json:"port,omitempty"`
	CapacityBPS int64    `json:"capacityBps,omitempty"`
	Status      string   `json:"status,omitempty"`
	Preview     bool     `json:"preview,omitempty"`
	Remove      bool     `json:"remove,omitempty"`
	PublicKey   string   `json:"publicKey,omitempty"`
	WarnPercent float64  `json:"warnPercent,omitempty"`
	WarnSeconds int      `json:"warnSeconds,omitempty"`
}

func adminHandler(s *Store, cfg ControlConfig) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != "POST" || r.URL.Path != "/admin" {
			reply(w, 404, map[string]string{"error": "NOT_FOUND"})
			return
		}
		var q AdminRequest
		if err := readJSON(r, &q); err != nil {
			fail(w, err)
			return
		}
		var out any = map[string]bool{"ok": true}
		var err error
		switch q.Action {
		case "snapshot":
			out, err = s.snapshot(time.Now())
		case "account-create":
			if q.MaxDevices == 0 {
				q.MaxDevices = 2
			}
			var id, code string
			id, code, err = s.createAccount(q.Name, q.MaxDevices, q.NodeIDs...)
			out = map[string]string{"id": id, "code": code}
		case "account-code":
			var code string
			code, err = s.code(q.ID)
			out = map[string]string{"code": code}
		case "account-reset":
			var code string
			code, err = s.resetCode(q.ID)
			out = map[string]string{"code": code}
		case "account-set":
			err = s.setAccount(q.ID, q.Enabled, q.MaxDevices)
		case "account-import":
			if _, err = parsePublic(q.PublicKey); err == nil {
				var code string
				code, err = s.code(q.ID)
				if err == nil {
					out, err = s.register(code, q.PublicKey, q.Name)
				}
			}
		case "device-revoke":
			err = s.revokeDevice(q.ID)
		case "grant-set":
			err = s.grant(q.ID, q.NodeID, q.Preview, q.Remove)
		case "node-add":
			var token string
			token, err = s.newNode(q.ID, q.Name, q.Host, q.Port, q.CapacityBPS)
			out = map[string]string{"id": q.ID, "token": token}
		case "node-ticket":
			token := randomString(32)
			var key *string
			if err = s.db.QueryRow("SELECT identity_key FROM nodes WHERE id=?", q.ID).Scan(&key); err == nil {
				if key != nil {
					err = fault("NODE_ALREADY_REGISTERED")
				} else {
					_, err = s.db.Exec("UPDATE nodes SET join_hash=?,join_expires=? WHERE id=?", digest(token), time.Now().Add(10*time.Minute).Unix(), q.ID)
					out = map[string]string{"token": token}
				}
			}
		case "node-set":
			if q.WarnPercent == 0 {
				q.WarnPercent = 70
			}
			if q.WarnSeconds == 0 {
				q.WarnSeconds = 300
			}
			err = s.setNode(q.ID, q.Status, q.CapacityBPS, q.WarnPercent, q.WarnSeconds)
		case "doctor":
			out = controlDoctor(cfg, s)
		case "audit":
			rows, e := s.db.Query("SELECT at,action,resource FROM audit ORDER BY seq DESC LIMIT 100")
			err = e
			if e == nil {
				items := []map[string]any{}
				for rows.Next() {
					var at int64
					var action, resource string
					if e = rows.Scan(&at, &action, &resource); e != nil {
						err = e
						break
					}
					items = append(items, map[string]any{"at": at, "action": action, "resource": resource})
				}
				rows.Close()
				out = items
			}
		default:
			err = fault("INVALID_ACTION")
		}
		if err != nil {
			fail(w, err)
			return
		}
		reply(w, 200, out)
	})
}
func serveControl(ctx context.Context, c ControlConfig) error {
	if err := c.Validate(); err != nil {
		return err
	}
	s, err := openStore(c.DataDir, c.MasterKey)
	if err != nil {
		return err
	}
	defer s.db.Close()
	if err = os.MkdirAll(filepath.Dir(c.AdminSocket), 0700); err != nil {
		return err
	}
	if _, err = os.Stat(c.AdminSocket); err == nil {
		conn, e := net.DialTimeout("unix", c.AdminSocket, time.Second)
		if e == nil {
			conn.Close()
			return errors.New("admin socket in use")
		}
		if info, _ := os.Lstat(c.AdminSocket); info == nil || info.Mode()&os.ModeSocket == 0 {
			return errors.New("admin socket path occupied")
		}
		_ = os.Remove(c.AdminSocket)
	}
	admin, err := net.Listen("unix", c.AdminSocket)
	if err != nil {
		return err
	}
	defer admin.Close()
	defer os.Remove(c.AdminSocket)
	if err = os.Chmod(c.AdminSocket, 0600); err != nil {
		return err
	}
	public, err := net.Listen("tcp", c.Listen)
	if err != nil {
		return err
	}
	defer public.Close()
	api := httpServer(newControl(s))
	ops := httpServer(adminHandler(s, c))
	errs := make(chan error, 2)
	go func() { errs <- api.ServeTLS(public, c.TLSCert, c.TLSKey) }()
	go func() { errs <- ops.Serve(admin) }()
	select {
	case <-ctx.Done():
	case err = <-errs:
	}
	shutdown, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	_ = api.Shutdown(shutdown)
	_ = ops.Shutdown(shutdown)
	if errors.Is(err, http.ErrServerClosed) {
		return nil
	}
	return err
}
func httpServer(h http.Handler) *http.Server {
	return &http.Server{Handler: h, ReadHeaderTimeout: 5 * time.Second, ReadTimeout: 8 * time.Second, WriteTimeout: 10 * time.Second, IdleTimeout: 30 * time.Second, MaxHeaderBytes: 16 << 10}
}
func adminCall(socket string, q AdminRequest, out any) error {
	client := &http.Client{Timeout: 10 * time.Second, Transport: &http.Transport{DialContext: func(ctx context.Context, _, _ string) (net.Conn, error) {
		return (&net.Dialer{}).DialContext(ctx, "unix", socket)
	}}}
	r, err := client.Post("http://localhost/admin", "application/json", strings.NewReader(stringify(q)))
	if err != nil {
		return fault("ADMIN_UNAVAILABLE")
	}
	defer r.Body.Close()
	if r.StatusCode != 200 {
		var f struct {
			Error string `json:"error"`
		}
		_ = json.NewDecoder(r.Body).Decode(&f)
		return fault(f.Error)
	}
	return json.NewDecoder(r.Body).Decode(out)
}
func initControl(dir, host string) error {
	if !validHost(host) {
		return errors.New("invalid control host")
	}
	if err := os.MkdirAll(dir, 0700); err != nil {
		return err
	}
	if _, err := os.Stat(filepath.Join(dir, "tls.crt")); err == nil {
		if _, err = os.Stat(filepath.Join(dir, "master.key")); err != nil {
			return errors.New("master key missing; restore it rather than regenerate")
		}
	}
	key := filepath.Join(dir, "master.key")
	if _, err := os.Stat(key); os.IsNotExist(err) {
		b := make([]byte, 32)
		if _, err = rand.Read(b); err != nil {
			return err
		}
		if err = privateWrite(key, b); err != nil {
			return err
		}
	}
	certPath, privPath := filepath.Join(dir, "tls.crt"), filepath.Join(dir, "tls.key")
	_, ce := os.Stat(certPath)
	_, ke := os.Stat(privPath)
	if (ce == nil) != (ke == nil) {
		return errors.New("incomplete TLS identity; restore missing file")
	}
	if ce == nil {
		return nil
	}
	pub, priv, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		return err
	}
	serial, _ := rand.Int(rand.Reader, new(big.Int).Lsh(big.NewInt(1), 128))
	template := &x509.Certificate{SerialNumber: serial, Subject: pkix.Name{CommonName: "Reborn Control"}, NotBefore: time.Now().Add(-time.Minute), NotAfter: time.Now().AddDate(2, 0, 0), KeyUsage: x509.KeyUsageDigitalSignature, ExtKeyUsage: []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth}, IsCA: false, BasicConstraintsValid: true}
	if ip := net.ParseIP(host); ip != nil {
		template.IPAddresses = []net.IP{ip}
	} else {
		template.DNSNames = []string{host}
	}
	cert, err := x509.CreateCertificate(rand.Reader, template, template, pub, priv)
	if err != nil {
		return err
	}
	pk, err := x509.MarshalPKCS8PrivateKey(priv)
	if err != nil {
		return err
	}
	if err = privateWrite(privPath, pem.EncodeToMemory(&pem.Block{Type: "PRIVATE KEY", Bytes: pk})); err != nil {
		return err
	}
	return privateWrite(certPath, pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: cert}))
}
func validHost(s string) bool {
	return s != "" && len(s) <= 253 && !strings.ContainsAny(s, "/\\\x00\n\r @?#") && (net.ParseIP(s) != nil || regexpHost(s))
}
func regexpHost(s string) bool {
	for _, r := range s {
		if !(r >= 'a' && r <= 'z' || r >= 'A' && r <= 'Z' || r >= '0' && r <= '9' || r == '.' || r == '-') {
			return false
		}
	}
	return !strings.HasPrefix(s, "-")
}
func requestDescription(q AdminRequest) string { return fmt.Sprintf("%s %s", q.Action, q.ID) }
