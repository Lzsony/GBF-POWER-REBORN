package main

import (
	"bytes"
	"crypto/ed25519"
	"encoding/json"
	"net/http/httptest"
	"testing"
	"time"
)

func TestCanonicalResponsesAndSignedGateway(t *testing.T) {
	s := testStore(t)
	token, err := s.newNode("node-a", "Node A", "example.com", 2222, 100)
	if err != nil {
		t.Fatal(err)
	}
	gateway := keypair(t)
	device := keypair(t)
	gatewayPub := publicText(gateway.Public().(ed25519.PublicKey))
	devicePub := publicText(device.Public().(ed25519.PublicKey))
	if err = s.join("node-a", token, gatewayPub, publicText(keypair(t).Public().(ed25519.PublicKey))); err != nil {
		t.Fatal(err)
	}
	account, code := mustAccount(t, s)
	if err = s.grant(account, "node-a", true, false); err != nil {
		t.Fatal(err)
	}
	if err = s.report("node-a", NodeReport{Healthy: true, Sessions: []string{}}, time.Now()); err != nil {
		t.Fatal(err)
	}
	h := newControl(s)
	request := func(path string, body any, key ed25519.PrivateKey) *httptest.ResponseRecorder {
		data, _ := json.Marshal(sign(path, body, key))
		r := httptest.NewRequest("POST", path, bytes.NewReader(data))
		w := httptest.NewRecorder()
		h.ServeHTTP(w, r)
		return w
	}
	for _, path := range []string{"/v1/activate", "/v1/status"} {
		var body any = struct{}{}
		if path == "/v1/activate" {
			body = activation{code, "device"}
		}
		w := request(path, body, device)
		if w.Code != 200 {
			t.Fatal(path, w.Code, w.Body.String())
		}
		var status AuthStatus
		if err = json.Unmarshal(w.Body.Bytes(), &status); err != nil {
			t.Fatal(err)
		}
		expected := "node-a"
		if len(status.Lines) != 1 || status.Lines[0].ID != expected {
			t.Fatal("incorrect ID presentation", status.Lines)
		}
	}
	for _, id := range []string{"node-a"} {
		w := request("/internal/v1/leases", leaseRequest{id, []string{devicePub}}, gateway)
		var leases leaseReply
		json.Unmarshal(w.Body.Bytes(), &leases)
		if w.Code != 200 || len(leases.Leases) != 1 || !leases.Leases[0].Active {
			t.Fatal("gateway lease rejected", w.Code)
		}
		w = request("/internal/v1/report", NodeReport{NodeID: id, Healthy: true, Sessions: []string{}}, gateway)
		if w.Code != 200 {
			t.Fatal("gateway report rejected", w.Code)
		}
	}
	if request("/internal/v1/leases", leaseRequest{"node-b", nil}, gateway).Code == 200 {
		t.Fatal("wrong node identity accepted")
	}
	if request("/internal/v1/leases", leaseRequest{"node-a", nil}, device).Code == 200 {
		t.Fatal("device impersonated gateway")
	}
	disabled := false
	if err = s.setAccount(account, &disabled, 0); err != nil {
		t.Fatal(err)
	}
	w := request("/internal/v1/leases", leaseRequest{"node-a", []string{devicePub}}, gateway)
	var leases leaseReply
	json.Unmarshal(w.Body.Bytes(), &leases)
	if w.Code != 200 || len(leases.Leases) != 1 || leases.Leases[0].Active {
		t.Fatal("revocation failed")
	}
}
