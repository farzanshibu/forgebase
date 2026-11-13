package provider_test

import (
	"fmt"
	"net/http"
	"strings"
	"sync"
	"testing"
)

type fakeAuth struct {
	mu      sync.Mutex
	sas     map[string]map[string]any
	keys    map[string][]map[string]any // sa id -> keys
	clients map[string]map[string]any
	n       int
}

func newFakeAuth(m *mockServer) *fakeAuth {
	f := &fakeAuth{sas: map[string]map[string]any{}, keys: map[string][]map[string]any{}, clients: map[string]map[string]any{}}
	m.on("POST /api/auth/service-accounts", func(_ *http.Request, b map[string]any) (int, any) {
		f.mu.Lock()
		defer f.mu.Unlock()
		for _, s := range f.sas {
			if s["name"] == b["name"] {
				return 409, apiErr("conflict", "a service account named "+b["name"].(string)+" already exists")
			}
		}
		f.n++
		id := fmt.Sprintf("sa%d", f.n)
		s := map[string]any{"id": id, "name": b["name"], "description": orDefault(b["description"], ""), "role": orDefault(b["role"], "service"),
			"created_by": "admin:1", "created_at": "2026-01-01T00:00:00Z", "disabled": false, "realm": "default"}
		f.sas[id] = s
		return 201, s
	})
	m.on("GET /api/auth/service-accounts", func(*http.Request, map[string]any) (int, any) {
		f.mu.Lock()
		defer f.mu.Unlock()
		out := []any{}
		for _, s := range f.sas {
			out = append(out, s)
		}
		return 200, map[string]any{"items": out}
	})
	m.on("GET /api/auth/service-accounts/{id}", func(r *http.Request, _ map[string]any) (int, any) {
		f.mu.Lock()
		defer f.mu.Unlock()
		if s, ok := f.sas[r.PathValue("id")]; ok {
			return 200, s
		}
		return 404, apiErr("not_found", "service account not found")
	})
	m.on("DELETE /api/auth/service-accounts/{id}", func(r *http.Request, _ map[string]any) (int, any) {
		f.mu.Lock()
		defer f.mu.Unlock()
		if _, ok := f.sas[r.PathValue("id")]; !ok {
			return 404, apiErr("not_found", "service account not found")
		}
		delete(f.sas, r.PathValue("id"))
		delete(f.keys, r.PathValue("id"))
		return 204, nil
	})
	m.on("POST /api/auth/service-accounts/{id}/keys", func(r *http.Request, b map[string]any) (int, any) {
		f.mu.Lock()
		defer f.mu.Unlock()
		sa := r.PathValue("id")
		if _, ok := f.sas[sa]; !ok {
			return 404, apiErr("not_found", "service account not found")
		}
		f.n++
		id := fmt.Sprintf("k%d", f.n)
		var exp any
		if d, ok := b["expires_in_days"].(float64); ok {
			exp = fmt.Sprintf("2026-%02d-01T00:00:00Z", int(d)%12+1)
		}
		f.keys[sa] = append(f.keys[sa], map[string]any{"id": id, "kind": "api_key", "name": b["name"], "scopes": b["scopes"], "created_at": "2026-01-01T00:00:00Z",
			"expires_at": exp, "revoked": false, "tenant": b["tenant"]})
		return 201, map[string]any{"id": id, "key": "fbk_" + id + "_secret", "name": b["name"], "scopes": b["scopes"]}
	})
	m.on("GET /api/auth/service-accounts/{id}/keys", func(r *http.Request, _ map[string]any) (int, any) {
		f.mu.Lock()
		defer f.mu.Unlock()
		out := []any{}
		for _, k := range f.keys[r.PathValue("id")] {
			out = append(out, k)
		}
		return 200, map[string]any{"items": out}
	})
	m.on("DELETE /api/auth/api-keys/{id}", func(r *http.Request, _ map[string]any) (int, any) {
		f.mu.Lock()
		defer f.mu.Unlock()
		for _, ks := range f.keys {
			for _, k := range ks {
				if k["id"] == r.PathValue("id") {
					k["revoked"] = true
					return 204, nil
				}
			}
		}
		return 404, apiErr("not_found", "key not found")
	})
	return f
}

func TestServiceAccountAndAPIKey(t *testing.T) {
	m := newMock(t)
	fake := newFakeAuth(m)
	k := newKit(t, m)

	sa := k.create("forgebase_service_account", map[string]any{"name": "ci", "description": "CI deploys"})
	if body := m.last("POST", "/api/auth/service-accounts").Body; body["name"] != "ci" || body["role"] != "service" || body["description"] != "CI deploys" {
		t.Errorf("sa body = %v", body)
	}
	saID := attr(sa, "id").(string)
	if attr(sa, "realm") != "default" || attr(sa, "disabled") != false {
		t.Errorf("sa state = %v", goValue(sa))
	}
	if k.planChanged("forgebase_service_account", sa, map[string]any{"name": "ci", "description": "CI deploys"}) {
		t.Error("plan after apply is not empty")
	}
	for name, c := range map[string]map[string]any{
		"name": {"name": "ci2", "description": "CI deploys"}, "description": {"name": "ci"}, "role": {"name": "ci", "description": "CI deploys", "role": "admin"},
	} {
		if _, replace := k.update("forgebase_service_account", sa, c); len(replace) == 0 {
			t.Errorf("changing %s must replace the service account (no update API)", name)
		}
	}
	if e := k.createErr("forgebase_service_account", map[string]any{"name": "ci"}); !strings.Contains(e, "already exists") {
		t.Errorf("409: %q", e)
	}
	ds, d := k.dataSource("forgebase_service_account", map[string]any{"name": "ci"})
	k.failOn(d)
	if attr(ds, "id") != saID {
		t.Errorf("data source = %v", goValue(ds))
	}
	if _, d := k.dataSource("forgebase_service_account", map[string]any{"name": "ghost"}); !strings.Contains(errText(d), "not found") {
		t.Errorf("missing: %q", errText(d))
	}
	imp, d := k.importState("forgebase_service_account", saID)
	k.failOn(d)
	if attr(imp, "name") != "ci" {
		t.Errorf("import = %v", goValue(imp))
	}

	// API key: the secret is shown once and kept as a sensitive value.
	keyCfg := map[string]any{"service_account_id": saID, "name": "deploy", "scopes": []any{"collections:read", "storage:write:avatars"}, "expires_in_days": 30}
	key := k.create("forgebase_api_key", keyCfg)
	kb := m.last("POST", "/api/auth/service-accounts/"+saID+"/keys").Body
	if kb["name"] != "deploy" || kb["expires_in_days"] != float64(30) || len(kb["scopes"].([]any)) != 2 {
		t.Errorf("key body = %v", kb)
	}
	secret := attr(key, "key")
	if !strings.HasPrefix(fmt.Sprint(secret), "fbk_") {
		t.Fatalf("key secret = %v", secret)
	}
	for _, a := range k.schemas["forgebase_api_key"].Block.Attributes {
		if a.Name == "key" && !a.Sensitive {
			t.Error("key must be sensitive")
		}
	}
	if attr(key, "expires_at") == nil || attr(key, "created_at") == nil {
		t.Errorf("key state = %v", goValue(key))
	}
	// Refresh keeps the secret and shows no diff.
	key2, d := k.read("forgebase_api_key", key)
	k.failOn(d)
	if attr(key2, "key") != secret || k.planChanged("forgebase_api_key", key2, keyCfg) {
		t.Errorf("refresh: %v", goValue(key2))
	}
	// Any change replaces the key.
	if _, replace := k.update("forgebase_api_key", key, map[string]any{"service_account_id": saID, "name": "deploy", "scopes": []any{"*"}, "expires_in_days": 30}); len(replace) == 0 {
		t.Error("changing scopes must replace the key")
	}
	// Imported key: no secret.
	impKey, d := k.importState("forgebase_api_key", saID+"/"+attr(key, "id").(string))
	k.failOn(d)
	if attr(impKey, "key") != nil || attr(impKey, "name") != "deploy" {
		t.Errorf("imported key = %v", goValue(impKey))
	}
	if _, d := k.importState("forgebase_api_key", "nokey"); !strings.Contains(errText(d), "Invalid import id") {
		t.Errorf("bad import id: %q", errText(d))
	}

	// A key revoked out of band is dropped from state.
	fake.mu.Lock()
	fake.keys[saID][0]["revoked"] = true
	fake.mu.Unlock()
	gone, d := k.read("forgebase_api_key", key)
	k.failOn(d)
	if !gone.IsNull() {
		t.Error("revoked key must leave state")
	}
	fake.mu.Lock()
	fake.keys[saID][0]["revoked"] = false
	fake.mu.Unlock()
	if e := errText(k.destroy("forgebase_api_key", key)); e != "" {
		t.Fatal(e)
	}
	if len(m.requests("DELETE", "/api/auth/api-keys/")) != 1 {
		t.Errorf("revoke not sent: %v", m.summary())
	}
	for name, c := range map[string]struct {
		cfg  map[string]any
		want string
	}{
		"scope":   {map[string]any{"service_account_id": "x", "name": "k", "scopes": []any{"everything"}}, "must be `*`"},
		"empty":   {map[string]any{"service_account_id": "x", "name": "k", "scopes": []any{}}, "at least 1"},
		"expires": {map[string]any{"service_account_id": "x", "name": "k", "scopes": []any{"*"}, "expires_in_days": 5000}, "between 1 and 3650"},
	} {
		if e := errText(k.validate("forgebase_api_key", c.cfg)); !strings.Contains(e, c.want) {
			t.Errorf("%s: want %q in %q", name, c.want, e)
		}
	}
	if e := errText(k.destroy("forgebase_service_account", sa)); e != "" {
		t.Fatal(e)
	}
	gone, _ = k.read("forgebase_service_account", sa)
	if !gone.IsNull() {
		t.Error("deleted service account must leave state")
	}
}

func oauthRoutes(m *mockServer, f *fakeAuth) {
	view := func(c map[string]any) map[string]any {
		out := copyMap(c)
		delete(out, "secret")
		return out
	}
	m.on("POST /api/auth/clients", func(_ *http.Request, b map[string]any) (int, any) {
		f.mu.Lock()
		defer f.mu.Unlock()
		f.n++
		id := fmt.Sprintf("cli_%d", f.n)
		c := map[string]any{"client_id": id, "realm": orDefault(b["realm"], "default"), "name": b["name"], "description": b["description"],
			"type": orDefault(b["type"], "confidential"), "redirect_uris": orDefault(b["redirect_uris"], []any{}), "post_logout_redirect_uris": orDefault(b["post_logout_redirect_uris"], []any{}),
			"grant_types": orDefault(b["grant_types"], []any{"authorization_code", "refresh_token"}), "scopes": orDefault(b["scopes"], []any{"openid"}),
			"skip_consent": orDefault(b["skip_consent"], false), "access_token_ttl": b["access_token_ttl"], "refresh_token_ttl": b["refresh_token_ttl"],
			"disabled": false, "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z"}
		f.clients[id] = c
		out := map[string]any{"client": view(c)}
		if c["type"] == "confidential" {
			out["client_secret"] = "cs_" + id
		}
		return 201, out
	})
	m.on("GET /api/auth/clients/{id}", func(r *http.Request, _ map[string]any) (int, any) {
		f.mu.Lock()
		defer f.mu.Unlock()
		if c, ok := f.clients[r.PathValue("id")]; ok {
			return 200, view(c)
		}
		return 404, apiErr("not_found", "client not found")
	})
	m.on("PATCH /api/auth/clients/{id}", func(r *http.Request, b map[string]any) (int, any) {
		f.mu.Lock()
		defer f.mu.Unlock()
		c, ok := f.clients[r.PathValue("id")]
		if !ok {
			return 404, apiErr("not_found", "client not found")
		}
		for k, v := range b {
			if k == "description" && v == "" {
				c[k] = nil
				continue
			}
			c[k] = v
		}
		c["updated_at"] = "2026-02-02T00:00:00Z"
		return 200, view(c)
	})
	m.on("DELETE /api/auth/clients/{id}", func(r *http.Request, _ map[string]any) (int, any) {
		f.mu.Lock()
		defer f.mu.Unlock()
		if _, ok := f.clients[r.PathValue("id")]; !ok {
			return 404, apiErr("not_found", "client not found")
		}
		delete(f.clients, r.PathValue("id"))
		return 204, nil
	})
}

func TestOAuthClientLifecycle(t *testing.T) {
	m := newMock(t)
	fake := newFakeAuth(m)
	oauthRoutes(m, fake)
	k := newKit(t, m)
	cfg := map[string]any{
		"name": "Web app", "redirect_uris": []any{"https://app.example.com/cb"}, "scopes": []any{"openid", "email"},
		"access_token_ttl": 900, "description": "the web app",
	}
	st := k.create("forgebase_oauth_client", cfg)
	body := m.last("POST", "/api/auth/clients").Body
	if body["type"] != "confidential" || body["access_token_ttl"] != float64(900) || len(body["grant_types"].([]any)) != 2 || body["skip_consent"] != false {
		t.Errorf("create body = %v", body)
	}
	if _, has := body["realm"]; has {
		t.Errorf("an unset realm is left to the server: %v", body)
	}
	id := attr(st, "id").(string)
	if attr(st, "client_secret") != "cs_"+id || attr(st, "realm") != "default" {
		t.Errorf("state = %v", goValue(st))
	}
	for _, a := range k.schemas["forgebase_oauth_client"].Block.Attributes {
		if a.Name == "client_secret" && !a.Sensitive {
			t.Error("client_secret must be sensitive")
		}
	}
	if k.planChanged("forgebase_oauth_client", st, cfg) {
		t.Error("plan after apply is not empty")
	}
	// The secret survives a refresh.
	st1, d := k.read("forgebase_oauth_client", st)
	k.failOn(d)
	if attr(st1, "client_secret") != "cs_"+id {
		t.Errorf("refresh dropped the secret: %v", goValue(st1))
	}

	// Update in place.
	cfg2 := map[string]any{"name": "Web app v2", "redirect_uris": []any{"https://app.example.com/cb", "https://app.example.com/cb2"},
		"scopes": []any{"openid", "email"}, "access_token_ttl": 1800, "disabled": true}
	st2, rr := k.update("forgebase_oauth_client", st, cfg2)
	if len(rr) > 0 {
		t.Fatalf("unexpected replace: %v", rr)
	}
	patch := m.last("PATCH", "/api/auth/clients/"+id).Body
	if patch["name"] != "Web app v2" || patch["disabled"] != true || patch["access_token_ttl"] != float64(1800) || patch["description"] != "" {
		t.Errorf("patch = %v", patch)
	}
	if attr(st2, "disabled") != true || attr(st2, "description") != nil || attr(st2, "client_secret") != "cs_"+id {
		t.Errorf("state = %v", goValue(st2))
	}

	// Immutable: realm, type; a TTL override cannot be removed in place.
	if _, replace := k.update("forgebase_oauth_client", st2, map[string]any{"name": "x", "type": "public"}); len(replace) == 0 {
		t.Error("type change must replace")
	}
	cfg3 := map[string]any{"name": "Web app v2", "redirect_uris": cfg2["redirect_uris"], "scopes": cfg2["scopes"], "disabled": true}
	if _, replace := k.update("forgebase_oauth_client", st2, cfg3); len(replace) == 0 {
		t.Error("removing a TTL override must replace the client")
	}

	// Public clients have no secret.
	pub := k.create("forgebase_oauth_client", map[string]any{"name": "spa", "type": "public", "redirect_uris": []any{"https://spa.example.com"}})
	if attr(pub, "client_secret") != nil {
		t.Errorf("public client secret = %v", attr(pub, "client_secret"))
	}
	// Import has no secret.
	imp, d := k.importState("forgebase_oauth_client", id)
	k.failOn(d)
	if attr(imp, "client_secret") != nil || attr(imp, "name") != "Web app v2" {
		t.Errorf("import = %v", goValue(imp))
	}
	// 404 and delete.
	if e := errText(k.destroy("forgebase_oauth_client", st2)); e != "" {
		t.Fatal(e)
	}
	gone, d := k.read("forgebase_oauth_client", st2)
	k.failOn(d)
	if !gone.IsNull() {
		t.Error("deleted client must leave state")
	}
	for name, c := range map[string]struct {
		cfg  map[string]any
		want string
	}{
		"grant": {map[string]any{"name": "a", "grant_types": []any{"password"}}, "value must be one of"},
		"ttl":   {map[string]any{"name": "a", "access_token_ttl": 5}, "between 60 and 86400"},
		"uris":  {map[string]any{"name": "a", "redirect_uris": make21()}, "at most 20"},
	} {
		if e := errText(k.validate("forgebase_oauth_client", c.cfg)); !strings.Contains(e, c.want) {
			t.Errorf("%s: want %q in %q", name, c.want, e)
		}
	}
	m2 := newMock(t)
	m2.on("POST /api/auth/clients", func(*http.Request, map[string]any) (int, any) {
		return 400, apiErr("invalid_input", "authorization_code needs at least one redirect URI")
	})
	if e := newKit(t, m2).createErr("forgebase_oauth_client", map[string]any{"name": "a"}); !strings.Contains(e, "needs at least one redirect URI") {
		t.Errorf("server validation error: %q", e)
	}
}

func make21() []any {
	out := make([]any, 21)
	for i := range out {
		out[i] = fmt.Sprintf("https://x.example.com/%d", i)
	}
	return out
}
