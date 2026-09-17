package main

import (
	"os"
	"path/filepath"
	"testing"
)

func TestSchemaTwoFreshAndUnsupportedVersionPreserved(t *testing.T) {
	dir := t.TempDir()
	key := filepath.Join(dir, "master.key")
	if err := os.WriteFile(key, make([]byte, 32), 0600); err != nil {
		t.Fatal(err)
	}
	s, err := openStore(dir, key)
	if err != nil {
		t.Fatal(err)
	}
	var version int
	if err = s.db.QueryRow("PRAGMA user_version").Scan(&version); err != nil || version != 2 {
		t.Fatal(version, err)
	}
	account, _ := mustAccount(t, s)
	if _, err = s.db.Exec("PRAGMA user_version=1"); err != nil {
		t.Fatal(err)
	}
	s.db.Close()
	before, err := os.ReadFile(filepath.Join(dir, "control.sqlite"))
	if err != nil {
		t.Fatal(err)
	}
	if unsupported, err := openStore(dir, key); err == nil {
		unsupported.db.Close()
		t.Fatal("schema 1 accepted")
	}
	after, err := os.ReadFile(filepath.Join(dir, "control.sqlite"))
	if err != nil {
		t.Fatal(err)
	}
	if string(before) != string(after) {
		t.Fatal("unsupported database changed", account)
	}
}
