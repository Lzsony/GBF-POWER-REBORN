package main

import (
	"context"
	"crypto/ed25519"
	"crypto/tls"
	"crypto/x509"
	"encoding/pem"
	"fmt"
	"net"
	"net/http"
	"net/url"
	"os"
	"os/exec"
	"runtime"
	"strings"
	"time"
)

type Diagnostic struct {
	Check  string `json:"check"`
	Status string `json:"status"`
	Detail string `json:"detail"`
}

func result(name string, err error) Diagnostic {
	if err != nil {
		return Diagnostic{name, "failed", err.Error()}
	}
	return Diagnostic{name, "passed", "OK"}
}
func privateFile(path string, size int) error {
	info, err := os.Lstat(path)
	if err != nil {
		return fmt.Errorf("file unavailable")
	}
	if !info.Mode().IsRegular() || info.Mode().Perm()&0077 != 0 {
		return fmt.Errorf("private file must be regular and mode 0600 or stricter")
	}
	if size > 0 && info.Size() != int64(size) {
		return fmt.Errorf("unexpected file size")
	}
	return nil
}
func clockDiagnostic() Diagnostic {
	if runtime.GOOS != "linux" {
		return Diagnostic{"time-sync", "not_checked", "Linux time service check only"}
	}
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	b, err := exec.CommandContext(ctx, "timedatectl", "show", "-p", "NTPSynchronized", "--value").Output()
	if err != nil {
		return Diagnostic{"time-sync", "not_checked", "timedatectl unavailable"}
	}
	if strings.TrimSpace(string(b)) != "yes" {
		return Diagnostic{"time-sync", "failed", "NTP is not synchronized"}
	}
	return result("time-sync", nil)
}
func certificateDiagnostic(cert, key, host string) Diagnostic {
	pair, err := tls.LoadX509KeyPair(cert, key)
	if err == nil {
		var c *x509.Certificate
		c, err = x509.ParseCertificate(pair.Certificate[0])
		if err == nil {
			err = c.VerifyHostname(host)
			if err == nil && (time.Now().Before(c.NotBefore) || time.Now().After(c.NotAfter)) {
				err = fmt.Errorf("certificate expired or not yet valid")
			}
		}
	}
	return result("tls-certificate", err)
}
func listenerDiagnostic(address string) Diagnostic {
	host, port, err := net.SplitHostPort(address)
	if err != nil {
		return result("listener", err)
	}
	if port == "0" {
		return Diagnostic{"listener", "not_checked", "ephemeral port"}
	}
	if host == "" || host == "0.0.0.0" || host == "::" {
		host = "127.0.0.1"
	}
	c, err := net.DialTimeout("tcp", net.JoinHostPort(host, port), time.Second)
	if err == nil {
		c.Close()
	}
	return result("listener", err)
}
func publicProbe(host string) Diagnostic {
	client := &http.Client{Timeout: 5 * time.Second, Transport: &http.Transport{Proxy: nil}, CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}
	req, _ := http.NewRequest("HEAD", "https://"+host+"/", nil)
	r, err := client.Do(req)
	if err != nil {
		return Diagnostic{host, "failed", "HTTPS probe failed"}
	}
	r.Body.Close()
	status := "passed"
	if r.StatusCode >= 400 {
		status = "failed"
	}
	return Diagnostic{host, status, fmt.Sprintf("HTTP %d; probe from this machine", r.StatusCode)}
}
func controlDoctor(c ControlConfig, s *Store) []Diagnostic {
	u, _ := url.Parse(c.PublicURL)
	host := ""
	if u != nil {
		host = u.Hostname()
	}
	items := []Diagnostic{result("configuration", c.Validate()), result("master-key", privateFile(c.MasterKey, 32)), result("tls-key", privateFile(c.TLSKey, 0)), certificateDiagnostic(c.TLSCert, c.TLSKey, host), clockDiagnostic(), listenerDiagnostic(c.Listen)}
	var value string
	err := s.db.QueryRow("PRAGMA quick_check").Scan(&value)
	if err == nil && value != "ok" {
		err = fmt.Errorf("SQLite integrity check failed")
	}
	items = append(items, result("database", err), Diagnostic{"external-reachability", "not_checked", "Run deploy verification from the administrator machine; local checks do not prove cloud firewall reachability"})
	return items
}
func gatewayDoctor(c GatewayConfig) []Diagnostic {
	items := []Diagnostic{result("configuration", c.Validate()), result("ssh-host-key", privateFile(c.HostKey, 0)), result("node-identity", privateFile(c.IdentityKey, 0)), clockDiagnostic(), listenerDiagnostic(c.Listen)}
	client := pinnedClient(c.ControlCA)
	key, err := loadPrivate(c.IdentityKey)
	if client == nil {
		err = fmt.Errorf("invalid control certificate")
	}
	if err == nil {
		var out leaseReply
		err = postSigned(client, c.ControlURL, "/internal/v1/leases", leaseRequest{c.ID, []string{}}, key, &out)
	}
	items = append(items, result("control-node-authentication", err))
	var rules Rules
	err = loadConfig(c.Rules, &rules)
	if err == nil && !rules.valid() {
		err = fmt.Errorf("invalid whitelist")
	}
	items = append(items, result("target-whitelist", err), publicProbe("game.granbluefantasy.jp"), publicProbe("steam.granbluefantasy.com"), Diagnostic{"device-ssh-end-to-end", "not_checked", "Requires a registered device test from a client"}, Diagnostic{"external-reachability", "not_checked", "Run deploy verification from outside this server"})
	return items
}
func exportPublic(c GatewayConfig) (map[string]string, error) {
	host, err := loadPrivate(c.HostKey)
	if err != nil {
		return nil, err
	}
	id, err := loadPrivate(c.IdentityKey)
	if err != nil {
		return nil, err
	}
	return map[string]string{"nodeId": c.ID, "hostKey": publicText(host.Public().(ed25519.PublicKey)), "identityKey": publicText(id.Public().(ed25519.PublicKey))}, nil
}
func publicCertPEM(path string) error {
	b, err := os.ReadFile(path)
	if err != nil {
		return err
	}
	block, _ := pem.Decode(b)
	if block == nil || block.Type != "CERTIFICATE" {
		return fmt.Errorf("certificate required")
	}
	_, err = x509.ParseCertificate(block.Bytes)
	return err
}
