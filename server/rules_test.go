package main

import (
	"context"
	"crypto/ed25519"
	"encoding/json"
	"io"
	"net"
	"net/http/httptest"
	"os"
	"testing"
)

func TestSharedTargetRules(t *testing.T) {
	// Load the named Rust arrays' generated fixture via equivalent explicit rules.
	r := Rules{Hosts: []string{"game.granbluefantasy.jp", "steam.granbluefantasy.com", "granbluefantasy.jp", "granbluefantasy.com", "prd-game-a-granbluefantasy.akamaized.net", "prd-game-a-granbluefantasy-steam.akamaized.net", "prd-game-a-gbf.akamaized.net", "code.createjs.com", "code.jquery.com", "cdnjs.cloudflare.com", "cdn.jsdelivr.net", "fonts.fontplus.dev", "www.datadoghq-browser-agent.com"}, Domains: []string{"gamewith.jp", "mbga.jp", "mobage.jp", "dmm.com", "dmm.co.jp", "dmmgames.com"}, Ports: []int{80, 443}}
	data, err := os.ReadFile("../tests/fixtures/target-rules.json")
	if err != nil {
		t.Fatal(err)
	}
	var cases []struct {
		Host   string `json:"host"`
		Target bool   `json:"target"`
	}
	if err = json.Unmarshal(data, &cases); err != nil {
		t.Fatal(err)
	}
	if !r.valid() {
		t.Fatal("rules rejected")
	}
	for _, c := range cases {
		for _, port := range []uint32{80, 443} {
			if r.allows(c.Host, port) != c.Target {
				t.Errorf("%s:%d", c.Host, port)
			}
		}
		if r.allows(c.Host, 22) {
			t.Errorf("unexpected port %s", c.Host)
		}
	}
}
func TestRulesOptionalDomainsAndInvalidDomains(t *testing.T) {
	var r Rules
	if err := json.Unmarshal([]byte(`{"hosts":["game.granbluefantasy.jp"],"ports":[80,443]}`), &r); err != nil {
		t.Fatal(err)
	}
	if !r.valid() || !r.allows("GAME.GRANBLUEFANTASY.JP.", 443) || r.allows("login.mobage.jp", 443) {
		t.Fatal("host-only rules changed")
	}
	for _, h := range []string{"", "*.mobage.jp", "127.0.0.1", "https://mobage.jp", ".mobage.jp", "a..mobage.jp", "a-.mobage.jp", "-a.mobage.jp", "user@mobage.jp"} {
		r.Domains = []string{h}
		if r.valid() {
			t.Errorf("accepted %q", h)
		}
	}
}

func TestDomainRoutesOverIsolatedSSHGateway(t *testing.T) {
	s := testStore(t)
	control := httptest.NewTLSServer(newControl(s))
	defer control.Close()
	account, code := mustAccount(t, s)
	key := keypair(t)
	if _, err := s.register(code, publicText(key.Public().(ed25519.PublicKey)), "rules-device"); err != nil {
		t.Fatal(err)
	}
	g, l, _ := setupGateway(t, s, control, "rules-node")
	g.rules.Domains = []string{"mobage.jp", "dmm.com"}
	g.rules.Hosts = append(g.rules.Hosts, "code.jquery.com")
	g.dial = func(context.Context, string, uint32) (net.Conn, error) {
		a, b := net.Pipe()
		go func() { defer b.Close(); _, _ = io.Copy(b, b) }()
		return a, nil
	}
	if err := s.grant(account, "rules-node", false, false); err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() { done <- g.serve(ctx, l) }()
	c := sshClient(t, g, l.Addr().String(), key)
	defer func() {
		c.Close()
		cancel()
		if err := <-done; err != nil {
			t.Error(err)
		}
	}()
	for _, host := range []string{"mobage.jp", "a.b.mobage.jp", "LOGIN.DMM.COM.", "code.jquery.com"} {
		conn, err := c.Dial("tcp", net.JoinHostPort(host, "443"))
		if err != nil {
			t.Fatal(host, err)
		}
		if _, err = conn.Write([]byte("opaque")); err != nil {
			t.Fatal(err)
		}
		buf := make([]byte, 6)
		if _, err = io.ReadFull(conn, buf); err != nil || string(buf) != "opaque" {
			t.Fatal(host, err)
		}
		conn.Close()
	}
	for _, addr := range []string{"evilmobage.jp:443", "mobage.jp.evil.com:443", "sub.code.jquery.com:443", "mobage.jp:22"} {
		if conn, err := c.Dial("tcp", addr); err == nil {
			conn.Close()
			t.Fatal("unexpected forwarding", addr)
		}
	}
}
