package client_test

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/farzanshibu/forgebase/integrations/terraform-provider-forgebase/internal/client"
)

func newClient(t *testing.T, h http.HandlerFunc, cfg client.Config) *client.Client {
	t.Helper()
	srv := httptest.NewServer(h)
	t.Cleanup(srv.Close)
	cfg.URL = srv.URL
	c, err := client.New(cfg)
	if err != nil {
		t.Fatal(err)
	}
	return c
}

func TestNewValidatesURL(t *testing.T) {
	for _, u := range []string{"", "  ", "forgebase.example.com", "ftp://x", "http://"} {
		if _, err := client.New(client.Config{URL: u}); err == nil {
			t.Errorf("URL %q accepted", u)
		}
	}
	if _, err := client.New(client.Config{URL: "https://forgebase.example.com/base/"}); err != nil {
		t.Error(err)
	}
}

func TestHeadersAndBody(t *testing.T) {
	var got http.Header
	var body map[string]any
	var path, rawPath string
	c := newClient(t, func(w http.ResponseWriter, r *http.Request) {
		got, path, rawPath = r.Header.Clone(), r.URL.Path, r.URL.EscapedPath()
		_ = json.NewDecoder(r.Body).Decode(&body)
		w.WriteHeader(http.StatusCreated)
		_, _ = w.Write([]byte(`{"ok":true}`))
	}, client.Config{Token: "tok", Realm: "r1", Org: "o1", Tenant: "t1", UserAgent: "tf/1"})
	var out struct{ OK bool }
	if err := c.Post(context.Background(), "/api/v1/things/"+client.Seg("a b/c"), map[string]any{"x": 1}, &out); err != nil {
		t.Fatal(err)
	}
	if !out.OK || body["x"] != float64(1) {
		t.Errorf("out=%v body=%v", out, body)
	}
	for k, want := range map[string]string{
		"Authorization": "Bearer tok", "X-Forgebase-Realm": "r1", "X-Forgebase-Org": "o1", "X-Forgebase-Tenant": "t1",
		"User-Agent": "tf/1", "Content-Type": "application/json", "Accept": "application/json",
	} {
		if got.Get(k) != want {
			t.Errorf("header %s = %q, want %q", k, got.Get(k), want)
		}
	}
	if path != "/api/v1/things/a b/c" || rawPath != "/api/v1/things/a%20b%2Fc" {
		t.Errorf("path = %q raw %q (segments must be escaped)", path, rawPath)
	}
}

func TestNoCredentialsNoAuthHeader(t *testing.T) {
	c := newClient(t, func(w http.ResponseWriter, r *http.Request) {
		if r.Header.Get("Authorization") != "" || r.Header.Get("X-Forgebase-Realm") != "" {
			t.Errorf("unexpected headers: %v", r.Header)
		}
		w.WriteHeader(http.StatusNoContent)
	}, client.Config{})
	if err := c.Delete(context.Background(), "/x", client.Query{"force": "true"}); err != nil {
		t.Fatal(err)
	}
}

func TestErrorEnvelope(t *testing.T) {
	c := newClient(t, func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusConflict)
		_, _ = w.Write([]byte(`{"error":{"code":"already_exists","message":"collection posts exists","request_id":"rq-9","details":{"field":"name"}}}`))
	}, client.Config{})
	err := c.Get(context.Background(), "/api/v1/collections", nil, nil)
	var ae *client.APIError
	if !asAPIError(err, &ae) {
		t.Fatalf("not an APIError: %v", err)
	}
	if ae.Status != 409 || ae.Code != "already_exists" || ae.RequestID != "rq-9" || !client.IsConflict(err) || client.IsNotFound(err) {
		t.Errorf("error = %+v", ae)
	}
	msg := err.Error()
	for _, want := range []string{"GET /api/v1/collections", "HTTP 409", "already_exists", "collection posts exists", "rq-9", `"field":"name"`} {
		if !strings.Contains(msg, want) {
			t.Errorf("message %q lacks %q", msg, want)
		}
	}
	if client.CodeOf(err) != "already_exists" || client.StatusOf(err) != 409 {
		t.Error("CodeOf/StatusOf")
	}
}

func asAPIError(err error, target **client.APIError) bool {
	ae, ok := err.(*client.APIError)
	if ok {
		*target = ae
	}
	return ok
}

func TestNonJSONErrorBody(t *testing.T) {
	c := newClient(t, func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusNotFound)
		_, _ = w.Write([]byte("404 page not found\n"))
	}, client.Config{})
	err := c.Get(context.Background(), "/missing", nil, nil)
	if !client.IsNotFound(err) || !strings.Contains(err.Error(), "404 page not found") {
		t.Errorf("err = %v", err)
	}
	long := newClient(t, func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusBadGateway)
		_, _ = w.Write([]byte(strings.Repeat("x", 5000)))
	}, client.Config{MaxRetries: 1})
	if e := long.Post(context.Background(), "/x", nil, nil); e == nil || len(e.Error()) > 600 {
		t.Errorf("long body must be truncated: %d", len(e.Error()))
	}
}

func TestRetries(t *testing.T) {
	var calls atomic.Int32
	c := newClient(t, func(w http.ResponseWriter, r *http.Request) {
		if calls.Add(1) < 3 {
			w.Header().Set("Retry-After", "0")
			w.WriteHeader(http.StatusTooManyRequests)
			_, _ = w.Write([]byte(`{"error":{"code":"rate_limited","message":"slow down","request_id":"r"}}`))
			return
		}
		_, _ = w.Write([]byte(`{"ok":true}`))
	}, client.Config{})
	var out map[string]any
	if err := c.Post(context.Background(), "/x", map[string]any{}, &out); err != nil {
		t.Fatalf("429 must be retried (even POST): %v", err)
	}
	if calls.Load() != 3 {
		t.Errorf("calls = %d", calls.Load())
	}

	// 503 is retried for GET only.
	var gets, posts atomic.Int32
	c2 := newClient(t, func(w http.ResponseWriter, r *http.Request) {
		if r.Method == http.MethodGet {
			if gets.Add(1) < 2 {
				w.WriteHeader(http.StatusServiceUnavailable)
				return
			}
			_, _ = w.Write([]byte(`{}`))
			return
		}
		posts.Add(1)
		w.WriteHeader(http.StatusServiceUnavailable)
	}, client.Config{})
	if err := c2.Get(context.Background(), "/x", nil, nil); err != nil || gets.Load() != 2 {
		t.Errorf("GET retry: err=%v calls=%d", err, gets.Load())
	}
	if err := c2.Post(context.Background(), "/x", map[string]any{}, nil); err == nil || posts.Load() != 1 {
		t.Errorf("POST must not be retried on 503: err=%v calls=%d", err, posts.Load())
	}

	// Gives up after the retry budget.
	var n atomic.Int32
	c3 := newClient(t, func(w http.ResponseWriter, r *http.Request) {
		n.Add(1)
		w.Header().Set("Retry-After", "0")
		w.WriteHeader(http.StatusTooManyRequests)
	}, client.Config{MaxRetries: 2})
	err := c3.Get(context.Background(), "/x", nil, nil)
	if client.StatusOf(err) != 429 || n.Load() != 3 {
		t.Errorf("err=%v calls=%d", err, n.Load())
	}
}

func TestContextCancelStopsRetries(t *testing.T) {
	c := newClient(t, func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Retry-After", "5")
		w.WriteHeader(http.StatusTooManyRequests)
	}, client.Config{})
	ctx, cancel := context.WithTimeout(context.Background(), 100*time.Millisecond)
	defer cancel()
	start := time.Now()
	if err := c.Get(ctx, "/x", nil, nil); err == nil {
		t.Fatal("expected an error")
	}
	if time.Since(start) > 2*time.Second {
		t.Errorf("cancellation not honored: %v", time.Since(start))
	}
}

func TestDecodeErrorAndEmptyBody(t *testing.T) {
	c := newClient(t, func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path == "/bad" {
			_, _ = w.Write([]byte(`not json`))
			return
		}
		w.WriteHeader(http.StatusOK)
	}, client.Config{})
	var out map[string]any
	if err := c.Get(context.Background(), "/bad", nil, &out); err == nil || !strings.Contains(err.Error(), "decode response") {
		t.Errorf("err = %v", err)
	}
	if err := c.Get(context.Background(), "/empty", nil, &out); err != nil {
		t.Errorf("an empty 200 body is fine: %v", err)
	}
}

func TestConnectionError(t *testing.T) {
	c, _ := client.New(client.Config{URL: "http://127.0.0.1:1", MaxRetries: 1})
	err := c.Get(context.Background(), "/x", nil, nil)
	if err == nil || client.StatusOf(err) != 0 || !strings.Contains(err.Error(), "GET /x") {
		t.Errorf("err = %v", err)
	}
}
