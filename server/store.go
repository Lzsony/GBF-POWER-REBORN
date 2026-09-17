package main

import (
	"crypto/rand"
	"database/sql"
	"encoding/hex"
	"encoding/json"
	"errors"
	"net"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"golang.org/x/crypto/ssh"
	_ "modernc.org/sqlite"
)

type Store struct {
	db     *sql.DB
	master []byte
	mu     sync.Mutex
}

func newID() string { b := make([]byte, 16); _, _ = rand.Read(b); return hex.EncodeToString(b) }
func openStore(dir, keyPath string) (*Store, error) {
	if err := os.MkdirAll(dir, 0700); err != nil {
		return nil, err
	}
	master, err := os.ReadFile(keyPath)
	if err != nil {
		return nil, err
	}
	if len(master) != 32 {
		return nil, errors.New("master key must contain 32 bytes")
	}
	db, err := sql.Open("sqlite", filepath.Join(dir, "control.sqlite"))
	if err != nil {
		return nil, err
	}
	db.SetMaxOpenConns(1)
	var schema int
	if err = db.QueryRow("PRAGMA user_version").Scan(&schema); err != nil || (schema != 0 && schema != 2) {
		db.Close()
		return nil, errors.New("unsupported database schema; use the matching release")
	}
	if schema == 0 {
		var tables int
		if err = db.QueryRow("SELECT count(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'").Scan(&tables); err != nil || tables != 0 {
			db.Close()
			return nil, errors.New("unsupported unversioned database")
		}
	}
	_, err = db.Exec(`PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=3000;
 BEGIN;
 CREATE TABLE IF NOT EXISTS accounts(id TEXT PRIMARY KEY,name TEXT NOT NULL,code_hash TEXT UNIQUE NOT NULL,code_cipher BLOB NOT NULL,epoch INTEGER NOT NULL DEFAULT 1,enabled INTEGER NOT NULL DEFAULT 1,max_devices INTEGER NOT NULL DEFAULT 2,created_at INTEGER NOT NULL);
 CREATE TABLE IF NOT EXISTS devices(id TEXT PRIMARY KEY,public_key TEXT UNIQUE NOT NULL,account_id TEXT NOT NULL REFERENCES accounts(id),name TEXT NOT NULL,epoch INTEGER NOT NULL,revoked INTEGER NOT NULL DEFAULT 0,created_at INTEGER NOT NULL);
 CREATE TABLE IF NOT EXISTS nodes(id TEXT PRIMARY KEY,name TEXT NOT NULL,host TEXT NOT NULL,port INTEGER NOT NULL,status TEXT NOT NULL DEFAULT 'pending',host_key TEXT NOT NULL DEFAULT '',identity_key TEXT UNIQUE,join_hash TEXT UNIQUE,join_expires INTEGER,capacity_bps INTEGER NOT NULL DEFAULT 0,warn_percent REAL NOT NULL DEFAULT 70,warn_seconds INTEGER NOT NULL DEFAULT 300,last_report INTEGER NOT NULL DEFAULT 0,healthy INTEGER NOT NULL DEFAULT 0,sessions TEXT NOT NULL DEFAULT '[]',upload_bps REAL NOT NULL DEFAULT 0,download_bps REAL NOT NULL DEFAULT 0,high_since INTEGER NOT NULL DEFAULT 0,registered_at INTEGER NOT NULL DEFAULT 0);
 CREATE UNIQUE INDEX IF NOT EXISTS node_endpoint ON nodes(lower(host),port);
 CREATE TABLE IF NOT EXISTS grants(account_id TEXT NOT NULL REFERENCES accounts(id),node_id TEXT NOT NULL REFERENCES nodes(id),preview INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(account_id,node_id));
 CREATE TABLE IF NOT EXISTS traffic(node_id TEXT NOT NULL REFERENCES nodes(id),day TEXT NOT NULL,up INTEGER NOT NULL,down INTEGER NOT NULL,PRIMARY KEY(node_id,day));
 CREATE TABLE IF NOT EXISTS nonces(hash TEXT PRIMARY KEY,expires INTEGER NOT NULL);
 CREATE INDEX IF NOT EXISTS nonce_expiry ON nonces(expires);
 CREATE TABLE IF NOT EXISTS audit(seq INTEGER PRIMARY KEY AUTOINCREMENT,at INTEGER NOT NULL,action TEXT NOT NULL,resource TEXT NOT NULL);
 PRAGMA user_version=2; COMMIT;
 `)
	if err != nil {
		db.Close()
		return nil, err
	}
	_ = os.Chmod(filepath.Join(dir, "control.sqlite"), 0600)
	return &Store{db: db, master: master}, nil
}
func (s *Store) audit(action, id string) {
	_, _ = s.db.Exec("INSERT INTO audit(at,action,resource) VALUES(?,?,?)", time.Now().Unix(), action, id)
}
func (s *Store) consume(e Envelope) error {
	now := time.Now().Unix()
	_, err := s.db.Exec("DELETE FROM nonces WHERE expires < ?", now)
	if err != nil {
		return err
	}
	var n int
	if err = s.db.QueryRow("SELECT count(*) FROM nonces").Scan(&n); err != nil {
		return err
	}
	if n > 100000 {
		return fault("BUSY")
	}
	_, err = s.db.Exec("INSERT INTO nonces(hash,expires) VALUES(?,?)", digest(e.Key+"\n"+e.Nonce), now+120)
	if err != nil {
		return fault("REPLAY")
	}
	return nil
}

type Account struct {
	ID         string `json:"id"`
	Name       string `json:"name"`
	Enabled    bool   `json:"enabled"`
	MaxDevices int    `json:"maxDevices"`
	Epoch      int    `json:"epoch"`
	Devices    int    `json:"devices"`
}
type Device struct {
	ID                 string `json:"id"`
	Name               string `json:"name"`
	AccountID          string `json:"accountId"`
	Revoked            bool   `json:"revoked"`
	NeedsAuthorization bool   `json:"needsAuthorization"`
	Fingerprint        string `json:"fingerprint"`
}
type LineInfo struct {
	ID       string `json:"id"`
	Name     string `json:"name"`
	Host     string `json:"host"`
	Port     int    `json:"port"`
	Username string `json:"username"`
	HostKey  string `json:"hostKey"`
}
type AuthStatus struct {
	Active    bool       `json:"active"`
	DeviceID  string     `json:"deviceId"`
	AccountID string     `json:"accountId"`
	Epoch     int        `json:"epoch"`
	Lines     []LineInfo `json:"lines"`
}
type Lease struct {
	Epoch     int    `json:"epoch"`
	Key       string `json:"key"`
	DeviceID  string `json:"deviceId"`
	AccountID string `json:"accountId"`
	Active    bool   `json:"active"`
	Expires   int64  `json:"expires"`
}
type DayTraffic struct {
	Day  string `json:"day"`
	Up   uint64 `json:"up"`
	Down uint64 `json:"down"`
}
type NodeReport struct {
	NodeID      string       `json:"nodeId"`
	Healthy     bool         `json:"healthy"`
	Sessions    []string     `json:"sessions"`
	UploadBPS   float64      `json:"uploadBps"`
	DownloadBPS float64      `json:"downloadBps"`
	Days        []DayTraffic `json:"days"`
}
type Node struct {
	ID          string       `json:"id"`
	Name        string       `json:"name"`
	Host        string       `json:"host"`
	Port        int          `json:"port"`
	Status      string       `json:"status"`
	Registered  bool         `json:"registered"`
	LastReport  int64        `json:"lastReport"`
	Fresh       bool         `json:"fresh"`
	Healthy     *bool        `json:"healthy"`
	Sessions    *[]string    `json:"sessions"`
	UploadBPS   *float64     `json:"uploadBps"`
	DownloadBPS *float64     `json:"downloadBps"`
	CapacityBPS int64        `json:"capacityBps"`
	WarnPercent float64      `json:"warnPercent"`
	WarnSeconds int          `json:"warnSeconds"`
	Utilization *float64     `json:"utilization"`
	HighLoad    bool         `json:"highLoad"`
	Days        []DayTraffic `json:"days"`
}
type Snapshot struct {
	Accounts        []Account `json:"accounts"`
	Devices         []Device  `json:"devices"`
	Nodes           []Node    `json:"nodes"`
	ConfirmedOnline int       `json:"confirmedOnline"`
	UnknownNodes    int       `json:"unknownNodes"`
}

func (s *Store) createAccount(name string, max int, nodeIDs ...string) (string, string, error) {
	if !safeText(name) || max < 1 || max > 100 {
		return "", "", fault("INVALID_ACCOUNT")
	}
	id := newID()
	code := "RBRN-" + randomString(32)
	encrypted, err := seal(s.master, id, code)
	if err != nil {
		return "", "", err
	}
	tx, err := s.db.Begin()
	if err != nil {
		return "", "", err
	}
	defer tx.Rollback()
	seen := make(map[string]bool, len(nodeIDs))
	for _, nodeID := range nodeIDs {
		if !identifier.MatchString(nodeID) || seen[nodeID] {
			return "", "", fault("INVALID_GRANT")
		}
		seen[nodeID] = true
		var status string
		var registered bool
		if err = tx.QueryRow("SELECT status,identity_key IS NOT NULL FROM nodes WHERE id=?", nodeID).Scan(&status, &registered); err != nil || status != "enabled" || !registered {
			return "", "", fault("NODE_NOT_AVAILABLE")
		}
	}
	_, err = tx.Exec("INSERT INTO accounts(id,name,code_hash,code_cipher,max_devices,created_at) VALUES(?,?,?,?,?,?)", id, name, digest(code), encrypted, max, time.Now().Unix())
	for _, nodeID := range nodeIDs {
		if err != nil {
			break
		}
		_, err = tx.Exec("INSERT INTO grants(account_id,node_id,preview) VALUES(?,?,0)", id, nodeID)
	}
	if err == nil {
		err = tx.Commit()
	}
	if err == nil {
		s.audit("account_created", id)
	}
	if err != nil {
		return "", "", err
	}
	return id, code, nil
}
func (s *Store) code(id string) (string, error) {
	var b []byte
	if err := s.db.QueryRow("SELECT code_cipher FROM accounts WHERE id=?", id).Scan(&b); err != nil {
		return "", fault("NOT_FOUND")
	}
	return unseal(s.master, id, b)
}
func (s *Store) resetCode(id string) (string, error) {
	code := "RBRN-" + randomString(32)
	b, err := seal(s.master, id, code)
	if err != nil {
		return "", err
	}
	r, err := s.db.Exec("UPDATE accounts SET code_hash=?,code_cipher=?,epoch=epoch+1 WHERE id=?", digest(code), b, id)
	if err != nil {
		return "", err
	}
	if n, _ := r.RowsAffected(); n != 1 {
		return "", fault("NOT_FOUND")
	}
	s.audit("code_reset", id)
	return code, nil
}
func (s *Store) setAccount(id string, enabled *bool, max int) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	tx, err := s.db.Begin()
	if err != nil {
		return err
	}
	defer tx.Rollback()
	var current int
	if err = tx.QueryRow("SELECT max_devices FROM accounts WHERE id=?", id).Scan(&current); err != nil {
		return fault("NOT_FOUND")
	}
	if max != 0 {
		if max < 1 || max > 100 {
			return fault("INVALID_LIMIT")
		}
		var count int
		_ = tx.QueryRow("SELECT count(*) FROM devices WHERE account_id=? AND revoked=0", id).Scan(&count)
		if count > max {
			return fault("DEVICE_LIMIT_BELOW_CURRENT")
		}
		if _, err = tx.Exec("UPDATE accounts SET max_devices=? WHERE id=?", max, id); err != nil {
			return err
		}
	}
	if enabled != nil {
		if _, err = tx.Exec("UPDATE accounts SET enabled=? WHERE id=?", *enabled, id); err != nil {
			return err
		}
	}
	if err = tx.Commit(); err == nil {
		s.audit("account_updated", id)
	}
	return err
}
func (s *Store) register(code, key, name string) (AuthStatus, error) {
	if !safeText(name) {
		return AuthStatus{}, fault("INVALID_DEVICE")
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	tx, err := s.db.Begin()
	if err != nil {
		return AuthStatus{}, err
	}
	defer tx.Rollback()
	var account string
	var epoch, max int
	var enabled bool
	if err = tx.QueryRow("SELECT id,epoch,max_devices,enabled FROM accounts WHERE code_hash=?", digest(code)).Scan(&account, &epoch, &max, &enabled); err != nil || !enabled {
		return AuthStatus{}, denied
	}
	id := digest(key)
	var oldAccount string
	var revoked bool
	err = tx.QueryRow("SELECT account_id,revoked FROM devices WHERE id=?", id).Scan(&oldAccount, &revoked)
	if err != nil && err != sql.ErrNoRows {
		return AuthStatus{}, err
	}
	if err == nil && oldAccount != account && !revoked {
		return AuthStatus{}, fault("DEVICE_BOUND")
	}
	if err == sql.ErrNoRows || revoked || oldAccount != account {
		var count int
		if err = tx.QueryRow("SELECT count(*) FROM devices WHERE account_id=? AND revoked=0", account).Scan(&count); err != nil {
			return AuthStatus{}, err
		}
		if count >= max {
			return AuthStatus{}, fault("DEVICE_LIMIT")
		}
	}
	_, err = tx.Exec(`INSERT INTO devices(id,public_key,account_id,name,epoch,revoked,created_at) VALUES(?,?,?,?,?,0,?) ON CONFLICT(id) DO UPDATE SET account_id=excluded.account_id,name=excluded.name,epoch=excluded.epoch,revoked=0`, id, key, account, name, epoch, time.Now().Unix())
	if err != nil {
		return AuthStatus{}, err
	}
	if err = tx.Commit(); err != nil {
		return AuthStatus{}, err
	}
	s.audit("device_registered", id)
	return s.authStatus(key)
}
func (s *Store) authStatus(key string) (AuthStatus, error) {
	out := AuthStatus{Lines: []LineInfo{}}
	var enabled, revoked bool
	var de int
	err := s.db.QueryRow(`SELECT d.id,a.id,a.epoch,a.enabled,d.revoked,d.epoch FROM devices d JOIN accounts a ON d.account_id=a.id WHERE d.public_key=?`, key).Scan(&out.DeviceID, &out.AccountID, &out.Epoch, &enabled, &revoked, &de)
	if err != nil || !enabled || revoked || de != out.Epoch {
		return out, fault("AUTH_REVOKED")
	}
	rows, err := s.db.Query(`SELECT n.id,n.name,n.host,n.port,n.host_key FROM nodes n JOIN grants g ON g.node_id=n.id WHERE g.account_id=? AND n.identity_key IS NOT NULL AND (n.status='enabled' OR (n.status='pending' AND g.preview=1)) ORDER BY n.id`, out.AccountID)
	if err != nil {
		return out, err
	}
	defer rows.Close()
	for rows.Next() {
		var l LineInfo
		l.Username = "reborn"
		if err = rows.Scan(&l.ID, &l.Name, &l.Host, &l.Port, &l.HostKey); err != nil {
			return out, err
		}
		out.Lines = append(out.Lines, l)
	}
	out.Active = true
	return out, rows.Err()
}
func (s *Store) revokeDevice(id string) error {
	r, err := s.db.Exec("UPDATE devices SET revoked=1 WHERE id=?", id)
	if err != nil {
		return err
	}
	if n, _ := r.RowsAffected(); n != 1 {
		return fault("NOT_FOUND")
	}
	s.audit("device_revoked", id)
	return nil
}
func (s *Store) lease(node, key string, now time.Time) Lease {
	out := Lease{Key: key}
	a, err := s.authStatus(key)
	if err != nil {
		return out
	}
	for _, l := range a.Lines {
		if l.ID == node {
			out.Active = true
			out.DeviceID = a.DeviceID
			out.AccountID = a.AccountID
			out.Epoch = a.Epoch
			out.Expires = now.Add(LeaseDuration).Unix()
		}
	}
	return out
}
func (s *Store) grant(account, node string, preview bool, remove bool) error {
	var err error
	if remove {
		_, err = s.db.Exec("DELETE FROM grants WHERE account_id=? AND node_id=?", account, node)
	} else {
		_, err = s.db.Exec("INSERT INTO grants(account_id,node_id,preview) VALUES(?,?,?) ON CONFLICT(account_id,node_id) DO UPDATE SET preview=excluded.preview", account, node, preview)
	}
	if err == nil {
		s.audit("grant_updated", account+":"+node)
	}
	return err
}
func (s *Store) newNode(id, name, host string, port int, capacity int64) (string, error) {
	host = strings.ToLower(strings.TrimSuffix(host, "."))
	if ip := net.ParseIP(host); ip != nil {
		host = ip.String()
	}
	if !identifier.MatchString(id) || !safeText(name) || !validHost(host) || port < 1 || port > 65535 || capacity < 0 {
		return "", fault("INVALID_NODE")
	}
	token := randomString(32)
	_, err := s.db.Exec("INSERT INTO nodes(id,name,host,port,capacity_bps,join_hash,join_expires) VALUES(?,?,?,?,?,?,?)", id, name, host, port, capacity, digest(token), time.Now().Add(10*time.Minute).Unix())
	if err != nil {
		return "", fault("NODE_CONFLICT")
	}
	s.audit("node_created", id)
	return token, nil
}
func (s *Store) join(id, token, identity, hostKey string) error {
	if _, err := parsePublic(hostKey); err != nil {
		return fault("INVALID_HOST_KEY")
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	var oldIdentity, oldHost, hash sql.NullString
	var expiry sql.NullInt64
	if err := s.db.QueryRow("SELECT identity_key,host_key,join_hash,join_expires FROM nodes WHERE id=?", id).Scan(&oldIdentity, &oldHost, &hash, &expiry); err != nil {
		return denied
	}
	if oldIdentity.Valid {
		if oldIdentity.String == identity && oldHost.String == hostKey {
			return nil
		}
		return fault("NODE_CONFLICT")
	}
	if !hash.Valid || hash.String != digest(token) || time.Now().Unix() > expiry.Int64 {
		return denied
	}
	_, err := s.db.Exec("UPDATE nodes SET identity_key=?,host_key=?,join_hash=NULL,join_expires=NULL,registered_at=? WHERE id=?", identity, hostKey, time.Now().Unix(), id)
	if err != nil {
		return fault("NODE_CONFLICT")
	}
	s.audit("node_registered", id)
	return nil
}
func (s *Store) nodeForKey(key string) (string, error) {
	var id string
	err := s.db.QueryRow("SELECT id FROM nodes WHERE identity_key=?", key).Scan(&id)
	if err != nil {
		return "", denied
	}
	return id, nil
}
func (s *Store) setNode(id, status string, capacity int64, warn float64, seconds int) error {
	if status != "enabled" && status != "pending" && status != "disabled" {
		return fault("INVALID_NODE_STATUS")
	}
	if capacity < 0 || warn <= 0 || warn > 100 || seconds < 1 {
		return fault("INVALID_CAPACITY")
	}
	if status == "enabled" {
		var registered, healthy bool
		var last int64
		if err := s.db.QueryRow("SELECT identity_key IS NOT NULL,healthy,last_report FROM nodes WHERE id=?", id).Scan(&registered, &healthy, &last); err != nil {
			return fault("NOT_FOUND")
		}
		if !registered || !healthy || time.Now().Unix()-last > 30 {
			return fault("NODE_NOT_READY")
		}
	}
	r, err := s.db.Exec("UPDATE nodes SET status=?,capacity_bps=?,warn_percent=?,warn_seconds=? WHERE id=?", status, capacity, warn, seconds, id)
	if err == nil {
		if n, _ := r.RowsAffected(); n != 1 {
			return fault("NOT_FOUND")
		}
		s.audit("node_updated", id)
	}
	return err
}
func (s *Store) report(id string, r NodeReport, now time.Time) error {
	if len(r.Sessions) > 256 || len(r.Days) > 8 || r.UploadBPS < 0 || r.DownloadBPS < 0 || r.UploadBPS > 1e14 || r.DownloadBPS > 1e14 {
		return fault("INVALID_REPORT")
	}
	seen := map[string]bool{}
	for _, d := range r.Sessions {
		if len(d) != 64 || seen[d] {
			return fault("INVALID_REPORT")
		}
		seen[d] = true
	}
	var capacity int64
	var threshold float64
	var high, last int64
	if err := s.db.QueryRow("SELECT capacity_bps,warn_percent,high_since,last_report FROM nodes WHERE id=?", id).Scan(&capacity, &threshold, &high, &last); err != nil {
		return err
	}
	if now.Unix()-last > 30 {
		high = 0
	}
	if capacity > 0 && max(r.UploadBPS, r.DownloadBPS)*100/float64(capacity) >= threshold {
		if high == 0 {
			high = now.Unix()
		}
	} else {
		high = 0
	}
	tx, err := s.db.Begin()
	if err != nil {
		return err
	}
	defer tx.Rollback()
	_, err = tx.Exec("UPDATE nodes SET last_report=?,healthy=?,sessions=?,upload_bps=?,download_bps=?,high_since=? WHERE id=?", now.Unix(), r.Healthy, stringify(r.Sessions), r.UploadBPS, r.DownloadBPS, high, id)
	if err != nil {
		return err
	}
	for _, day := range r.Days {
		date, e := time.Parse("2006-01-02", day.Day)
		if e != nil || date.After(now.UTC()) || date.Before(now.UTC().Add(-8*24*time.Hour)) || day.Up > 1<<62 || day.Down > 1<<62 {
			return fault("INVALID_REPORT")
		}
		if _, err = tx.Exec("INSERT INTO traffic(node_id,day,up,down) VALUES(?,?,?,?) ON CONFLICT(node_id,day) DO UPDATE SET up=max(up,excluded.up),down=max(down,excluded.down)", id, day.Day, day.Up, day.Down); err != nil {
			return err
		}
	}
	return tx.Commit()
}
func (s *Store) snapshot(now time.Time) (Snapshot, error) {
	out := Snapshot{Accounts: []Account{}, Devices: []Device{}, Nodes: []Node{}}
	rows, err := s.db.Query("SELECT a.id,a.name,a.enabled,a.max_devices,a.epoch,(SELECT count(*) FROM devices d WHERE d.account_id=a.id AND d.revoked=0) FROM accounts a ORDER BY a.created_at")
	if err != nil {
		return out, err
	}
	for rows.Next() {
		var a Account
		if err = rows.Scan(&a.ID, &a.Name, &a.Enabled, &a.MaxDevices, &a.Epoch, &a.Devices); err != nil {
			rows.Close()
			return out, err
		}
		out.Accounts = append(out.Accounts, a)
	}
	rows.Close()
	rows, err = s.db.Query("SELECT d.id,d.name,d.account_id,d.revoked,d.epoch!=a.epoch,d.public_key FROM devices d JOIN accounts a ON d.account_id=a.id ORDER BY d.created_at")
	if err != nil {
		return out, err
	}
	for rows.Next() {
		var d Device
		var key string
		if err = rows.Scan(&d.ID, &d.Name, &d.AccountID, &d.Revoked, &d.NeedsAuthorization, &key); err != nil {
			rows.Close()
			return out, err
		}
		p, _, _, _, _ := ssh.ParseAuthorizedKey([]byte(key))
		if p != nil {
			d.Fingerprint = ssh.FingerprintSHA256(p)
		}
		out.Devices = append(out.Devices, d)
	}
	rows.Close()
	rows, err = s.db.Query("SELECT id,name,host,port,status,identity_key IS NOT NULL,last_report,healthy,sessions,upload_bps,download_bps,capacity_bps,high_since,warn_seconds,warn_percent FROM nodes ORDER BY id")
	if err != nil {
		return out, err
	}
	online := map[string]bool{}
	for rows.Next() {
		var n Node
		var healthy bool
		var sessions string
		var up, down float64
		var high int64
		var seconds int
		if err = rows.Scan(&n.ID, &n.Name, &n.Host, &n.Port, &n.Status, &n.Registered, &n.LastReport, &healthy, &sessions, &up, &down, &n.CapacityBPS, &high, &seconds, &n.WarnPercent); err != nil {
			rows.Close()
			return out, err
		}
		n.WarnSeconds = seconds
		n.Days = []DayTraffic{}
		n.Fresh = n.LastReport > 0 && now.Unix()-n.LastReport <= 30
		n.HighLoad = n.Fresh && high > 0 && now.Unix()-high >= int64(seconds)
		if n.Fresh {
			ids := []string{}
			_ = json.Unmarshal([]byte(sessions), &ids)
			n.Healthy = &healthy
			n.Sessions = &ids
			n.UploadBPS = &up
			n.DownloadBPS = &down
			for _, id := range ids {
				online[id] = true
			}
			if n.CapacityBPS > 0 {
				v := max(up, down) * 100 / float64(n.CapacityBPS)
				n.Utilization = &v
			}
		} else {
			out.UnknownNodes++
		}
		out.Nodes = append(out.Nodes, n)
	}
	rows.Close()
	for i := range out.Nodes {
		r, e := s.db.Query("SELECT day,up,down FROM traffic WHERE node_id=? ORDER BY day DESC LIMIT 8", out.Nodes[i].ID)
		if e != nil {
			return out, e
		}
		for r.Next() {
			var d DayTraffic
			if e = r.Scan(&d.Day, &d.Up, &d.Down); e != nil {
				r.Close()
				return out, e
			}
			out.Nodes[i].Days = append(out.Nodes[i].Days, d)
		}
		r.Close()
	}
	out.ConfirmedOnline = len(online)
	return out, nil
}
