package main

import (
	"bytes"
	"crypto/ed25519"
	"encoding/json"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
)

func availableNode(t *testing.T, s *Store, id string, port int) {
	t.Helper()
	token, err := s.newNode(id, id, "example.com", port, 0)
	if err != nil {
		t.Fatal(err)
	}
	identity := keypair(t)
	if err = s.join(id, token, publicText(identity.Public().(ed25519.PublicKey)), publicText(keypair(t).Public().(ed25519.PublicKey))); err != nil {
		t.Fatal(err)
	}
	if err = s.report(id, NodeReport{Healthy: true, Sessions: []string{}}, time.Now()); err != nil {
		t.Fatal(err)
	}
	if err = s.setNode(id, "enabled", 0, 70, 300); err != nil {
		t.Fatal(err)
	}
}

func TestAccountCreationUsesOnlyExplicitAvailableNodes(t *testing.T) {
	s := testStore(t)
	availableNode(t, s, "node-a", 2201)
	availableNode(t, s, "node-b", 2202)
	id, _, err := s.createAccount("without routes", 2)
	if err != nil {
		t.Fatal(err)
	}
	var count int
	if err = s.db.QueryRow("SELECT count(*) FROM grants WHERE account_id=?", id).Scan(&count); err != nil || count != 0 {
		t.Fatal("zero selection created grants", count, err)
	}
	selected, _, err := s.createAccount("one route", 2, "node-b")
	if err != nil {
		t.Fatal(err)
	}
	var node string
	var preview bool
	if err = s.db.QueryRow("SELECT node_id,preview FROM grants WHERE account_id=?", selected).Scan(&node, &preview); err != nil || node != "node-b" || preview {
		t.Fatal("selection not preserved", node, preview, err)
	}
	if err = s.db.QueryRow("SELECT count(*) FROM grants WHERE account_id=?", selected).Scan(&count); err != nil || count != 1 {
		t.Fatal(count, err)
	}
}

func TestAccountAndAllInitialGrantsAreOneTransaction(t *testing.T) {
	s := testStore(t)
	availableNode(t, s, "node-a", 2201)
	availableNode(t, s, "node-b", 2202)
	_, err := s.db.Exec("CREATE TRIGGER reject_second_grant BEFORE INSERT ON grants WHEN NEW.node_id='node-b' BEGIN SELECT RAISE(ABORT,'test failure'); END")
	if err != nil {
		t.Fatal(err)
	}
	if id, code, err := s.createAccount("must roll back", 2, "node-a", "node-b"); err == nil || id != "" || code != "" {
		t.Fatal("partial account success", id, err)
	}
	for _, table := range []string{"accounts", "grants"} {
		var count int
		if err = s.db.QueryRow("SELECT count(*) FROM " + table).Scan(&count); err != nil || count != 0 {
			t.Fatal(table, count, err)
		}
	}
}

func TestInitialGrantRejectsPendingDisabledMissingUnregisteredAndDuplicates(t *testing.T) {
	s := testStore(t)
	availableNode(t, s, "enabled", 2201)
	availableNode(t, s, "disabled", 2202)
	if err := s.setNode("disabled", "disabled", 0, 70, 300); err != nil {
		t.Fatal(err)
	}
	if _, err := s.newNode("pending", "pending", "example.com", 2203, 0); err != nil {
		t.Fatal(err)
	}
	if _, err := s.newNode("unregistered", "unregistered", "example.com", 2204, 0); err != nil {
		t.Fatal(err)
	}
	if _, err := s.db.Exec("UPDATE nodes SET status='enabled' WHERE id='unregistered'"); err != nil {
		t.Fatal(err)
	}
	for _, nodes := range [][]string{{"pending"}, {"disabled"}, {"missing"}, {"unregistered"}, {"enabled", "enabled"}, {"enabled", "pending"}} {
		if _, _, err := s.createAccount("reject", 2, nodes...); err == nil {
			t.Fatal("unavailable initial grant accepted", nodes)
		}
	}
	var count int
	if err := s.db.QueryRow("SELECT count(*) FROM accounts").Scan(&count); err != nil || count != 0 {
		t.Fatal("failed validation persisted account", count, err)
	}
	id, _, err := s.createAccount("preview granted separately", 2)
	if err != nil {
		t.Fatal(err)
	}
	if err = s.grant(id, "pending", true, false); err != nil {
		t.Fatal("explicit preview grant rejected", err)
	}
}

func TestAdminAccountCreateAcceptsExplicitNodeSelection(t *testing.T) {
	s := testStore(t)
	availableNode(t, s, "node-a", 2201)
	request := httptest.NewRequest("POST", "/admin", strings.NewReader(`{"action":"account-create","name":"chosen","maxDevices":2,"nodeIds":["node-a"]}`))
	recorder := httptest.NewRecorder()
	adminHandler(s, ControlConfig{}).ServeHTTP(recorder, request)
	if recorder.Code != 200 {
		t.Fatal(recorder.Code, recorder.Body.String())
	}
	var response struct {
		ID   string `json:"id"`
		Code string `json:"code"`
	}
	if err := json.Unmarshal(recorder.Body.Bytes(), &response); err != nil || response.ID == "" || response.Code == "" {
		t.Fatal(err)
	}
	var node string
	if err := s.db.QueryRow("SELECT node_id FROM grants WHERE account_id=?", response.ID).Scan(&node); err != nil || node != "node-a" {
		t.Fatal(node, err)
	}
}

func TestTUIAccountCreationHasAnExplicitEmptyChecklist(t *testing.T) {
	m := tui{width: 100, height: 30, data: Snapshot{Nodes: []Node{
		{ID: "node-a", Name: "A", Status: "enabled", Registered: true},
		{ID: "node-b", Name: "B", Status: "enabled", Registered: true},
		{ID: "pending", Name: "Pending", Status: "pending", Registered: true},
		{ID: "disabled", Name: "Disabled", Status: "disabled", Registered: true},
		{ID: "unregistered", Name: "Unregistered", Status: "enabled"},
	}}}
	model, _ := m.Update(tea.KeyPressMsg{Code: 'n', Text: "n"})
	next := model.(tui)
	if next.form == nil || len(next.form.nodes) != 2 {
		t.Fatal("available node choices missing")
	}
	next.form.fields[0].value = "operator choice"
	q, err := formRequest(*next.form)
	if err != nil || q.NodeIDs == nil || len(q.NodeIDs) != 0 {
		t.Fatal("nodes implicitly selected", q, err)
	}
	if !bytes.Contains([]byte(next.View().Content), []byte("已選 0／2")) {
		t.Fatal("zero-grant choice not visible")
	}
	next.form.focus = 3
	model, _ = next.Update(tea.KeyPressMsg{Code: ' ', Text: " "})
	next = model.(tui)
	q, err = formRequest(*next.form)
	if err != nil || len(q.NodeIDs) != 1 || q.NodeIDs[0] != "node-b" {
		t.Fatal("keyboard choice lost", q, err)
	}
	model, _ = next.Update(tea.KeyPressMsg{Code: ' ', Text: " "})
	next = model.(tui)
	q, err = formRequest(*next.form)
	if err != nil || len(q.NodeIDs) != 0 {
		t.Fatal("cannot deselect", q, err)
	}
}
