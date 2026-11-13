package provider_test

import (
	"net/http"
	"strings"
	"sync"
	"testing"
)

// ---- forgebase_index ------------------------------------------------------------

func TestIndexLifecycle(t *testing.T) {
	m := newMock(t)
	var mu sync.Mutex
	idx := map[string]map[string]any{}
	m.on("POST /api/v1/indexes", func(_ *http.Request, b map[string]any) (int, any) {
		mu.Lock()
		defer mu.Unlock()
		name, _ := b["name"].(string)
		if name == "" {
			name = "idx_" + b["collection"].(string) + "_title_ab12"
		}
		if _, ok := idx[name]; ok {
			return 409, apiErr("already_exists", "index exists")
		}
		info := map[string]any{"name": name, "table": b["collection"], "origin": "manual", "kind": "btree", "unique": b["unique"], "partial": false, "columns": []any{"title"}, "stats": map[string]any{}, "definition": b}
		idx[name] = info
		return 201, info
	})
	m.on("GET /api/v1/indexes/{name}", func(r *http.Request, _ map[string]any) (int, any) {
		mu.Lock()
		defer mu.Unlock()
		if i, ok := idx[r.PathValue("name")]; ok {
			return 200, i
		}
		return 404, apiErr("not_found", "index not found")
	})
	m.on("DELETE /api/v1/indexes/{name}", func(r *http.Request, _ map[string]any) (int, any) {
		mu.Lock()
		defer mu.Unlock()
		delete(idx, r.PathValue("name"))
		return 204, nil
	})
	k := newKit(t, m)
	cfg := map[string]any{"collection": "posts", "unique": true, "keys": []any{map[string]any{"path": "title"}, map[string]any{"path": "created", "desc": true}}}
	st := k.create("forgebase_index", cfg)

	body := m.last("POST", "/api/v1/indexes").Body
	keys := body["keys"].([]any)
	if body["kind"] != "btree" || body["unique"] != true || keys[1].(map[string]any)["desc"] != true || keys[0].(map[string]any)["desc"] != false {
		t.Errorf("index body = %v", body)
	}
	if _, named := body["name"]; named {
		t.Errorf("an omitted name must not be sent: %v", body)
	}
	name := attr(st, "name").(string)
	if name == "" || attr(st, "id") != name {
		t.Errorf("name/id = %v / %v", name, attr(st, "id"))
	}
	if k.planChanged("forgebase_index", st, cfg) {
		t.Error("plan after apply is not empty")
	}

	// Any change replaces the index.
	cfg2 := map[string]any{"collection": "posts", "unique": true, "keys": []any{map[string]any{"path": "title"}}}
	if _, replace := k.update("forgebase_index", st, cfg2); len(replace) == 0 {
		t.Error("changing keys must replace the index")
	}
	cfg3 := map[string]any{"collection": "posts", "unique": false, "keys": cfg["keys"]}
	if _, replace := k.update("forgebase_index", st, cfg3); len(replace) == 0 {
		t.Error("changing unique must replace the index")
	}

	if e := errText(k.destroy("forgebase_index", st)); e != "" {
		t.Fatal(e)
	}
	gone, d := k.read("forgebase_index", st)
	k.failOn(d)
	if !gone.IsNull() {
		t.Error("dropped index must leave state")
	}
	if e := k.createErr("forgebase_index", map[string]any{"collection": "posts", "name": "dup", "keys": cfg["keys"]}); e != "" {
		t.Fatal(e)
	}
	if e := k.createErr("forgebase_index", map[string]any{"collection": "posts", "name": "dup", "keys": cfg["keys"]}); !strings.Contains(e, "already_exists") {
		t.Errorf("conflict mapping: %q", e)
	}
}

func TestIndexValidation(t *testing.T) {
	k := newKit(t, newMock(t))
	for name, c := range map[string]struct {
		cfg  map[string]any
		want string
	}{
		"no keys":  {map[string]any{"collection": "a", "keys": []any{}}, "at least 1"},
		"bad name": {map[string]any{"collection": "a", "name": "9x", "keys": []any{map[string]any{"path": "x"}}}, "must be 1-63"},
		"bad kind": {map[string]any{"collection": "a", "kind": "hash", "keys": []any{map[string]any{"path": "x"}}}, "kind value must be one of"},
	} {
		if e := errText(k.validate("forgebase_index", c.cfg)); !strings.Contains(e, c.want) {
			t.Errorf("%s: want %q in %q", name, c.want, e)
		}
	}
}

// ---- forgebase_collection_rules ----------------------------------------------------

func TestCollectionRules(t *testing.T) {
	m := newMock(t)
	var mu sync.Mutex
	var stored map[string]any
	m.on("PUT /api/v1/policies/{c}", func(r *http.Request, b map[string]any) (int, any) {
		mu.Lock()
		defer mu.Unlock()
		if r.PathValue("c") == "ghost" {
			return 404, apiErr("not_found", "collection ghost not found")
		}
		b["collection"] = r.PathValue("c")
		stored = b
		return 200, b
	})
	m.on("GET /api/v1/policies/{c}", func(r *http.Request, _ map[string]any) (int, any) {
		mu.Lock()
		defer mu.Unlock()
		if stored == nil {
			return 404, apiErr("not_found", "collection not found")
		}
		return 200, stored
	})
	m.on("DELETE /api/v1/policies/{c}", func(r *http.Request, _ map[string]any) (int, any) {
		mu.Lock()
		defer mu.Unlock()
		stored = nil
		return 200, map[string]any{}
	})
	k := newKit(t, m)
	cfg := map[string]any{
		"collection": "posts",
		"list_rule":  "", // open to everyone
		"view_rule":  `@request.auth.id != ""`,
		// create_rule omitted: locked
		"field_rules": map[string]any{"secret": map[string]any{"read": `@request.auth.role = "admin"`}},
	}
	st := k.create("forgebase_collection_rules", cfg)
	body := m.last("PUT", "/api/v1/policies/posts").Body
	if body["list_rule"] != "" || body["create_rule"] != nil || body["view_rule"] != `@request.auth.id != ""` {
		t.Errorf("rules body = %v (empty string opens, null locks)", body)
	}
	if fr := body["fields"].(map[string]any)["secret"].(map[string]any); fr["read"] != `@request.auth.role = "admin"` || fr["write"] != nil {
		t.Errorf("field rule body = %v", fr)
	}
	if attr(st, "list_rule") != "" || attr(st, "create_rule") != nil {
		t.Errorf("state must keep \"\" (open) distinct from null (locked): %v", goValue(st))
	}
	if k.planChanged("forgebase_collection_rules", st, cfg) {
		t.Error("plan after apply is not empty")
	}

	cfg["create_rule"] = "@request.auth.id != \"\""
	delete(cfg, "field_rules")
	st2, _ := k.update("forgebase_collection_rules", st, cfg)
	if attr(st2, "create_rule") == nil || attr(st2, "field_rules") != nil {
		t.Errorf("after update: %v", goValue(st2))
	}
	if fields := m.last("PUT", "/api/v1/policies/posts").Body["fields"].(map[string]any); len(fields) != 0 {
		t.Errorf("removed field rules must be sent as empty: %v", fields)
	}

	if e := errText(k.destroy("forgebase_collection_rules", st2)); e != "" {
		t.Fatal(e)
	}
	if len(m.requests("DELETE", "/api/v1/policies/posts")) != 1 {
		t.Errorf("delete not sent: %v", m.summary())
	}
	gone, _ := k.read("forgebase_collection_rules", st2)
	if !gone.IsNull() {
		t.Error("rules gone (404) must leave state")
	}
	if e := k.createErr("forgebase_collection_rules", map[string]any{"collection": "ghost"}); !strings.Contains(e, "collection ghost not found") {
		t.Errorf("404 on create: %q", e)
	}
}

// ---- forgebase_storage_bucket ------------------------------------------------------

type fakeBuckets struct {
	mu sync.Mutex
	b  map[string]map[string]any
}

func newFakeBuckets(m *mockServer) *fakeBuckets {
	f := &fakeBuckets{b: map[string]map[string]any{}}
	m.on("POST /api/v1/storage/buckets", func(_ *http.Request, b map[string]any) (int, any) {
		f.mu.Lock()
		defer f.mu.Unlock()
		n := b["name"].(string)
		if _, ok := f.b[n]; ok {
			return 409, apiErr("bucket_exists", "bucket exists")
		}
		out := map[string]any{"name": n, "visibility": orDefault(b["visibility"], "private"), "versioning": orDefault(b["versioning"], false),
			"max_object_bytes": b["max_object_bytes"], "allowed_content_types": b["allowed_content_types"], "owner": "admin:1", "tenant": nil,
			"created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z"}
		f.b[n] = out
		return 201, out
	})
	m.on("GET /api/v1/storage/buckets/{b}", func(r *http.Request, _ map[string]any) (int, any) {
		f.mu.Lock()
		defer f.mu.Unlock()
		if o, ok := f.b[r.PathValue("b")]; ok {
			return 200, o
		}
		return 404, apiErr("bucket_not_found", "no such bucket")
	})
	m.on("PATCH /api/v1/storage/buckets/{b}", func(r *http.Request, b map[string]any) (int, any) {
		f.mu.Lock()
		defer f.mu.Unlock()
		o, ok := f.b[r.PathValue("b")]
		if !ok {
			return 404, apiErr("bucket_not_found", "no such bucket")
		}
		for _, k := range []string{"visibility", "versioning"} {
			if b[k] != nil {
				o[k] = b[k]
			}
		}
		if v, ok := b["max_object_bytes"].(float64); ok {
			if v == 0 {
				o["max_object_bytes"] = nil
			} else {
				o["max_object_bytes"] = v
			}
		}
		if v, ok := b["allowed_content_types"].([]any); ok {
			if len(v) == 0 {
				o["allowed_content_types"] = nil
			} else {
				o["allowed_content_types"] = v
			}
		}
		o["updated_at"] = "2026-02-02T00:00:00Z"
		return 200, o
	})
	m.on("DELETE /api/v1/storage/buckets/{b}", func(r *http.Request, _ map[string]any) (int, any) {
		f.mu.Lock()
		defer f.mu.Unlock()
		if _, ok := f.b[r.PathValue("b")]; !ok {
			return 404, apiErr("bucket_not_found", "no such bucket")
		}
		if r.URL.Query().Get("force") != "true" {
			return 409, apiErr("bucket_not_empty", "bucket has objects")
		}
		delete(f.b, r.PathValue("b"))
		return 204, nil
	})
	return f
}

func orDefault(v, d any) any {
	if v == nil {
		return d
	}
	return v
}

func TestStorageBucketLifecycle(t *testing.T) {
	m := newMock(t)
	fake := newFakeBuckets(m)
	k := newKit(t, m)
	cfg := map[string]any{"name": "avatars", "visibility": "public", "max_object_bytes": 1048576, "allowed_content_types": []any{"image/png", "image/jpeg"}}
	st := k.create("forgebase_storage_bucket", cfg)

	body := m.last("POST", "/api/v1/storage/buckets").Body
	if body["visibility"] != "public" || body["max_object_bytes"] != float64(1048576) || len(body["allowed_content_types"].([]any)) != 2 || body["versioning"] != false {
		t.Errorf("create body = %v", body)
	}
	if attr(st, "owner") != "admin:1" || attr(st, "id") != "avatars" || attr(st, "force_destroy") != false {
		t.Errorf("state = %v", goValue(st))
	}
	if k.planChanged("forgebase_storage_bucket", st, cfg) {
		t.Error("plan after apply is not empty")
	}

	// Update: drop the limit and the allow list (PATCH clears with 0 / []), flip versioning.
	cfg2 := map[string]any{"name": "avatars", "visibility": "authenticated", "versioning": true}
	st2, _ := k.update("forgebase_storage_bucket", st, cfg2)
	patch := m.last("PATCH", "/api/v1/storage/buckets/avatars").Body
	if patch["max_object_bytes"] != float64(0) || len(patch["allowed_content_types"].([]any)) != 0 || patch["versioning"] != true || patch["visibility"] != "authenticated" {
		t.Errorf("patch body = %v", patch)
	}
	if attr(st2, "max_object_bytes") != nil || attr(st2, "allowed_content_types") != nil || attr(st2, "visibility") != "authenticated" {
		t.Errorf("state after update = %v", goValue(st2))
	}
	if k.planChanged("forgebase_storage_bucket", st2, cfg2) {
		t.Error("plan after update is not empty")
	}

	// Name changes replace.
	if _, replace := k.update("forgebase_storage_bucket", st2, map[string]any{"name": "avatars2"}); len(replace) == 0 {
		t.Error("renaming must replace")
	}

	// A non-empty bucket is only deleted with force_destroy.
	if e := errText(k.destroy("forgebase_storage_bucket", st2)); !strings.Contains(e, "bucket_not_empty") {
		t.Errorf("delete without force: %q", e)
	}
	cfg3 := map[string]any{"name": "avatars", "visibility": "authenticated", "versioning": true, "force_destroy": true}
	st3, _ := k.update("forgebase_storage_bucket", st2, cfg3)
	if e := errText(k.destroy("forgebase_storage_bucket", st3)); e != "" {
		t.Fatal(e)
	}
	if q := m.last("DELETE", "/api/v1/storage/buckets/avatars").Query; q != "force=true" {
		t.Errorf("delete query = %q", q)
	}

	// 404 on refresh removes the bucket from state.
	fake.mu.Lock()
	fake.b = map[string]map[string]any{}
	fake.mu.Unlock()
	gone, d := k.read("forgebase_storage_bucket", st)
	k.failOn(d)
	if !gone.IsNull() {
		t.Error("missing bucket must leave state")
	}
}

func TestStorageBucketImportAndDataSource(t *testing.T) {
	m := newMock(t)
	newFakeBuckets(m)
	k := newKit(t, m)
	created := k.create("forgebase_storage_bucket", map[string]any{"name": "docs"})
	st, d := k.importState("forgebase_storage_bucket", "docs")
	k.failOn(d)
	sameState(t, "bucket", created, st)
	if attr(st, "name") != "docs" || attr(st, "visibility") != "private" || attr(st, "force_destroy") != false {
		t.Errorf("imported = %v", goValue(st))
	}
	ds, d := k.dataSource("forgebase_storage_bucket", map[string]any{"name": "docs"})
	k.failOn(d)
	if attr(ds, "owner") != "admin:1" {
		t.Errorf("data source = %v", goValue(ds))
	}
	if e := k.createErr("forgebase_storage_bucket", map[string]any{"name": "docs"}); !strings.Contains(e, "bucket_exists") {
		t.Errorf("conflict mapping: %q", e)
	}
	for name, c := range map[string]struct {
		cfg  map[string]any
		want string
	}{
		"upper":      {map[string]any{"name": "Docs"}, "must be 1-63"},
		"visibility": {map[string]any{"name": "d", "visibility": "world"}, "visibility value must be one of"},
		"zero limit": {map[string]any{"name": "d", "max_object_bytes": 0}, "at least 1"},
	} {
		if e := errText(k.validate("forgebase_storage_bucket", c.cfg)); !strings.Contains(e, c.want) {
			t.Errorf("%s: want %q in %q", name, c.want, e)
		}
	}
}

// ---- forgebase_storage_bucket_acl ----------------------------------------------------

func TestStorageBucketACL(t *testing.T) {
	m := newMock(t)
	var mu sync.Mutex
	acl := map[string][]any{}
	m.on("PUT /api/v1/storage/buckets/{b}/acl", func(r *http.Request, b map[string]any) (int, any) {
		mu.Lock()
		defer mu.Unlock()
		if r.PathValue("b") == "ghost" {
			return 404, apiErr("bucket_not_found", "no such bucket")
		}
		e, _ := b["entries"].([]any)
		acl[r.PathValue("b")] = e
		return 200, map[string]any{"entries": e}
	})
	m.on("GET /api/v1/storage/buckets/{b}/acl", func(r *http.Request, _ map[string]any) (int, any) {
		mu.Lock()
		defer mu.Unlock()
		e, ok := acl[r.PathValue("b")]
		if !ok {
			return 404, apiErr("bucket_not_found", "no such bucket")
		}
		return 200, map[string]any{"entries": e}
	})
	k := newKit(t, m)
	cfg := map[string]any{"bucket": "docs", "entries": []any{
		map[string]any{"grantee": "public", "permission": "read", "prefix": "public/"},
		map[string]any{"grantee": "role:editors", "permission": "write"},
	}}
	st := k.create("forgebase_storage_bucket_acl", cfg)
	body := m.last("PUT", "/api/v1/storage/buckets/docs/acl").Body["entries"].([]any)
	if len(body) != 2 {
		t.Fatalf("entries sent = %v", body)
	}
	foundEmptyPrefix := false
	for _, e := range body {
		if e.(map[string]any)["grantee"] == "role:editors" && e.(map[string]any)["prefix"] == "" {
			foundEmptyPrefix = true
		}
	}
	if !foundEmptyPrefix {
		t.Errorf("an omitted prefix is sent as \"\": %v", body)
	}
	if k.planChanged("forgebase_storage_bucket_acl", st, cfg) {
		t.Error("plan after apply is not empty (set semantics, null prefix)")
	}

	// Entries added out of band are drift.
	mu.Lock()
	acl["docs"] = append(acl["docs"], map[string]any{"prefix": "", "grantee": "authenticated", "permission": "admin"})
	mu.Unlock()
	st2, d := k.read("forgebase_storage_bucket_acl", st)
	k.failOn(d)
	if !k.planChanged("forgebase_storage_bucket_acl", st2, cfg) {
		t.Error("out-of-band grant must show as drift")
	}
	st3, _ := k.update("forgebase_storage_bucket_acl", st2, cfg)
	if len(m.last("PUT", "/api/v1/storage/buckets/docs/acl").Body["entries"].([]any)) != 2 {
		t.Error("update must replace the whole list")
	}

	if e := errText(k.destroy("forgebase_storage_bucket_acl", st3)); e != "" {
		t.Fatal(e)
	}
	if got := m.last("PUT", "/api/v1/storage/buckets/docs/acl").Body["entries"].([]any); len(got) != 0 {
		t.Errorf("destroy must empty the ACL: %v", got)
	}
	if e := k.createErr("forgebase_storage_bucket_acl", map[string]any{"bucket": "ghost", "entries": []any{}}); !strings.Contains(e, "bucket_not_found") {
		t.Errorf("404: %q", e)
	}
	if e := errText(k.validate("forgebase_storage_bucket_acl", map[string]any{"bucket": "d", "entries": []any{map[string]any{"grantee": "everyone", "permission": "read"}}})); !strings.Contains(e, "must be user:") {
		t.Errorf("grantee validation: %q", e)
	}
	if e := errText(k.validate("forgebase_storage_bucket_acl", map[string]any{"bucket": "d", "entries": []any{map[string]any{"grantee": "public", "permission": "delete"}}})); !strings.Contains(e, "permission value must be one of") {
		t.Errorf("permission validation: %q", e)
	}
}
