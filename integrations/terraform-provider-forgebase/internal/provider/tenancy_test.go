package provider_test

import (
	"encoding/json"
	"fmt"
	"net/http"
	"strings"
	"sync"
	"testing"
)

func TestRoleRealmAndOrg(t *testing.T) {
	m := newMock(t)
	var mu sync.Mutex
	roles := map[string]map[string]any{} // "<org>/<key>"
	put := func(org string) func(r *http.Request, b map[string]any) (int, any) {
		return func(r *http.Request, b map[string]any) (int, any) {
			mu.Lock()
			defer mu.Unlock()
			name := orDefault(b["name"], b["key"])
			var orgID any
			o := org
			if org != "" {
				o = r.PathValue("org")
				orgID = o
			}
			role := map[string]any{"id": "r_" + b["key"].(string), "org_id": orgID, "key": b["key"], "name": name, "description": b["description"],
				"allow": b["allow"], "deny": b["deny"], "system": false, "assignable": true}
			roles[o+"/"+b["key"].(string)] = role
			return 200, role
		}
	}
	m.on("PUT /api/v1/roles", put(""))
	m.on("PUT /api/v1/orgs/{org}/roles", put("x"))
	list := func(org string) func(r *http.Request, _ map[string]any) (int, any) {
		return func(r *http.Request, _ map[string]any) (int, any) {
			mu.Lock()
			defer mu.Unlock()
			out := []any{roles["/admin"]}
			if roles["/admin"] == nil {
				out = []any{}
			}
			for k, v := range roles {
				if k != "/admin" {
					out = append(out, v)
				}
			}
			if org != "" {
				return 200, map[string]any{"items": out} // org listing uses the envelope; realm uses a bare array
			}
			return 200, out
		}
	}
	m.on("GET /api/v1/roles", list(""))
	m.on("GET /api/v1/orgs/{org}/roles", list("x"))
	m.on("DELETE /api/v1/roles/{key}", func(r *http.Request, _ map[string]any) (int, any) {
		mu.Lock()
		defer mu.Unlock()
		delete(roles, "/"+r.PathValue("key"))
		return 204, nil
	})
	m.on("DELETE /api/v1/orgs/{org}/roles/{key}", func(r *http.Request, _ map[string]any) (int, any) {
		mu.Lock()
		defer mu.Unlock()
		delete(roles, r.PathValue("org")+"/"+r.PathValue("key"))
		return 204, nil
	})
	k := newKit(t, m)

	cfg := map[string]any{"key": "editor", "name": "Editor", "allow": []any{"collections:read", "records:*"}, "deny": []any{"collections:delete"}}
	st := k.create("forgebase_role", cfg)
	body := m.last("PUT", "/api/v1/roles").Body
	if body["key"] != "editor" || len(body["allow"].([]any)) != 2 || body["name"] != "Editor" {
		t.Errorf("realm role body = %v", body)
	}
	if attr(st, "id") != "editor" || attr(st, "org") != nil {
		t.Errorf("state = %v", goValue(st))
	}
	if k.planChanged("forgebase_role", st, cfg) {
		t.Error("plan after apply is not empty")
	}
	// Creating over an existing role is refused (PUT would silently replace it).
	if e := k.createErr("forgebase_role", map[string]any{"key": "editor"}); !strings.Contains(e, "Role already exists") {
		t.Errorf("adopt guard: %q", e)
	}
	// Update in place; removed sets are sent empty.
	st2, _ := k.update("forgebase_role", st, map[string]any{"key": "editor", "name": "Editor", "allow": []any{"records:*"}})
	if got := m.last("PUT", "/api/v1/roles").Body; len(got["allow"].([]any)) != 1 || len(got["deny"].([]any)) != 0 {
		t.Errorf("update body = %v", got)
	}
	if _, replace := k.update("forgebase_role", st2, map[string]any{"key": "other", "allow": []any{"records:*"}}); len(replace) == 0 {
		t.Error("key change must replace")
	}

	// Organization role.
	ocfg := map[string]any{"key": "billing", "org": "org_1", "allow": []any{"billing:read"}}
	ost := k.create("forgebase_role", ocfg)
	if attr(ost, "id") != "org_1/billing" || attr(ost, "org") != "org_1" {
		t.Errorf("org role state = %v", goValue(ost))
	}
	if len(m.requests("PUT", "/api/v1/orgs/org_1/roles")) != 1 {
		t.Errorf("org role path: %v", m.summary())
	}
	if _, replace := k.update("forgebase_role", ost, map[string]any{"key": "billing", "org": "org_2", "allow": []any{"billing:read"}}); len(replace) == 0 {
		t.Error("org change must replace")
	}
	imp, d := k.importState("forgebase_role", "org_1/billing")
	k.failOn(d)
	if attr(imp, "org") != "org_1" || attr(imp, "key") != "billing" {
		t.Errorf("org import = %v", goValue(imp))
	}
	impRealm, d := k.importState("forgebase_role", "editor")
	k.failOn(d)
	if attr(impRealm, "org") != nil || attr(impRealm, "name") != "Editor" {
		t.Errorf("realm import = %v", goValue(impRealm))
	}

	if e := errText(k.destroy("forgebase_role", ost)); e != "" {
		t.Fatal(e)
	}
	gone, d := k.read("forgebase_role", ost)
	k.failOn(d)
	if !gone.IsNull() {
		t.Error("deleted org role must leave state")
	}
	if e := errText(k.destroy("forgebase_role", st2)); e != "" {
		t.Fatal(e)
	}
	if len(m.requests("DELETE", "/api/v1/roles/editor")) != 1 {
		t.Errorf("realm delete: %v", m.summary())
	}
	if e := errText(k.validate("forgebase_role", map[string]any{"key": "Bad Key"})); !strings.Contains(e, "must be 1-64") {
		t.Errorf("key validation: %q", e)
	}
}

func TestOrgAndProject(t *testing.T) {
	m := newMock(t)
	var mu sync.Mutex
	orgs := map[string]map[string]any{}
	projects := map[string]map[string]any{}
	n := 0
	m.on("POST /api/v1/orgs", func(_ *http.Request, b map[string]any) (int, any) {
		mu.Lock()
		defer mu.Unlock()
		n++
		id := fmt.Sprintf("org%d", n)
		o := map[string]any{"id": id, "realm_id": "default", "slug": b["slug"], "name": b["name"]}
		orgs[id] = o
		return 201, o
	})
	m.on("GET /api/v1/orgs/{o}", func(r *http.Request, _ map[string]any) (int, any) {
		mu.Lock()
		defer mu.Unlock()
		if o, ok := orgs[r.PathValue("o")]; ok {
			return 200, o
		}
		return 404, apiErr("not_found", "organization not found")
	})
	m.on("GET /api/v1/orgs", func(*http.Request, map[string]any) (int, any) {
		mu.Lock()
		defer mu.Unlock()
		out := []any{}
		for _, o := range orgs {
			out = append(out, o)
		}
		return 200, out
	})
	m.on("PATCH /api/v1/orgs/{o}", func(r *http.Request, b map[string]any) (int, any) {
		mu.Lock()
		defer mu.Unlock()
		o, ok := orgs[r.PathValue("o")]
		if !ok {
			return 404, apiErr("not_found", "organization not found")
		}
		o["name"] = b["name"]
		return 200, o
	})
	m.on("DELETE /api/v1/orgs/{o}", func(r *http.Request, _ map[string]any) (int, any) {
		mu.Lock()
		defer mu.Unlock()
		delete(orgs, r.PathValue("o"))
		return 204, nil
	})
	newProject := func(org any, b map[string]any) map[string]any {
		n++
		id := fmt.Sprintf("prj%d", n)
		p := map[string]any{"id": id, "realm_id": "default", "org_id": org, "slug": b["slug"], "name": b["name"],
			"environments": []any{map[string]any{"id": id + "-dev", "name": "development"}, map[string]any{"id": id + "-prod", "name": "production"}}}
		projects[id] = p
		return p
	}
	m.on("POST /api/v1/orgs/{o}/projects", func(r *http.Request, b map[string]any) (int, any) {
		mu.Lock()
		defer mu.Unlock()
		p := newProject(r.PathValue("o"), b)
		out := copyMap(p)
		delete(out, "environments") // the create answer carries no environments
		return 201, out
	})
	m.on("POST /api/v1/projects", func(_ *http.Request, b map[string]any) (int, any) {
		mu.Lock()
		defer mu.Unlock()
		return 201, newProject(nil, b)
	})
	m.on("GET /api/v1/projects/{p}", func(r *http.Request, _ map[string]any) (int, any) {
		mu.Lock()
		defer mu.Unlock()
		if p, ok := projects[r.PathValue("p")]; ok {
			return 200, p
		}
		return 404, apiErr("not_found", "project not found")
	})
	m.on("DELETE /api/v1/projects/{p}", func(r *http.Request, _ map[string]any) (int, any) {
		mu.Lock()
		defer mu.Unlock()
		delete(projects, r.PathValue("p"))
		return 204, nil
	})
	k := newKit(t, m)

	org := k.create("forgebase_org", map[string]any{"slug": "acme", "name": "Acme"})
	orgID := attr(org, "id").(string)
	if attr(org, "realm_id") != "default" {
		t.Errorf("org = %v", goValue(org))
	}
	org2, _ := k.update("forgebase_org", org, map[string]any{"slug": "acme", "name": "Acme Inc"})
	if m.last("PATCH", "/api/v1/orgs/"+orgID).Body["name"] != "Acme Inc" || attr(org2, "name") != "Acme Inc" {
		t.Errorf("rename: %v", goValue(org2))
	}
	if _, replace := k.update("forgebase_org", org2, map[string]any{"slug": "acme2", "name": "Acme Inc"}); len(replace) == 0 {
		t.Error("slug change must replace")
	}
	ds, d := k.dataSource("forgebase_org", map[string]any{"slug": "acme"})
	k.failOn(d)
	if attr(ds, "id") != orgID {
		t.Errorf("org data source = %v", goValue(ds))
	}
	if _, d := k.dataSource("forgebase_org", map[string]any{"slug": "nope"}); !strings.Contains(errText(d), "Organization not found") {
		t.Errorf("missing org: %q", errText(d))
	}
	if e := errText(k.validate("forgebase_org", map[string]any{"slug": "Has Space", "name": "x"})); !strings.Contains(e, "must be 1-64") {
		t.Errorf("slug validation: %q", e)
	}

	// Project in the org: environments are filled from the project read.
	pst := k.create("forgebase_project", map[string]any{"slug": "web", "name": "Web", "org": orgID})
	if len(m.requests("POST", "/api/v1/orgs/"+orgID+"/projects")) != 1 {
		t.Errorf("org project path: %v", m.summary())
	}
	if attr(pst, "environments.1.name") != "production" || attr(pst, "org") != orgID {
		t.Errorf("project = %v", goValue(pst))
	}
	if k.planChanged("forgebase_project", pst, map[string]any{"slug": "web", "name": "Web", "org": orgID}) {
		t.Error("plan after apply is not empty")
	}
	if _, replace := k.update("forgebase_project", pst, map[string]any{"slug": "web", "name": "Renamed", "org": orgID}); len(replace) == 0 {
		t.Error("project name change must replace (no update API)")
	}
	personal := k.create("forgebase_project", map[string]any{"slug": "mine", "name": "Mine"})
	if attr(personal, "org") != nil {
		t.Errorf("personal project org = %v", attr(personal, "org"))
	}
	pds, d := k.dataSource("forgebase_project", map[string]any{"id": attr(pst, "id")})
	k.failOn(d)
	if attr(pds, "environments.0.name") != "development" {
		t.Errorf("project data source = %v", goValue(pds))
	}
	imp, d := k.importState("forgebase_project", attr(pst, "id").(string))
	k.failOn(d)
	if attr(imp, "slug") != "web" {
		t.Errorf("import = %v", goValue(imp))
	}
	if e := errText(k.destroy("forgebase_project", pst)); e != "" {
		t.Fatal(e)
	}
	gone, _ := k.read("forgebase_project", pst)
	if !gone.IsNull() {
		t.Error("deleted project must leave state")
	}
	if e := errText(k.destroy("forgebase_org", org2)); e != "" {
		t.Fatal(e)
	}
	gone, _ = k.read("forgebase_org", org2)
	if !gone.IsNull() {
		t.Error("deleted org must leave state")
	}
	impOrg, d := k.importState("forgebase_org", "ghost")
	_ = d
	if !impOrg.IsNull() {
		t.Error("importing a missing org yields no state")
	}
}

func TestOrgNullAnswerMeansGone(t *testing.T) {
	m := newMock(t)
	// The server answers `null` for an unknown id on some paths; that means gone.
	m.on("GET /api/v1/orgs/{o}", func(*http.Request, map[string]any) (int, any) { return 200, json.RawMessage("null") })
	m.on("GET /api/v1/projects/{p}", func(*http.Request, map[string]any) (int, any) { return 200, json.RawMessage("null") })
	k := newKit(t, m)
	for _, res := range []string{"forgebase_org", "forgebase_project"} {
		st, d := k.importState(res, "ghost")
		k.failOn(d)
		if !st.IsNull() {
			t.Errorf("%s: a null answer must leave no state: %v", res, goValue(st))
		}
	}
}
