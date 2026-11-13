// Package client is a small HTTP client for the ForgeBase REST API.
//
// It speaks JSON, authenticates with a bearer token (API key, personal access
// token or access token) and maps the server's error envelope
// (`{"error":{"code","message","request_id","details"}}`) to [APIError].
package client

import (
	"bytes"
	"context"
	"crypto/tls"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strconv"
	"strings"
	"time"
)

// Config configures a [Client].
type Config struct {
	// URL is the server base URL, e.g. https://forgebase.example.com.
	URL string
	// Token is sent as `Authorization: Bearer <token>`.
	Token string
	// Realm, Org and Tenant are sent as the X-Forgebase-* tenancy headers when set.
	Realm  string
	Org    string
	Tenant string
	// Insecure skips TLS certificate verification.
	Insecure bool
	// UserAgent overrides the default User-Agent.
	UserAgent string
	// HTTPClient replaces the default HTTP client (tests).
	HTTPClient *http.Client
	// MaxRetries is how many times a throttled (429) or, for GET, briefly
	// unavailable (502/503) request is retried. Default 3.
	MaxRetries int
}

// Client calls the ForgeBase API.
type Client struct {
	base       *url.URL
	cfg        Config
	http       *http.Client
	maxRetries int
}

// New validates cfg and returns a client.
func New(cfg Config) (*Client, error) {
	raw := strings.TrimSpace(cfg.URL)
	if raw == "" {
		return nil, errors.New("the ForgeBase URL is empty")
	}
	u, err := url.Parse(raw)
	if err != nil || (u.Scheme != "http" && u.Scheme != "https") || u.Host == "" {
		return nil, fmt.Errorf("invalid ForgeBase URL %q: want http(s)://host[:port]", raw)
	}
	u.Path = strings.TrimRight(u.Path, "/")
	hc := cfg.HTTPClient
	if hc == nil {
		tr := http.DefaultTransport.(*http.Transport).Clone()
		if cfg.Insecure {
			tr.TLSClientConfig = &tls.Config{InsecureSkipVerify: true} //nolint:gosec // opt-in
		}
		hc = &http.Client{Timeout: 60 * time.Second, Transport: tr}
	}
	retries := cfg.MaxRetries
	if retries <= 0 {
		retries = 3
	}
	return &Client{base: u, cfg: cfg, http: hc, maxRetries: retries}, nil
}

// APIError is an error response from the server.
type APIError struct {
	Status    int
	Code      string
	Message   string
	RequestID string
	Details   json.RawMessage
	Method    string
	Path      string
}

// Error implements error.
func (e *APIError) Error() string {
	var b strings.Builder
	fmt.Fprintf(&b, "%s %s: HTTP %d", e.Method, e.Path, e.Status)
	if e.Code != "" {
		fmt.Fprintf(&b, " %s", e.Code)
	}
	if e.Message != "" {
		fmt.Fprintf(&b, ": %s", e.Message)
	}
	if len(e.Details) > 0 && string(e.Details) != "null" {
		fmt.Fprintf(&b, " (details: %s)", e.Details)
	}
	if e.RequestID != "" {
		fmt.Fprintf(&b, " [request_id %s]", e.RequestID)
	}
	return b.String()
}

// IsNotFound reports whether err is a 404 from the server.
func IsNotFound(err error) bool { return StatusOf(err) == http.StatusNotFound }

// IsConflict reports whether err is a 409 from the server.
func IsConflict(err error) bool { return StatusOf(err) == http.StatusConflict }

// StatusOf returns the HTTP status of an [APIError], or 0.
func StatusOf(err error) int {
	var ae *APIError
	if errors.As(err, &ae) {
		return ae.Status
	}
	return 0
}

// CodeOf returns the error code of an [APIError], or "".
func CodeOf(err error) string {
	var ae *APIError
	if errors.As(err, &ae) {
		return ae.Code
	}
	return ""
}

// Query is a set of URL query parameters.
type Query map[string]string

// Do sends a request. body (if not nil) is encoded as JSON; a 2xx response
// body is decoded into out (if not nil). A 204 leaves out untouched.
func (c *Client) Do(ctx context.Context, method, path string, q Query, body, out any) error {
	var payload []byte
	if body != nil {
		var err error
		if payload, err = json.Marshal(body); err != nil {
			return fmt.Errorf("encode request body: %w", err)
		}
	}
	u := *c.base
	u.Path = c.base.Path + path
	u.RawPath = ""
	if len(q) > 0 {
		vals := url.Values{}
		for k, v := range q {
			vals.Set(k, v)
		}
		u.RawQuery = vals.Encode()
	}
	for attempt := 0; ; attempt++ {
		req, err := http.NewRequestWithContext(ctx, method, u.String(), bytes.NewReader(payload))
		if err != nil {
			return err
		}
		c.headers(req, body != nil)
		resp, err := c.http.Do(req)
		if err != nil {
			if method == http.MethodGet && attempt < c.maxRetries && ctx.Err() == nil {
				sleep(ctx, backoff(attempt, ""))
				continue
			}
			return fmt.Errorf("%s %s: %w", method, path, err)
		}
		data, rerr := io.ReadAll(io.LimitReader(resp.Body, 64<<20))
		_ = resp.Body.Close()
		if rerr != nil {
			return fmt.Errorf("%s %s: read response: %w", method, path, rerr)
		}
		if resp.StatusCode >= 200 && resp.StatusCode < 300 {
			if out != nil && len(bytes.TrimSpace(data)) > 0 {
				if err := json.Unmarshal(data, out); err != nil {
					return fmt.Errorf("%s %s: decode response: %w", method, path, err)
				}
			}
			return nil
		}
		retryable := resp.StatusCode == http.StatusTooManyRequests ||
			(method == http.MethodGet && (resp.StatusCode == http.StatusBadGateway || resp.StatusCode == http.StatusServiceUnavailable))
		if retryable && attempt < c.maxRetries && ctx.Err() == nil {
			sleep(ctx, backoff(attempt, resp.Header.Get("Retry-After")))
			continue
		}
		return parseError(method, path, resp.StatusCode, data)
	}
}

// Get decodes the response of GET path into out.
func (c *Client) Get(ctx context.Context, path string, q Query, out any) error {
	return c.Do(ctx, http.MethodGet, path, q, nil, out)
}

// Post sends body with POST.
func (c *Client) Post(ctx context.Context, path string, body, out any) error {
	return c.Do(ctx, http.MethodPost, path, nil, body, out)
}

// Put sends body with PUT.
func (c *Client) Put(ctx context.Context, path string, body, out any) error {
	return c.Do(ctx, http.MethodPut, path, nil, body, out)
}

// Patch sends body with PATCH.
func (c *Client) Patch(ctx context.Context, path string, body, out any) error {
	return c.Do(ctx, http.MethodPatch, path, nil, body, out)
}

// Delete sends DELETE.
func (c *Client) Delete(ctx context.Context, path string, q Query) error {
	return c.Do(ctx, http.MethodDelete, path, q, nil, nil)
}

// Seg escapes one path segment (names and ids may hold `/`, `?` or spaces).
func Seg(s string) string { return url.PathEscape(s) }

func (c *Client) headers(req *http.Request, hasBody bool) {
	req.Header.Set("Accept", "application/json")
	ua := c.cfg.UserAgent
	if ua == "" {
		ua = "terraform-provider-forgebase"
	}
	req.Header.Set("User-Agent", ua)
	if hasBody {
		req.Header.Set("Content-Type", "application/json")
	}
	if c.cfg.Token != "" {
		req.Header.Set("Authorization", "Bearer "+c.cfg.Token)
	}
	if c.cfg.Realm != "" {
		req.Header.Set("X-Forgebase-Realm", c.cfg.Realm)
	}
	if c.cfg.Org != "" {
		req.Header.Set("X-Forgebase-Org", c.cfg.Org)
	}
	if c.cfg.Tenant != "" {
		req.Header.Set("X-Forgebase-Tenant", c.cfg.Tenant)
	}
}

func parseError(method, path string, status int, data []byte) error {
	e := &APIError{Status: status, Method: method, Path: path}
	var env struct {
		Error struct {
			Code      string          `json:"code"`
			Message   string          `json:"message"`
			RequestID string          `json:"request_id"`
			Details   json.RawMessage `json:"details"`
		} `json:"error"`
	}
	if err := json.Unmarshal(data, &env); err == nil && (env.Error.Code != "" || env.Error.Message != "") {
		e.Code, e.Message, e.RequestID, e.Details = env.Error.Code, env.Error.Message, env.Error.RequestID, env.Error.Details
		return e
	}
	e.Message = strings.TrimSpace(string(data))
	if len(e.Message) > 300 {
		e.Message = e.Message[:300] + "…"
	}
	return e
}

func backoff(attempt int, retryAfter string) time.Duration {
	if s, err := strconv.Atoi(strings.TrimSpace(retryAfter)); err == nil && s >= 0 {
		if s > 10 {
			s = 10
		}
		return time.Duration(s) * time.Second
	}
	return time.Duration(200*(1<<attempt)) * time.Millisecond
}

func sleep(ctx context.Context, d time.Duration) {
	t := time.NewTimer(d)
	defer t.Stop()
	select {
	case <-ctx.Done():
	case <-t.C:
	}
}
