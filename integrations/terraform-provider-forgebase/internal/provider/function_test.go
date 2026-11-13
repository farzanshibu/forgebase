package provider_test

import (
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
)

type fakeFunctions struct {
	mu       sync.Mutex
	fn       map[string]map[string]any   // name -> info
	versions map[string][]map[string]any // name -> versions (oldest first)
	sources  map[string]map[int]string
	secrets  map[string]map[string]string
	kv       map[string][]any
}

func sum(b []byte) string { s := sha256.Sum256(b); return hex.EncodeToString(s[:]) }

func newFakeFunctions(m *mockServer) *fakeFunctions {
	f := &fakeFunctions{fn: map[string]map[string]any{}, versions: map[string][]map[string]any{}, sources: map[string]map[int]string{},
		secrets: map[string]map[string]string{}, kv: map[string][]any{}}
	notFound := func() (int, any) { return 404, apiErr("function_not_found", "no such function") }
	m.on("POST /api/v1/functions", func(_ *http.Request, b map[string]any) (int, any) {
		f.mu.Lock()
		defer f.mu.Unlock()
		name := b["name"].(string)
		info, exists := f.fn[name]
		if !exists {
			info = map[string]any{"id": "fn_" + name, "name": name, "description": "", "access": "permission", "principal": "service:fn_" + name,
				"created_at": "2026-01-01T00:00:00Z", "created_by": "admin:1", "latest_version": float64(0), "active_version": float64(0)}
			f.fn[name] = info
			f.sources[name] = map[int]string{}
		}
		if d, ok := b["description"].(string); ok {
			info["description"] = d
		}
		if a, ok := b["access"].(string); ok {
			info["access"] = a
		}
		v := int(info["latest_version"].(float64)) + 1
		info["latest_version"], info["active_version"], info["updated_at"] = float64(v), float64(v), fmt.Sprintf("2026-01-0%dT00:00:00Z", v)
		var hash string
		lang := "typescript"
		if src, ok := b["source"].(string); ok {
			hash = sum([]byte(src))
			f.sources[name][v] = src
			if l, ok := b["language"].(string); ok {
				lang = l
			}
		} else {
			raw, _ := base64.StdEncoding.DecodeString(b["wasm"].(string))
			hash = sum(raw)
			lang = "wasm"
		}
		lim := map[string]any{"fuel": float64(100000000), "memory_bytes": float64(16777216), "timeout_ms": float64(5000), "max_concurrency": float64(8)}
		if l, ok := b["limits"].(map[string]any); ok {
			for k, x := range l {
				lim[k] = x
			}
		}
		ver := map[string]any{"version": float64(v), "sha256": hash, "active": true, "language": lang, "capabilities": orDefault(b["capabilities"], []any{}),
			"fetch_hosts": orDefault(b["fetch_hosts"], []any{}), "env": orDefault(b["env"], map[string]any{}), "hooks": orDefault(b["hooks"], []any{}), "limits": lim}
		for _, old := range f.versions[name] {
			old["active"] = false
		}
		f.versions[name] = append(f.versions[name], ver)
		return 201, map[string]any{"function": info, "version": ver}
	})
	m.on("GET /api/v1/functions/{n}", func(r *http.Request, _ map[string]any) (int, any) {
		f.mu.Lock()
		defer f.mu.Unlock()
		if i, ok := f.fn[r.PathValue("n")]; ok {
			return 200, i
		}
		return notFound()
	})
	m.on("GET /api/v1/functions/{n}/versions", func(r *http.Request, _ map[string]any) (int, any) {
		f.mu.Lock()
		defer f.mu.Unlock()
		vs := f.versions[r.PathValue("n")]
		out := make([]any, 0, len(vs))
		for i := len(vs) - 1; i >= 0; i-- {
			out = append(out, vs[i])
		}
		return 200, map[string]any{"items": out}
	})
	m.on("GET /api/v1/functions/{n}/versions/{v}/source", func(r *http.Request, _ map[string]any) (int, any) {
		f.mu.Lock()
		defer f.mu.Unlock()
		var v int
		fmt.Sscanf(r.PathValue("v"), "%d", &v)
		return 200, map[string]any{"language": "typescript", "source": f.sources[r.PathValue("n")][v]}
	})
	m.on("GET /api/v1/functions/{n}/secrets", func(r *http.Request, _ map[string]any) (int, any) {
		f.mu.Lock()
		defer f.mu.Unlock()
		out := []any{}
		for k := range f.secrets[r.PathValue("n")] {
			out = append(out, map[string]any{"name": k, "created_at": "x", "updated_at": "x"})
		}
		return 200, map[string]any{"items": out}
	})
	m.on("PUT /api/v1/functions/{n}/secrets/{k}", func(r *http.Request, b map[string]any) (int, any) {
		f.mu.Lock()
		defer f.mu.Unlock()
		n := r.PathValue("n")
		if f.secrets[n] == nil {
			f.secrets[n] = map[string]string{}
		}
		f.secrets[n][r.PathValue("k")] = b["value"].(string)
		return 200, map[string]any{"name": r.PathValue("k"), "created_at": "x", "updated_at": "x"}
	})
	m.on("DELETE /api/v1/functions/{n}/secrets/{k}", func(r *http.Request, _ map[string]any) (int, any) {
		f.mu.Lock()
		defer f.mu.Unlock()
		delete(f.secrets[r.PathValue("n")], r.PathValue("k"))
		return 204, nil
	})
	m.on("GET /api/v1/functions/{n}/kv-rules", func(r *http.Request, _ map[string]any) (int, any) {
		f.mu.Lock()
		defer f.mu.Unlock()
		return 200, map[string]any{"items": orDefault(f.kv[r.PathValue("n")], []any{})}
	})
	m.on("PUT /api/v1/functions/{n}/kv-rules", func(r *http.Request, b map[string]any) (int, any) {
		f.mu.Lock()
		defer f.mu.Unlock()
		f.kv[r.PathValue("n")], _ = b["items"].([]any)
		return 200, b
	})
	m.on("DELETE /api/v1/functions/{n}", func(r *http.Request, _ map[string]any) (int, any) {
		f.mu.Lock()
		defer f.mu.Unlock()
		if _, ok := f.fn[r.PathValue("n")]; !ok {
			return notFound()
		}
		delete(f.fn, r.PathValue("n"))
		return 204, nil
	})
	return f
}

const fnRes = "forgebase_function"

func writeFile(t *testing.T, name, content string) string {
	t.Helper()
	p := filepath.Join(t.TempDir(), name)
	if err := os.WriteFile(p, []byte(content), 0o600); err != nil {
		t.Fatal(err)
	}
	return p
}

func TestFunctionSourceFileLifecycle(t *testing.T) {
	m := newMock(t)
	fake := newFakeFunctions(m)
	k := newKit(t, m)
	file := writeFile(t, "hello.ts", "serve(() => 'v1')")
	cfg := map[string]any{
		"name": "hello", "source_file": file, "capabilities": []any{"log", "env"}, "env": map[string]any{"MODE": "prod"},
		"secrets": map[string]any{"API_TOKEN": "s3cr3t"}, "description": "says hello",
	}
	st := k.create(fnRes, cfg)

	body := m.last("POST", "/api/v1/functions").Body
	if body["source"] != "serve(() => 'v1')" || body["activate"] != true || body["description"] != "says hello" || body["env"].(map[string]any)["MODE"] != "prod" {
		t.Errorf("deploy body = %v", body)
	}
	if _, has := body["wasm"]; has {
		t.Errorf("source deploy must not send wasm: %v", body)
	}
	if attr(st, "content_sha256") != sum([]byte("serve(() => 'v1')")) || attr(st, "active_version") != float64(1) {
		t.Errorf("state = %v", goValue(st))
	}
	if got := m.last("PUT", "/api/v1/functions/hello/secrets/API_TOKEN").Body["value"]; got != "s3cr3t" {
		t.Errorf("secret value sent = %v", got)
	}
	for _, a := range k.schemas[fnRes].Block.Attributes {
		if a.Name == "secrets" && !a.Sensitive {
			t.Error("secrets must be sensitive")
		}
	}
	if k.planChanged(fnRes, st, cfg) {
		t.Error("plan after apply is not empty")
	}
	st1, d := k.read(fnRes, st)
	k.failOn(d)
	if !st1.Equal(st) {
		t.Errorf("refresh changed state:\n%v\n%v", goValue(st), goValue(st1))
	}

	// Editing the file changes content_sha256 in the plan (hash-based detection).
	if err := os.WriteFile(file, []byte("serve(() => 'v2')"), 0o600); err != nil {
		t.Fatal(err)
	}
	p := k.plan(fnRes, st, cfg)
	k.failOn(p.Diags)
	if got := attr(p.Planned, "content_sha256"); got != sum([]byte("serve(() => 'v2')")) {
		t.Errorf("planned hash = %v", got)
	}
	if p.Planned.Equal(st) {
		t.Fatal("a changed file must show a diff")
	}
	st2, d := k.apply(fnRes, st, p)
	k.failOn(d)
	if attr(st2, "active_version") != float64(2) || attr(st2, "content_sha256") != sum([]byte("serve(() => 'v2')")) {
		t.Errorf("after redeploy = %v", goValue(st2))
	}
	if len(m.requests("POST", "/api/v1/functions")) != 2 {
		t.Errorf("expected exactly two deploys: %v", m.summary())
	}

	// The file changing between plan and apply is refused, not silently deployed.
	if err := os.WriteFile(file, []byte("serve(() => 'v3')"), 0o600); err != nil {
		t.Fatal(err)
	}
	p2 := k.plan(fnRes, st2, cfg)
	if err := os.WriteFile(file, []byte("serve(() => 'v4')"), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, d := k.apply(fnRes, st2, p2); !strings.Contains(errText(d), "code changed since the plan") {
		t.Errorf("plan/apply race: %q", errText(d))
	}
	if err := os.WriteFile(file, []byte("serve(() => 'v2')"), 0o600); err != nil {
		t.Fatal(err)
	}

	// Drift: someone deploys another version out of band.
	fake.mu.Lock()
	fake.versions["hello"][len(fake.versions["hello"])-1]["sha256"] = sum([]byte("tampered"))
	fake.mu.Unlock()
	st3, d := k.read(fnRes, st2)
	k.failOn(d)
	if attr(st3, "content_sha256") != sum([]byte("tampered")) {
		t.Errorf("drift not refreshed: %v", attr(st3, "content_sha256"))
	}
	if !k.planChanged(fnRes, st3, cfg) {
		t.Error("out-of-band code change must produce a diff")
	}

	// Delete.
	if e := errText(k.destroy(fnRes, st2)); e != "" {
		t.Fatal(e)
	}
	gone, d := k.read(fnRes, st2)
	k.failOn(d)
	if !gone.IsNull() {
		t.Error("deleted function must leave state")
	}
}

func TestFunctionSecretsAndKVRules(t *testing.T) {
	m := newMock(t)
	fake := newFakeFunctions(m)
	k := newKit(t, m)
	cfg := map[string]any{
		"name": "job", "source": "serve(() => 1)", "language": "javascript",
		"secrets":  map[string]any{"A": "1", "B": "2"},
		"kv_rules": []any{map[string]any{"namespace": "cache", "access": "read"}},
	}
	st := k.create(fnRes, cfg)
	if body := m.last("POST", "/api/v1/functions").Body; body["language"] != "javascript" {
		t.Errorf("language = %v", body["language"])
	}
	if len(fake.secrets["job"]) != 2 {
		t.Errorf("secrets on server = %v", fake.secrets["job"])
	}
	if items := m.last("PUT", "/api/v1/functions/job/kv-rules").Body["items"].([]any); len(items) != 1 {
		t.Errorf("kv rules sent = %v", items)
	}
	if k.planChanged(fnRes, st, cfg) {
		t.Error("plan after apply is not empty")
	}

	// Change A, drop B, keep nothing else: no redeploy for secret-only changes.
	deploys := len(m.requests("POST", "/api/v1/functions"))
	cfg2 := map[string]any{"name": "job", "source": "serve(() => 1)", "language": "javascript",
		"secrets": map[string]any{"A": "changed"}, "kv_rules": cfg["kv_rules"]}
	st2, _ := k.update(fnRes, st, cfg2)
	if len(m.requests("POST", "/api/v1/functions")) != deploys {
		t.Error("a secrets-only change must not deploy a new version")
	}
	if fake.secrets["job"]["A"] != "changed" || len(fake.secrets["job"]) != 1 {
		t.Errorf("secrets on server = %v", fake.secrets["job"])
	}
	if attr(st2, "active_version") != float64(1) {
		t.Errorf("version bumped: %v", attr(st2, "active_version"))
	}

	// A secret deleted out of band is re-created by the next apply.
	fake.mu.Lock()
	delete(fake.secrets["job"], "A")
	fake.mu.Unlock()
	st3, d := k.read(fnRes, st2)
	k.failOn(d)
	if attr(st3, "secrets.A") != nil {
		t.Errorf("missing secret should leave state: %v", goValue(st3))
	}
	if !k.planChanged(fnRes, st3, cfg2) {
		t.Error("missing secret must show a diff")
	}
	k.update(fnRes, st3, cfg2)
	if fake.secrets["job"]["A"] != "changed" {
		t.Errorf("secret not restored: %v", fake.secrets["job"])
	}

	// kv rules change without a deploy.
	cfg3 := map[string]any{"name": "job", "source": "serve(() => 1)", "language": "javascript", "secrets": cfg2["secrets"],
		"kv_rules": []any{map[string]any{"namespace": "cache", "access": "write"}}}
	k.update(fnRes, st2, cfg3)
	if items := m.last("PUT", "/api/v1/functions/job/kv-rules").Body["items"].([]any); items[0].(map[string]any)["access"] != "write" {
		t.Errorf("kv rules = %v", items)
	}
}

func TestFunctionWasmAndInlineSource(t *testing.T) {
	m := newMock(t)
	newFakeFunctions(m)
	k := newKit(t, m)
	wasm := []byte{0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00}
	file := filepath.Join(t.TempDir(), "m.wasm")
	if err := os.WriteFile(file, wasm, 0o600); err != nil {
		t.Fatal(err)
	}
	st := k.create(fnRes, map[string]any{"name": "w", "wasm_file": file, "limits": map[string]any{"timeout_ms": 1000}})
	body := m.last("POST", "/api/v1/functions").Body
	if body["wasm"] != base64.StdEncoding.EncodeToString(wasm) || body["language"] != "wasm" {
		t.Errorf("wasm body = %v", body)
	}
	if body["limits"].(map[string]any)["timeout_ms"] != float64(1000) {
		t.Errorf("limits = %v", body["limits"])
	}
	if attr(st, "content_sha256") != sum(wasm) || attr(st, "language") != "wasm" || attr(st, "limits.timeout_ms") != float64(1000) || attr(st, "limits.fuel") != float64(100000000) {
		t.Errorf("state = %v", goValue(st))
	}
	if k.planChanged(fnRes, st, map[string]any{"name": "w", "wasm_file": file, "limits": map[string]any{"timeout_ms": 1000}}) {
		t.Error("plan after apply is not empty")
	}
	// Raising the limit redeploys.
	st2, _ := k.update(fnRes, st, map[string]any{"name": "w", "wasm_file": file, "limits": map[string]any{"timeout_ms": 2000}})
	if attr(st2, "active_version") != float64(2) || attr(st2, "limits.timeout_ms") != float64(2000) {
		t.Errorf("after limit change = %v", goValue(st2))
	}
}

func TestFunctionImportShowsSource(t *testing.T) {
	m := newMock(t)
	newFakeFunctions(m)
	k := newKit(t, m)
	k.create(fnRes, map[string]any{"name": "greet", "source": "serve(() => 'hi')"})
	st, d := k.importState(fnRes, "greet")
	k.failOn(d)
	if attr(st, "source") != "serve(() => 'hi')" || attr(st, "name") != "greet" {
		t.Errorf("imported = %v", goValue(st))
	}
	ds, d := k.dataSource("forgebase_function", map[string]any{"name": "greet"})
	k.failOn(d)
	if attr(ds, "active_version") != float64(1) || attr(ds, "content_sha256") != sum([]byte("serve(() => 'hi')")) {
		t.Errorf("data source = %v", goValue(ds))
	}
	if _, d := k.dataSource("forgebase_function", map[string]any{"name": "none"}); !strings.Contains(errText(d), "function_not_found") {
		t.Errorf("missing function: %q", errText(d))
	}
}

func TestFunctionValidation(t *testing.T) {
	m := newMock(t)
	k := newKit(t, m)
	file := writeFile(t, "a.ts", "x")
	for name, c := range map[string]struct {
		cfg  map[string]any
		want string
	}{
		"no code":    {map[string]any{"name": "a"}, "Exactly one of these attributes"},
		"two codes":  {map[string]any{"name": "a", "source": "x", "source_file": file}, "Exactly one of these attributes"},
		"bad name":   {map[string]any{"name": "Bad", "source": "x"}, "must be 1-48"},
		"bad cap":    {map[string]any{"name": "a", "source": "x", "capabilities": []any{"root"}}, "value must be one of"},
		"wasm lang":  {map[string]any{"name": "a", "wasm_file": file, "language": "typescript"}, "Language mismatch"},
		"src wasm":   {map[string]any{"name": "a", "source": "x", "language": "wasm"}, "Language mismatch"},
		"bad access": {map[string]any{"name": "a", "source": "x", "access": "everyone"}, "access value must be one of"},
		"bad hook":   {map[string]any{"name": "a", "source": "x", "hooks": []any{map[string]any{"collection": "c", "events": []any{"on_create"}}}}, "value must be one of"},
	} {
		if e := errText(k.validate(fnRes, c.cfg)); !strings.Contains(e, c.want) {
			t.Errorf("%s: want %q in %q", name, c.want, e)
		}
	}
	// A missing code file is reported when planning.
	p := k.plan(fnRes, tftypesNull(k, fnRes), map[string]any{"name": "a", "source_file": filepath.Join(t.TempDir(), "missing.ts")})
	if !strings.Contains(errText(p.Diags), "Cannot read function code") {
		t.Errorf("missing file: %q", errText(p.Diags))
	}
	// Deploy errors are surfaced.
	m.on("POST /api/v1/functions", func(*http.Request, map[string]any) (int, any) {
		return 400, apiErr("invalid_function", "source does not parse")
	})
	if e := k.createErr(fnRes, map[string]any{"name": "a", "source": "x"}); !strings.Contains(e, "source does not parse") {
		t.Errorf("deploy error: %q", e)
	}
}
