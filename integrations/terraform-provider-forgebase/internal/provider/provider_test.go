package provider_test

import (
	"net/http"
	"strings"
	"testing"
)

func TestProviderConfigure(t *testing.T) {
	m := newMock(t)
	m.on("GET /api/v1/collections/{c}", func(*http.Request, map[string]any) (int, any) { return 404, apiErr("not_found", "no") })

	t.Run("explicit values", func(t *testing.T) {
		k := newKit(t, nil)
		if d := k.configure(map[string]any{"url": m.srv.URL, "token": "abc", "realm": "r", "org": "o", "tenant": "t"}); errText(d) != "" {
			t.Fatal(errText(d))
		}
		st := tftypesNull(k, "forgebase_collection")
		_, _ = k.read("forgebase_collection", st) // no state: nothing to call
		_, d := k.dataSource("forgebase_collection", map[string]any{"name": "x"})
		if !strings.Contains(errText(d), "not_found") {
			t.Errorf("request did not reach the mock: %q", errText(d))
		}
		r := m.last("GET", "/api/v1/collections/x")
		if r.Auth != "Bearer abc" {
			t.Errorf("auth = %q", r.Auth)
		}
	})

	t.Run("environment fallback", func(t *testing.T) {
		t.Setenv("FORGEBASE_URL", m.srv.URL)
		t.Setenv("FORGEBASE_TOKEN", "from-env")
		m.reset()
		k := newKit(t, nil)
		if d := k.configure(map[string]any{}); errText(d) != "" {
			t.Fatal(errText(d))
		}
		_, _ = k.dataSource("forgebase_collection", map[string]any{"name": "y"})
		if got := m.last("GET", "/api/v1/collections/y").Auth; got != "Bearer from-env" {
			t.Errorf("auth = %q", got)
		}
	})

	t.Run("missing url", func(t *testing.T) {
		t.Setenv("FORGEBASE_URL", "")
		k := newKit(t, nil)
		d := k.configure(map[string]any{"token": "x"})
		if !strings.Contains(errText(d), "Missing ForgeBase URL") || !strings.Contains(errText(d), "FORGEBASE_URL") {
			t.Errorf("diagnostics: %q", errText(d))
		}
	})

	t.Run("invalid url", func(t *testing.T) {
		k := newKit(t, nil)
		d := k.configure(map[string]any{"url": "not a url", "token": "x"})
		if !strings.Contains(errText(d), "Invalid provider configuration") {
			t.Errorf("diagnostics: %q", errText(d))
		}
	})

	t.Run("missing token warns", func(t *testing.T) {
		t.Setenv("FORGEBASE_TOKEN", "")
		k := newKit(t, nil)
		d := k.configure(map[string]any{"url": m.srv.URL})
		if errText(d) != "" || len(d) == 0 || !strings.Contains(d[0].Summary, "No ForgeBase credential") {
			t.Errorf("want one warning, got %v", d)
		}
	})
}
