package main

import (
	"bytes"
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"crypto/x509"
	"encoding/json"
	"encoding/pem"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"sync"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"golang.org/x/crypto/ssh"
)

func testStore(t *testing.T) *Store {
	t.Helper()
	dir := t.TempDir()
	key := filepath.Join(dir, "master.key")
	b := make([]byte, 32)
	_, _ = rand.Read(b)
	if err := privateWrite(key, b); err != nil {
		t.Fatal(err)
	}
	s, err := openStore(dir, key)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { s.db.Close() })
	return s
}
func keypair(t *testing.T) ed25519.PrivateKey {
	t.Helper()
	_, k, e := ed25519.GenerateKey(rand.Reader)
	if e != nil {
		t.Fatal(e)
	}
	return k
}
func mustAccount(t *testing.T, s *Store) (string, string) {
	t.Helper()
	id, code, e := s.createAccount("測試帳號", 2)
	if e != nil {
		t.Fatal(e)
	}
	return id, code
}
func TestRegistrationQuotaResetAndDeviceIdentity(t *testing.T) {
	s := testStore(t)
	id, code := mustAccount(t, s)
	keys := []ed25519.PrivateKey{}
	for range 8 {
		keys = append(keys, keypair(t))
	}
	var wg sync.WaitGroup
	var mu sync.Mutex
	success := []string{}
	for _, k := range keys {
		wg.Add(1)
		go func(k ed25519.PrivateKey) {
			defer wg.Done()
			pub := publicText(k.Public().(ed25519.PublicKey))
			if _, e := s.register(code, pub, "device"); e == nil {
				mu.Lock()
				success = append(success, pub)
				mu.Unlock()
			}
		}(k)
	}
	wg.Wait()
	if len(success) != 2 {
		t.Fatalf("quota accepted %d", len(success))
	}
	before, _ := s.authStatus(success[0])
	if _, e := s.register(code, success[0], "renamed"); e != nil {
		t.Fatal(e)
	}
	if e := s.setAccount(id, nil, 1); e != fault("DEVICE_LIMIT_BELOW_CURRENT") {
		t.Fatalf("reduced quota: %v", e)
	}
	fresh, e := s.resetCode(id)
	if e != nil || fresh == code {
		t.Fatal(e)
	}
	if _, e = s.authStatus(success[0]); e != fault("AUTH_REVOKED") {
		t.Fatal("old signature authorization survived reset")
	}
	if _, e = s.register(code, success[0], "device"); e != denied {
		t.Fatal("old code survived reset")
	}
	after, e := s.register(fresh, success[0], "device")
	if e != nil || before.DeviceID != after.DeviceID {
		t.Fatal("reauthorization duplicated device", e)
	}
	enabled := false
	if e = s.setAccount(id, &enabled, 0); e != nil {
		t.Fatal(e)
	}
	if _, e = s.register(fresh, success[0], "device"); e != denied {
		t.Fatal("disabled account reactivated")
	}
	recovered, e := s.code(id)
	if e != nil || recovered != fresh {
		t.Fatal("encrypted code retrieval", e)
	}
	var encrypted []byte
	_ = s.db.QueryRow("SELECT code_cipher FROM accounts WHERE id=?", id).Scan(&encrypted)
	if bytes.Contains(encrypted, []byte(fresh)) {
		t.Fatal("plaintext code stored")
	}
}
func TestSignedProtocolRejectsReplayTamperingAndWrongRole(t *testing.T) {
	s := testStore(t)
	_, code := mustAccount(t, s)
	h := newControl(s)
	key := keypair(t)
	request := func(path string, e Envelope) int {
		b, _ := json.Marshal(e)
		r := httptest.NewRequest("POST", path, bytes.NewReader(b))
		w := httptest.NewRecorder()
		h.ServeHTTP(w, r)
		return w.Code
	}
	e := sign("/v1/activate", activation{code, "test"}, key)
	if request("/v1/activate", e) != 200 {
		t.Fatal("valid activation rejected")
	}
	if request("/v1/activate", e) == 200 {
		t.Fatal("replay accepted")
	}
	e = sign("/v1/status", struct{}{}, key)
	if request("/internal/v1/leases", e) == 200 {
		t.Fatal("cross path signature accepted")
	}
	e = sign("/v1/status", struct{}{}, key)
	e.Payload = "e30="
	e.Timestamp -= 1000
	if request("/v1/status", e) == 200 {
		t.Fatal("stale signature accepted")
	}
	e = sign("/internal/v1/leases", leaseRequest{"node-a", nil}, key)
	if request("/internal/v1/leases", e) == 200 {
		t.Fatal("device impersonated node")
	}
	e = sign("/v1/status", struct{}{}, key)
	if request("/v1/status", e) != 200 {
		t.Fatal("daily signature requires long code")
	}
}
func TestNodeTrialRegistrationFreshnessAndTraffic(t *testing.T) {
	s := testStore(t)
	account, code := mustAccount(t, s)
	deviceKey := publicText(keypair(t).Public().(ed25519.PublicKey))
	a, e := s.register(code, deviceKey, "device")
	if e != nil {
		t.Fatal(e)
	}
	now := time.Now()
	nodes := []string{"node-a", "second"}
	for index, id := range nodes {
		token, e := s.newNode(id, id, "example.com", 2222+index, 100)
		if e != nil {
			t.Fatal(e)
		}
		identity := publicText(keypair(t).Public().(ed25519.PublicKey))
		host := publicText(keypair(t).Public().(ed25519.PublicKey))
		if e = s.join(id, token, identity, host); e != nil {
			t.Fatal(e)
		}
		if e = s.join(id, token, identity, host); e != nil {
			t.Fatal("repeat join", e)
		}
		if e = s.join(id, token, publicText(keypair(t).Public().(ed25519.PublicKey)), host); e != fault("NODE_CONFLICT") {
			t.Fatal("identity overwritten")
		}
		if e = s.grant(account, id, false, false); e != nil {
			t.Fatal(e)
		}
		if s.lease(id, deviceKey, now).Active {
			t.Fatal("pending node exposed to ordinary account")
		}
		if e = s.grant(account, id, true, false); e != nil {
			t.Fatal(e)
		}
		if !s.lease(id, deviceKey, now).Active {
			t.Fatal("trial grant rejected")
		}
		_ = s.report(id, NodeReport{Healthy: true, Sessions: []string{}}, now)
		if e = s.setNode(id, "enabled", 100, 70, 300); e != nil {
			t.Fatal(e)
		}
		day := now.UTC().Format("2006-01-02")
		r := NodeReport{NodeID: id, Healthy: true, Sessions: []string{a.DeviceID}, UploadBPS: 80, DownloadBPS: 10, Days: []DayTraffic{{day, 100, 200}}}
		if e = s.report(id, r, now); e != nil {
			t.Fatal(e)
		}
		r.Days[0].Up = 90
		if e = s.report(id, r, now.Add(time.Second)); e != nil {
			t.Fatal(e)
		}
	}
	snapshot, e := s.snapshot(now.Add(time.Second))
	if e != nil {
		t.Fatal(e)
	}
	if snapshot.ConfirmedOnline != 1 {
		t.Fatal("double counted cross-node device")
	}
	if snapshot.Nodes[0].Days[0].Up != 100 {
		t.Fatal("traffic went backwards")
	}
	stale, _ := s.snapshot(now.Add(32 * time.Second))
	if stale.UnknownNodes != 2 || stale.Nodes[0].Sessions != nil || stale.Nodes[0].Healthy != nil || stale.Nodes[0].Utilization != nil {
		t.Fatal("stale state fabricated values")
	}
	enabled := false
	_ = s.setAccount(account, &enabled, 0)
	for _, id := range nodes {
		if s.lease(id, deviceKey, now).Active {
			t.Fatal("revoked account authorized")
		}
	}
}
func TestNodeEndpointAndIdentityConflictsAreNotOverwritten(t *testing.T) {
	s := testStore(t)
	token, e := s.newNode("first", "first", "EXAMPLE.COM.", 2222, 0)
	if e != nil {
		t.Fatal(e)
	}
	if _, e = s.newNode("second", "second", "example.com", 2222, 0); e != fault("NODE_CONFLICT") {
		t.Fatal("endpoint conflict accepted", e)
	}
	if _, e = s.newNode("first", "first", "other.example", 2222, 0); e != fault("NODE_CONFLICT") {
		t.Fatal("id conflict accepted", e)
	}
	identity := publicText(keypair(t).Public().(ed25519.PublicKey))
	host := publicText(keypair(t).Public().(ed25519.PublicKey))
	if e = s.join("first", token, identity, host); e != nil {
		t.Fatal(e)
	}
	token, e = s.newNode("second", "second", "example.com", 2223, 0)
	if e != nil {
		t.Fatal(e)
	}
	if e = s.join("second", token, identity, host); e != fault("NODE_CONFLICT") {
		t.Fatal("identity conflict accepted", e)
	}
	if e = s.setNode("second", "enabled", 0, 70, 300); e != fault("NODE_NOT_READY") {
		t.Fatal("unregistered node opened", e)
	}
}

func TestCapacityRequiresContinuousFreshSamples(t *testing.T) {
	s := testStore(t)
	_, e := s.newNode("n", "node", "example.com", 2222, 100)
	if e != nil {
		t.Fatal(e)
	}
	now := time.Now()
	r := NodeReport{Healthy: true, Sessions: []string{}, UploadBPS: 80, Days: []DayTraffic{}}
	for i := 0; i <= 300; i += 10 {
		if e = s.report("n", r, now.Add(time.Duration(i)*time.Second)); e != nil {
			t.Fatal(e)
		}
	}
	v, _ := s.snapshot(now.Add(300 * time.Second))
	if !v.Nodes[0].HighLoad {
		t.Fatal("missing high load")
	}
	_ = s.report("n", r, now.Add(400*time.Second))
	v, _ = s.snapshot(now.Add(400 * time.Second))
	if v.Nodes[0].HighLoad {
		t.Fatal("stale gap counted as continuous load")
	}
	r.UploadBPS = 0
	_ = s.report("n", r, now.Add(410*time.Second))
	v, _ = s.snapshot(now.Add(410 * time.Second))
	if v.Nodes[0].HighLoad {
		t.Fatal("load never cleared")
	}
}

func setupGateway(t *testing.T, s *Store, control *httptest.Server, id string) (*Gateway, net.Listener, string) {
	t.Helper()
	dir := t.TempDir()
	ca := filepath.Join(dir, "ca.pem")
	if e := os.WriteFile(ca, pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: control.Certificate().Raw}), 0600); e != nil {
		t.Fatal(e)
	}
	l, e := net.Listen("tcp", "127.0.0.1:0")
	if e != nil {
		t.Fatal(e)
	}
	port := l.Addr().(*net.TCPAddr).Port
	c := GatewayConfig{ID: id, Name: id, Host: "127.0.0.1", Port: port, Listen: l.Addr().String(), Username: "reborn", ControlURL: control.URL, ControlCA: ca, HostKey: filepath.Join(dir, "host"), IdentityKey: filepath.Join(dir, "identity"), Rules: filepath.Join(dir, "rules.json"), StatsFile: filepath.Join(dir, "stats.json")}
	if e = atomicJSON(c.Rules, Rules{Hosts: []string{"game.granbluefantasy.jp"}, Ports: []int{80, 443}}); e != nil {
		t.Fatal(e)
	}
	if e = initGateway(c); e != nil {
		t.Fatal(e)
	}
	token, e := s.newNode(id, id, c.Host, port, 0)
	if e != nil {
		t.Fatal(e)
	}
	if e = joinGateway(c, token); e != nil {
		t.Fatal(e)
	}
	g, e := newGateway(c)
	if e != nil {
		t.Fatal(e)
	}
	_ = s.report(id, NodeReport{Healthy: true, Sessions: []string{}}, time.Now())
	_ = s.setNode(id, "enabled", 0, 70, 300)
	return g, l, dir
}
func sshClient(t *testing.T, g *Gateway, address string, key ed25519.PrivateKey) *ssh.Client {
	t.Helper()
	host, e := loadPrivate(g.config.HostKey)
	if e != nil {
		t.Fatal(e)
	}
	pub, _ := ssh.NewPublicKey(host.Public())
	signer, _ := ssh.NewSignerFromKey(key)
	c, e := ssh.Dial("tcp", address, &ssh.ClientConfig{User: "reborn", Auth: []ssh.AuthMethod{ssh.PublicKeys(signer)}, HostKeyCallback: ssh.FixedHostKey(pub), Timeout: 3 * time.Second})
	if e != nil {
		t.Fatal(e)
	}
	return c
}
func TestTwoGatewaysRevocationAndForwardingBoundaries(t *testing.T) {
	s := testStore(t)
	control := httptest.NewTLSServer(newControl(s))
	defer control.Close()
	account, code := mustAccount(t, s)
	key := keypair(t)
	pub := publicText(key.Public().(ed25519.PublicKey))
	a, e := s.register(code, pub, "device")
	if e != nil {
		t.Fatal(e)
	}
	otherID, otherCode, e := s.createAccount("other", 2)
	if e != nil {
		t.Fatal(e)
	}
	otherKey := keypair(t)
	_, e = s.register(otherCode, publicText(otherKey.Public().(ed25519.PublicKey)), "other")
	if e != nil {
		t.Fatal(e)
	}
	clients := []*ssh.Client{}
	gateways := []*Gateway{}
	cancels := []context.CancelFunc{}
	done := []chan error{}
	for _, id := range []string{"node-a", "node-b"} {
		g, l, _ := setupGateway(t, s, control, id)
		g.dial = func(context.Context, string, uint32) (net.Conn, error) {
			a, b := net.Pipe()
			go func() { defer b.Close(); _, _ = io.Copy(b, b) }()
			return a, nil
		}
		_ = s.grant(account, id, false, false)
		_ = s.grant(otherID, id, false, false)
		ctx, cancel := context.WithCancel(context.Background())
		ch := make(chan error, 1)
		go func() { ch <- g.serve(ctx, l) }()
		cancels = append(cancels, cancel)
		done = append(done, ch)
		gateways = append(gateways, g)
		clients = append(clients, sshClient(t, g, l.Addr().String(), key))
	}
	defer func() {
		for _, c := range clients {
			c.Close()
		}
		for _, cancel := range cancels {
			cancel()
		}
		for _, ch := range done {
			select {
			case e := <-ch:
				if e != nil {
					t.Error(e)
				}
			case <-time.After(8 * time.Second):
				t.Error("gateway shutdown stuck")
			}
		}
	}()
	for _, c := range clients {
		if _, e = c.NewSession(); e == nil {
			t.Fatal("shell allowed")
		}
		if _, e = c.Listen("tcp", "127.0.0.1:0"); e == nil {
			t.Fatal("remote forwarding allowed")
		}
		if _, e = c.Dial("tcp", "example.com:443"); e == nil {
			t.Fatal("non-whitelist allowed")
		}
		stream, e := c.Dial("tcp", "game.granbluefantasy.jp:443")
		if e != nil {
			t.Fatal(e)
		}
		_ = stream.SetDeadline(time.Now().Add(2 * time.Second))
		_, _ = stream.Write([]byte("opaque TLS bytes"))
		b := make([]byte, 16)
		if _, e = io.ReadFull(stream, b); e != nil || string(b) != "opaque TLS bytes" {
			t.Fatal("forwarding failed", e)
		}
		stream.Close()
	}
	other := sshClient(t, gateways[0], clients[0].RemoteAddr().String(), otherKey)
	defer other.Close()
	enabled := false
	_ = s.setAccount(account, &enabled, 0)
	for _, g := range gateways {
		g.refresh()
	}
	for _, c := range clients {
		wait := make(chan error, 1)
		go func(c *ssh.Client) { wait <- c.Wait() }(c)
		select {
		case <-wait:
		case <-time.After(time.Second):
			t.Fatal("revoked connection survived")
		}
	}
	stream, e := other.Dial("tcp", "game.granbluefantasy.jp:443")
	if e != nil {
		t.Fatal("unrelated account interrupted", e)
	}
	stream.Close()
	if l := s.lease("node-a", pub, time.Now()); l.Active || l.DeviceID == a.DeviceID {
		t.Fatal("revocation bypass")
	}
}
func TestExpiryDoesNotWaitForControlRequest(t *testing.T) {
	s := testStore(t)
	control := httptest.NewTLSServer(newControl(s))
	defer control.Close()
	g, l, _ := setupGateway(t, s, control, "expiry")
	defer l.Close()
	a, b := net.Pipe()
	defer b.Close()
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	g.sessions["device"] = &gatewaySession{raw: a, expires: time.Now().Add(-time.Second), stop: cancel}
	g.leases["key"] = cachedLease{deadline: time.Now().Add(-time.Second)}
	// Expiry closes the transport directly and requires no authorization round trip.
	_ = ctx
	start := time.Now()
	g.expire(time.Now())
	if time.Since(start) > 100*time.Millisecond {
		t.Fatal("expiry blocked")
	}
	if len(g.leases) != 0 {
		t.Fatal("expired lease retained")
	}
}
func TestTUIFormsAndSecretMasking(t *testing.T) {
	m := tui{width: 100, height: 30, data: Snapshot{Accounts: []Account{{ID: "01234567890123456789012345678901", Name: "owner", Enabled: true, MaxDevices: 2}}}}
	if bytes.Contains([]byte(m.View().Content), []byte("RBRN-")) {
		t.Fatal("secret revealed without action")
	}
	model, _ := m.Update(tea.KeyPressMsg{Code: 'r', Text: "r"})
	next := model.(tui)
	if next.form == nil || !next.form.confirm {
		t.Fatal("reset confirmation missing")
	}
	if _, e := formRequest(*next.form); e == nil {
		t.Fatal("reset without confirmation")
	}
	next.form.fields[0].value = "YES"
	q, e := formRequest(*next.form)
	if e != nil || q.Action != "account-reset" {
		t.Fatal(e)
	}
	next.secret = "RBRN-fixture"
	model, _ = next.Update(tea.KeyPressMsg{Code: 27})
	if model.(tui).secret != "" {
		t.Fatal("secret not cleared")
	}
}

func TestLeaseTimerSurvivesBlockedControlAndEpochReset(t *testing.T) {
	s := testStore(t)
	var blocked sync.Mutex
	block := false
	release := make(chan struct{})
	control := httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		blocked.Lock()
		wait := block
		blocked.Unlock()
		if wait {
			select {
			case <-release:
			case <-r.Context().Done():
			}
			return
		}
		newControl(s).ServeHTTP(w, r)
	}))
	defer control.Close()
	defer close(release)
	g, l, _ := setupGateway(t, s, control, "timer")
	account, code := mustAccount(t, s)
	key := keypair(t)
	pub := publicText(key.Public().(ed25519.PublicKey))
	_, _ = s.register(code, pub, "device")
	_ = s.grant(account, "timer", false, false)
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() { done <- g.serve(ctx, l) }()
	defer func() { cancel(); <-done }()
	client := sshClient(t, g, l.Addr().String(), key)
	fresh, err := s.resetCode(account)
	if err != nil {
		t.Fatal(err)
	}
	if _, err = s.register(fresh, pub, "device"); err != nil {
		t.Fatal(err)
	}
	g.refresh()
	closed := make(chan error, 1)
	go func() { closed <- client.Wait() }()
	select {
	case <-closed:
	case <-time.After(time.Second):
		t.Fatal("old epoch survived reauthorization")
	}
	client = sshClient(t, g, l.Addr().String(), key)
	defer client.Close()
	g.mu.Lock()
	for _, session := range g.sessions {
		session.expires = time.Now().Add(50 * time.Millisecond)
	}
	g.mu.Unlock()
	blocked.Lock()
	block = true
	blocked.Unlock()
	refreshDone := make(chan struct{})
	go func() { g.refresh(); close(refreshDone) }()
	closed = make(chan error, 1)
	go func() { closed <- client.Wait() }()
	select {
	case <-closed:
	case <-time.After(7 * time.Second):
		t.Fatal("expiry worker waited for blocked Control")
	}
	<-refreshDone
}

func TestGeneratedIPCertificateIsServerIdentityAndInitializationPreservesKeys(t *testing.T) {
	dir := t.TempDir()
	if e := initControl(dir, "127.0.0.1"); e != nil {
		t.Fatal(e)
	}
	before, _ := os.ReadFile(filepath.Join(dir, "tls.key"))
	master, _ := os.ReadFile(filepath.Join(dir, "master.key"))
	pemBytes, _ := os.ReadFile(filepath.Join(dir, "tls.crt"))
	block, _ := pem.Decode(pemBytes)
	cert, e := x509.ParseCertificate(block.Bytes)
	if e != nil || cert.IsCA || cert.VerifyHostname("127.0.0.1") != nil {
		t.Fatal("invalid server certificate", e)
	}
	if e = initControl(dir, "127.0.0.1"); e != nil {
		t.Fatal(e)
	}
	after, _ := os.ReadFile(filepath.Join(dir, "tls.key"))
	afterMaster, _ := os.ReadFile(filepath.Join(dir, "master.key"))
	if !bytes.Equal(before, after) || !bytes.Equal(master, afterMaster) {
		t.Fatal("initialization regenerated identity")
	}
}
