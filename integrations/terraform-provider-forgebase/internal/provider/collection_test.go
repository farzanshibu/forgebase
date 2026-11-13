package provider_test

import (
	"fmt"
	"net/http"
	"strings"
	"sync"
	"testing"
)

// fakeCollections is a tiny stateful stand-in for the collections API.
type fakeCollections struct {
	mu   sync.Mutex
	byID map[string]map[string]any
	seq  int
}

func newFakeCollections(m *mockServer) *fakeCollections {
	f := &fakeCollections{byID: map[string]map[string]any{}}
	find := func(key string) map[string]any {
		for id, c := range f.byID {
			if id == key || c["name"] == key {
				return c
			}
		}
		return nil
	}
	m.on("POST /api/v1/collections", func(r *http.Request, b map[string]any) (int, any) {
		f.mu.Lock()
		defer f.mu.Unlock()
		if find(b["name"].(string)) != nil {
			return 409, apiErr("already_exists", "collection exists")
		}
		f.seq++
		b["id"] = fmt.Sprintf("col%d", f.seq)
		f.finish(b, 1)
		b["created"], b["updated"] = "2026-01-01T00:00:00.000Z", "2026-01-01T00:00:00.000Z"
		f.byID[b["id"].(string)] = b
		return 201, b
	})
	m.on("GET /api/v1/collections/{c}", func(r *http.Request, _ map[string]any) (int, any) {
		f.mu.Lock()
		defer f.mu.Unlock()
		if c := find(r.PathValue("c")); c != nil {
			return 200, c
		}
		return 404, apiErr("not_found", "collection not found")
	})
	m.on("PUT /api/v1/collections/{c}", func(r *http.Request, b map[string]any) (int, any) {
		f.mu.Lock()
		defer f.mu.Unlock()
		cur := find(r.PathValue("c"))
		if cur == nil {
			return 404, apiErr("not_found", "collection not found")
		}
		b["id"], b["created"] = cur["id"], cur["created"]
		f.finish(b, int(cur["version"].(float64))+1)
		b["updated"] = "2026-02-02T00:00:00.000Z"
		f.byID[cur["id"].(string)] = b
		return 200, b
	})
	m.on("DELETE /api/v1/collections/{c}", func(r *http.Request, _ map[string]any) (int, any) {
		f.mu.Lock()
		defer f.mu.Unlock()
		c := find(r.PathValue("c"))
		if c == nil {
			return 404, apiErr("not_found", "collection not found")
		}
		delete(f.byID, c["id"].(string))
		return 204, nil
	})
	return f
}

// finish fills what the server fills: ids, system fields, option defaults.
func (f *fakeCollections) finish(b map[string]any, version int) {
	if b["type"] == nil || b["type"] == "" {
		b["type"] = "base"
	}
	if b["schema_mode"] == nil || b["schema_mode"] == "" {
		b["schema_mode"] = "strict"
	}
	if b["options"] == nil {
		b["options"] = map[string]any{}
	}
	b["version"] = float64(version)
	fields, _ := b["fields"].([]any)
	has := map[string]bool{}
	for i, x := range fields {
		fm := x.(map[string]any)
		if fm["id"] == nil || fm["id"] == "" {
			f.seq++
			fm["id"] = fmt.Sprintf("fld%d", f.seq)
		}
		has[fm["name"].(string)] = true
		switch fm["type"] {
		case "select":
			if fm["max_select"] == nil {
				fm["max_select"] = float64(1)
			}
		case "number":
			if fm["integer"] == nil {
				fm["integer"] = false
			}
		}
		fields[i] = fm
	}
	if b["type"] == "auth" {
		for _, n := range []string{"email", "password"} {
			if !has[n] {
				f.seq++
				fields = append(fields, map[string]any{"id": fmt.Sprintf("sys%d", f.seq), "name": n, "type": map[string]string{"email": "email", "password": "password"}[n], "required": true, "unique": n == "email", "hidden": false, "system": true})
			}
		}
	}
	if fields == nil {
		fields = []any{}
	}
	b["fields"] = fields
}

const collectionRes = "forgebase_collection"

func postsConfig() map[string]any {
	return map[string]any{
		"name": "posts",
		"fields": []any{
			map[string]any{"name": "title", "type": "text", "required": true, "options": `{"max_length": 200}`},
			map[string]any{"name": "status", "type": "select", "options": `{"values": ["draft", "live"]}`, "default": `"draft"`},
			map[string]any{"name": "views", "type": "number"},
		},
	}
}

func TestCollectionCreateReadUpdateDelete(t *testing.T) {
	m := newMock(t)
	newFakeCollections(m)
	k := newKit(t, m)

	st := k.create(collectionRes, postsConfig())

	// The request flattened options beside the field keys and sent defaults.
	req := m.last("POST", "/api/v1/collections")
	if m.last("POST", "/api/v1/collections").Auth != "Bearer test-token" {
		t.Errorf("auth header = %q", req.Auth)
	}
	fields := req.Body["fields"].([]any)
	title := fields[0].(map[string]any)
	if title["type"] != "text" || title["max_length"] != float64(200) || title["required"] != true {
		t.Errorf("title field body = %v", title)
	}
	if _, has := title["id"]; has {
		t.Errorf("a new field must not send an id: %v", title)
	}
	status := fields[1].(map[string]any)
	if status["default"] != "draft" || len(status["values"].([]any)) != 2 {
		t.Errorf("status field body = %v", status)
	}
	if req.Body["type"] != "base" || req.Body["schema_mode"] != "strict" || req.Body["versioned"] != false {
		t.Errorf("collection defaults = %v", req.Body)
	}

	if attr(st, "id") == nil || attr(st, "fields.0.id") == nil {
		t.Fatalf("ids missing: %v", goValue(st))
	}
	// Server-added option defaults (max_select) do not appear: only declared keys are tracked.
	if got := attr(st, "fields.1.options"); got != `{"values": ["draft", "live"]}` {
		t.Errorf("tracked options = %v", got)
	}
	if attr(st, "fields.2.options") != nil {
		t.Errorf("undeclared options must stay null, got %v", attr(st, "fields.2.options"))
	}
	if attr(st, "version") != float64(1) {
		t.Errorf("version = %v", attr(st, "version"))
	}

	// No drift right after apply, and a refresh is stable.
	if k.planChanged(collectionRes, st, postsConfig()) {
		t.Error("plan after apply is not empty")
	}
	st2, d := k.read(collectionRes, st)
	k.failOn(d)
	if !st2.Equal(st) {
		t.Errorf("refresh changed state:\n%v\n%v", goValue(st), goValue(st2))
	}

	// Update: add a field, keep the others' ids.
	cfg := postsConfig()
	cfg["fields"] = append(cfg["fields"].([]any), map[string]any{"name": "published", "type": "date"})
	oldTitleID := attr(st, "fields.0.id")
	st3, replace := k.update(collectionRes, st, cfg)
	if len(replace) > 0 {
		t.Fatalf("unexpected replace %v", replace)
	}
	put := m.last("PUT", "/api/v1/collections/")
	pf := put.Body["fields"].([]any)
	if len(pf) != 4 || pf[0].(map[string]any)["id"] != oldTitleID {
		t.Errorf("PUT must keep ids of unchanged fields: %v", pf)
	}
	if _, has := pf[3].(map[string]any)["id"]; has {
		t.Errorf("new field must not carry an id: %v", pf[3])
	}
	if attr(st3, "version") != float64(2) || attr(st3, "fields.3.name") != "published" {
		t.Errorf("after update: %v", goValue(st3))
	}

	if d := k.destroy(collectionRes, st3); errText(d) != "" {
		t.Fatal(errText(d))
	}
	if len(m.requests("DELETE", "/api/v1/collections/col1")) != 1 {
		t.Errorf("delete not sent: %v", m.summary())
	}
}

func TestCollectionRenameKeepsFieldID(t *testing.T) {
	m := newMock(t)
	newFakeCollections(m)
	k := newKit(t, m)
	st := k.create(collectionRes, postsConfig())
	titleID := attr(st, "fields.0.id")

	cfg := postsConfig()
	f0 := cfg["fields"].([]any)[0].(map[string]any)
	f0["name"], f0["renamed_from"] = "headline", "title"
	st2, _ := k.update(collectionRes, st, cfg)

	put := m.last("PUT", "/api/v1/collections/")
	if got := put.Body["fields"].([]any)[0].(map[string]any)["id"]; got != titleID {
		t.Errorf("renamed field sent id %v, want %v", got, titleID)
	}
	if attr(st2, "fields.0.id") != titleID || attr(st2, "fields.0.name") != "headline" {
		t.Errorf("state after rename = %v", goValue(st2))
	}
}

func TestCollectionRenameWithoutHintDropsAndAdds(t *testing.T) {
	m := newMock(t)
	newFakeCollections(m)
	k := newKit(t, m)
	st := k.create(collectionRes, postsConfig())
	cfg := postsConfig()
	cfg["fields"].([]any)[0].(map[string]any)["name"] = "headline"
	k.update(collectionRes, st, cfg)
	put := m.last("PUT", "/api/v1/collections/")
	if _, has := put.Body["fields"].([]any)[0].(map[string]any)["id"]; has {
		t.Errorf("without renamed_from the field is new (no id): %v", put.Body["fields"])
	}
}

func TestCollectionAuthKeepsSystemFields(t *testing.T) {
	m := newMock(t)
	newFakeCollections(m)
	k := newKit(t, m)
	cfg := map[string]any{"name": "members", "type": "auth", "fields": []any{map[string]any{"name": "nick", "type": "text"}}}
	st := k.create(collectionRes, cfg)
	// System fields are hidden from state while the config does not declare them.
	if got := goValue(st).(map[string]any)["fields"].([]any); len(got) != 1 {
		t.Errorf("state fields = %v", got)
	}
	if k.planChanged(collectionRes, st, cfg) {
		t.Error("auth collection shows a perpetual diff")
	}
	cfg["fields"] = append(cfg["fields"].([]any), map[string]any{"name": "bio", "type": "text"})
	k.update(collectionRes, st, cfg)
	names := []string{}
	for _, f := range m.last("PUT", "/api/v1/collections/").Body["fields"].([]any) {
		names = append(names, f.(map[string]any)["name"].(string))
	}
	if strings.Join(names, ",") != "nick,bio,email,password" {
		t.Errorf("PUT fields = %v (system fields must be kept)", names)
	}
}

func TestCollectionTypeChangeReplaces(t *testing.T) {
	m := newMock(t)
	newFakeCollections(m)
	k := newKit(t, m)
	st := k.create(collectionRes, postsConfig())
	cfg := postsConfig()
	cfg["type"] = "auth"
	_, replace := k.update(collectionRes, st, cfg)
	if len(replace) == 0 || replace[0] != `AttributeName("type")` {
		t.Errorf("RequiresReplace = %v", replace)
	}
	cfg = postsConfig()
	cfg["tenant_scoped"] = true
	if _, replace = k.update(collectionRes, st, cfg); len(replace) == 0 {
		t.Error("tenant_scoped must force replacement")
	}
}

func TestCollectionReadDriftAndNotFound(t *testing.T) {
	m := newMock(t)
	fake := newFakeCollections(m)
	k := newKit(t, m)
	st := k.create(collectionRes, postsConfig())

	// A field added out of band shows in state so the next plan removes it.
	fake.mu.Lock()
	c := fake.byID["col1"]
	c["fields"] = append(c["fields"].([]any), map[string]any{"id": "x", "name": "extra", "type": "bool", "required": false, "unique": false, "hidden": false})
	fake.mu.Unlock()
	st2, d := k.read(collectionRes, st)
	k.failOn(d)
	if attr(st2, "fields.3.name") != "extra" {
		t.Errorf("drift not refreshed: %v", goValue(st2))
	}
	if !k.planChanged(collectionRes, st2, postsConfig()) {
		t.Error("drifted field should produce a diff")
	}

	// 404 removes the resource from state.
	fake.mu.Lock()
	delete(fake.byID, "col1")
	fake.mu.Unlock()
	gone, d := k.read(collectionRes, st)
	k.failOn(d)
	if !gone.IsNull() {
		t.Errorf("state not removed: %v", goValue(gone))
	}
}

func TestCollectionErrorMapping(t *testing.T) {
	m := newMock(t)
	newFakeCollections(m)
	k := newKit(t, m)
	k.create(collectionRes, postsConfig())
	e := k.createErr(collectionRes, postsConfig())
	if !strings.Contains(e, "already_exists") || !strings.Contains(e, "409") || !strings.Contains(e, "req-1") {
		t.Errorf("error should carry status, code and request id: %q", e)
	}

	m2 := newMock(t)
	m2.on("POST /api/v1/collections", func(*http.Request, map[string]any) (int, any) {
		return 403, apiErr("forbidden", "collections:manage required")
	})
	k2 := newKit(t, m2)
	if e := k2.createErr(collectionRes, postsConfig()); !strings.Contains(e, "forbidden") || !strings.Contains(e, "collections:manage required") {
		t.Errorf("403 mapping: %q", e)
	}
}

func TestCollectionImportByName(t *testing.T) {
	m := newMock(t)
	newFakeCollections(m)
	k := newKit(t, m)
	k.create(collectionRes, postsConfig())
	st, d := k.importState(collectionRes, "posts")
	k.failOn(d)
	if attr(st, "name") != "posts" || attr(st, "fields.1.options") == nil {
		t.Errorf("imported = %v", goValue(st))
	}
	// Import keeps every server option key so the config can be copied.
	if got := attr(st, "fields.1.options").(string); !strings.Contains(got, "max_select") {
		t.Errorf("imported options = %s", got)
	}
	_, d = k.importState(collectionRes, "nope")
	if len(d) == 0 {
		// A missing collection yields an empty state, which Terraform reports as "cannot import".
		t.Log("import of a missing collection returned no state")
	}
}

func TestCollectionValidation(t *testing.T) {
	m := newMock(t)
	k := newKit(t, m)
	cases := map[string]struct {
		cfg  map[string]any
		want string
	}{
		"select without values": {map[string]any{"name": "a", "fields": []any{map[string]any{"name": "s", "type": "select"}}}, "non-empty `values`"},
		"reserved name":         {map[string]any{"name": "a", "fields": []any{map[string]any{"name": "created", "type": "text"}}}, "reserved"},
		"duplicate":             {map[string]any{"name": "a", "fields": []any{map[string]any{"name": "x", "type": "text"}, map[string]any{"name": "X", "type": "bool"}}}, "declared twice"},
		"bad type":              {map[string]any{"name": "a", "fields": []any{map[string]any{"name": "x", "type": "blob"}}}, "type value must be one of"},
		"view without query":    {map[string]any{"name": "v", "type": "view"}, "needs `view_query`"},
		"query on base":         {map[string]any{"name": "v", "view_query": "select 1"}, "Only `view` collections"},
		"fields on view":        {map[string]any{"name": "v", "type": "view", "view_query": "select 1", "fields": []any{map[string]any{"name": "x", "type": "text"}}}, "derived from the query"},
		"bad name":              {map[string]any{"name": "1bad"}, "must be 1-63"},
		"vector dims":           {map[string]any{"name": "a", "fields": []any{map[string]any{"name": "v", "type": "vector"}}}, "dimensions"},
		"relation target":       {map[string]any{"name": "a", "fields": []any{map[string]any{"name": "r", "type": "relation", "options": `{"max_select":1}`}}}, "collection_id"},
		"password unique":       {map[string]any{"name": "a", "fields": []any{map[string]any{"name": "p", "type": "password", "unique": true}}}, "cannot be unique"},
		"text min>max":          {map[string]any{"name": "a", "fields": []any{map[string]any{"name": "t", "type": "text", "options": `{"min_length":5,"max_length":2}`}}}, "min_length is greater"},
		"options not object":    {map[string]any{"name": "a", "fields": []any{map[string]any{"name": "t", "type": "text", "options": `[1]`}}}, "JSON object"},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			if e := errText(k.validate(collectionRes, tc.cfg)); !strings.Contains(e, tc.want) {
				t.Errorf("want %q in %q", tc.want, e)
			}
		})
	}
	if e := errText(k.validate(collectionRes, postsConfig())); e != "" {
		t.Errorf("valid config rejected: %s", e)
	}
	view := map[string]any{"name": "v", "type": "view", "view_query": "select 1"}
	if e := errText(k.validate(collectionRes, view)); e != "" {
		t.Errorf("valid view rejected: %s", e)
	}
}

func TestCollectionDataSource(t *testing.T) {
	m := newMock(t)
	newFakeCollections(m)
	k := newKit(t, m)
	k.create(collectionRes, postsConfig())
	st, d := k.dataSource("forgebase_collection", map[string]any{"name": "posts"})
	k.failOn(d)
	if attr(st, "type") != "base" || attr(st, "fields.0.name") != "title" || attr(st, "version") != float64(1) {
		t.Errorf("data source = %v", goValue(st))
	}
	_, d = k.dataSource("forgebase_collection", map[string]any{"name": "missing"})
	if !strings.Contains(errText(d), "not_found") {
		t.Errorf("missing collection error = %q", errText(d))
	}
}
