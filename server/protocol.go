package main

import (
	"bytes"
	"crypto/aes"
	"crypto/cipher"
	"crypto/ed25519"
	"crypto/rand"
	"crypto/sha256"
	"crypto/tls"
	"crypto/x509"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"encoding/pem"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
	"time"
	"unicode"

	"golang.org/x/crypto/ssh"
)

const ProtocolVersion = 1
const LeaseDuration = 45 * time.Second

var identifier = regexp.MustCompile(`^[a-zA-Z0-9][a-zA-Z0-9_-]{0,63}$`)

type fault string

func (e fault) Error() string { return string(e) }

const denied fault = "AUTH_DENIED"

type Envelope struct {
	Key       string `json:"key"`
	Timestamp int64  `json:"timestamp"`
	Nonce     string `json:"nonce"`
	Payload   string `json:"payload"`
	Signature string `json:"signature"`
}

func signatureMessage(path string, e Envelope) []byte {
	return []byte("REBORN-REQUEST-V1\n" + path + "\n" + strconv.FormatInt(e.Timestamp, 10) + "\n" + e.Nonce + "\n" + e.Key + "\n" + e.Payload)
}
func randomString(n int) string {
	b := make([]byte, n)
	if _, err := rand.Read(b); err != nil {
		panic("random source unavailable")
	}
	return base64.RawURLEncoding.EncodeToString(b)
}
func digest(s string) string { v := sha256.Sum256([]byte(s)); return hex.EncodeToString(v[:]) }
func publicText(key ed25519.PublicKey) string {
	p, _ := ssh.NewPublicKey(key)
	return strings.TrimSpace(string(ssh.MarshalAuthorizedKey(p)))
}
func parsePublic(text string) (ed25519.PublicKey, error) {
	key, _, _, rest, err := ssh.ParseAuthorizedKey([]byte(text))
	if err != nil || len(bytes.TrimSpace(rest)) != 0 {
		return nil, denied
	}
	c, ok := key.(ssh.CryptoPublicKey)
	if !ok {
		return nil, denied
	}
	p, ok := c.CryptoPublicKey().(ed25519.PublicKey)
	if !ok {
		return nil, denied
	}
	if publicText(p) != text {
		return nil, denied
	}
	return p, nil
}
func sign(path string, payload any, key ed25519.PrivateKey) Envelope {
	b, _ := json.Marshal(payload)
	e := Envelope{Key: publicText(key.Public().(ed25519.PublicKey)), Timestamp: time.Now().Unix(), Nonce: randomString(24), Payload: base64.StdEncoding.EncodeToString(b)}
	e.Signature = base64.StdEncoding.EncodeToString(ed25519.Sign(key, signatureMessage(path, e)))
	return e
}
func verify(path string, e Envelope, now time.Time) ([]byte, error) {
	p, err := parsePublic(e.Key)
	if err != nil {
		return nil, denied
	}
	nonce, nerr := base64.RawURLEncoding.DecodeString(e.Nonce)
	sig, serr := base64.StdEncoding.DecodeString(e.Signature)
	payload, perr := base64.StdEncoding.DecodeString(e.Payload)
	if nerr != nil || len(nonce) < 16 || len(nonce) > 64 || serr != nil || perr != nil || len(payload) > 64<<10 || e.Timestamp < now.Unix()-60 || e.Timestamp > now.Unix()+60 || !ed25519.Verify(p, signatureMessage(path, e), sig) {
		return nil, denied
	}
	return payload, nil
}
func decode(b []byte, v any) error {
	d := json.NewDecoder(bytes.NewReader(b))
	d.DisallowUnknownFields()
	if err := d.Decode(v); err != nil {
		return fault("INVALID_REQUEST")
	}
	var more any
	if d.Decode(&more) != io.EOF {
		return fault("INVALID_REQUEST")
	}
	return nil
}
func readJSON(r *http.Request, v any) error {
	b, err := io.ReadAll(io.LimitReader(r.Body, 128<<10+1))
	if err != nil || len(b) > 128<<10 {
		return fault("INVALID_REQUEST")
	}
	return decode(b, v)
}
func reply(w http.ResponseWriter, status int, value any) {
	w.Header().Set("Content-Type", "application/json")
	w.Header().Set("Cache-Control", "no-store")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(value)
}
func fail(w http.ResponseWriter, err error) {
	var f fault
	code := "INTERNAL_ERROR"
	status := 500
	if errors.As(err, &f) {
		code = string(f)
		status = 400
		if f == denied || f == "AUTH_REVOKED" {
			status = 403
		}
	}
	reply(w, status, map[string]string{"error": code})
}
func seal(master []byte, account, code string) ([]byte, error) {
	block, err := aes.NewCipher(master)
	if err != nil {
		return nil, err
	}
	g, _ := cipher.NewGCM(block)
	nonce := make([]byte, g.NonceSize())
	if _, err = rand.Read(nonce); err != nil {
		return nil, err
	}
	return g.Seal(nonce, nonce, []byte(code), []byte(account)), nil
}
func unseal(master []byte, account string, data []byte) (string, error) {
	block, err := aes.NewCipher(master)
	if err != nil {
		return "", err
	}
	g, _ := cipher.NewGCM(block)
	if len(data) < g.NonceSize() {
		return "", errors.New("invalid encrypted code")
	}
	b, err := g.Open(nil, data[:g.NonceSize()], data[g.NonceSize():], []byte(account))
	return string(b), err
}
func loadPrivate(path string) (ed25519.PrivateKey, error) {
	b, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	k, err := ssh.ParseRawPrivateKey(b)
	if err != nil {
		return nil, errors.New("invalid device key")
	}
	switch p := k.(type) {
	case ed25519.PrivateKey:
		return p, nil
	case *ed25519.PrivateKey:
		return *p, nil
	}
	return nil, errors.New("Ed25519 key required")
}
func generateKey(path string) error {
	if _, err := os.Stat(path); err == nil {
		_, err = loadPrivate(path)
		return err
	}
	_, k, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		return err
	}
	b, err := ssh.MarshalPrivateKey(k, "reborn")
	if err != nil {
		return err
	}
	return privateWrite(path, pem.EncodeToMemory(b))
}
func privateWrite(path string, b []byte) error {
	if err := os.MkdirAll(filepath.Dir(path), 0700); err != nil {
		return err
	}
	f, err := os.OpenFile(path, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0600)
	if err != nil {
		return err
	}
	_, err = f.Write(b)
	if err == nil {
		err = f.Sync()
	}
	_ = f.Close()
	return err
}
func atomicJSON(path string, v any) error {
	b, err := json.MarshalIndent(v, "", "  ")
	if err != nil {
		return err
	}
	tmp := path + ".pending"
	if err = os.WriteFile(tmp, append(b, '\n'), 0600); err != nil {
		return err
	}
	return os.Rename(tmp, path)
}

type ControlConfig struct {
	Listen      string `json:"listen"`
	PublicURL   string `json:"publicUrl"`
	DataDir     string `json:"dataDir"`
	MasterKey   string `json:"masterKey"`
	AdminSocket string `json:"adminSocket"`
	TLSCert     string `json:"tlsCert"`
	TLSKey      string `json:"tlsKey"`
}
type GatewayConfig struct {
	ID          string `json:"id"`
	Name        string `json:"name"`
	Listen      string `json:"listen"`
	Host        string `json:"host"`
	Port        int    `json:"port"`
	Username    string `json:"username"`
	ControlURL  string `json:"controlUrl"`
	ControlCA   string `json:"controlCa"`
	HostKey     string `json:"hostKey"`
	IdentityKey string `json:"identityKey"`
	Rules       string `json:"rules"`
	StatsFile   string `json:"statsFile"`
}

func loadConfig(path string, v any) error {
	b, err := os.ReadFile(path)
	if err != nil {
		return err
	}
	return decode(b, v)
}
func validHTTPS(value string) bool {
	u, err := url.Parse(value)
	return err == nil && u.Scheme == "https" && u.Host != "" && u.User == nil && u.RawQuery == "" && u.Fragment == "" && (u.Path == "" || u.Path == "/")
}
func (c ControlConfig) Validate() error {
	if _, _, err := net.SplitHostPort(c.Listen); err != nil {
		return err
	}
	if !validHTTPS(c.PublicURL) {
		return errors.New("publicUrl must be HTTPS")
	}
	for _, p := range []string{c.DataDir, c.MasterKey, c.AdminSocket, c.TLSCert, c.TLSKey} {
		if !filepath.IsAbs(p) {
			return errors.New("absolute paths required")
		}
	}
	return nil
}
func (c GatewayConfig) Validate() error {
	if !identifier.MatchString(c.ID) || c.Name == "" || c.Host == "" || c.Port < 1 || c.Port > 65535 || c.Username != "reborn" || !validHTTPS(c.ControlURL) {
		return errors.New("invalid gateway configuration")
	}
	if _, _, err := net.SplitHostPort(c.Listen); err != nil {
		return err
	}
	for _, p := range []string{c.ControlCA, c.HostKey, c.IdentityKey, c.Rules, c.StatsFile} {
		if !filepath.IsAbs(p) {
			return errors.New("absolute paths required")
		}
	}
	return nil
}
func pinnedClient(caFile string) *http.Client {
	roots := x509.NewCertPool()
	b, err := os.ReadFile(caFile)
	if err != nil || !roots.AppendCertsFromPEM(b) {
		return nil
	}
	return &http.Client{Timeout: 5 * time.Second, Transport: &http.Transport{TLSClientConfig: &tls.Config{MinVersion: tls.VersionTLS12, RootCAs: roots}, MaxIdleConns: 8, IdleConnTimeout: 30 * time.Second}, CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}
}
func postSigned(client *http.Client, base, path string, body any, key ed25519.PrivateKey, result any) error {
	e := sign(path, body, key)
	b, _ := json.Marshal(e)
	req, err := http.NewRequest(http.MethodPost, strings.TrimRight(base, "/")+path, bytes.NewReader(b))
	if err != nil {
		return err
	}
	req.Header.Set("Content-Type", "application/json")
	r, err := client.Do(req)
	if err != nil {
		return fault("CONTROL_UNAVAILABLE")
	}
	defer r.Body.Close()
	raw, err := io.ReadAll(io.LimitReader(r.Body, 256<<10))
	if err != nil {
		return err
	}
	if r.StatusCode != 200 {
		var f struct {
			Error string `json:"error"`
		}
		_ = json.Unmarshal(raw, &f)
		if f.Error != "" {
			return fault(f.Error)
		}
		return fault("CONTROL_UNAVAILABLE")
	}
	return decode(raw, result)
}
func safeText(s string) bool {
	if len(s) == 0 || len(s) > 160 {
		return false
	}
	for _, r := range s {
		if unicode.IsControl(r) {
			return false
		}
	}
	return true
}
func stringify(v any) string { b, _ := json.Marshal(v); return string(b) }
func check(err error) {
	if err != nil {
		fmt.Fprintln(os.Stderr, "ERROR:", err)
		os.Exit(1)
	}
}
