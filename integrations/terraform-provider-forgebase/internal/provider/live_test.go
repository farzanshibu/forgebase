package provider_test

import (
	"crypto/rand"
	"encoding/hex"
	"os"
	"strings"
	"testing"

	"github.com/hashicorp/terraform-plugin-go/tftypes"
)

// Live tests drive the provider through the Terraform plugin protocol
// (validate, plan, apply, refresh, import, destroy) against a real ForgeBase
// server, without the Terraform CLI. They are skipped unless FORGEBASE_LIVE=1:
//
//	FORGEBASE_LIVE=1 FORGEBASE_URL=http://127.0.0.1:8090 FORGEBASE_TOKEN=<admin key> \
//	  go test ./internal/provider -run TestLive -v
//
// Use a throwaway server: the tests create and delete real objects.

func liveKit(t *testing.T) *kit {
	t.Helper()
	if os.Getenv("FORGEBASE_LIVE") != "1" {
		t.Skip("set FORGEBASE_LIVE=1 (and FORGEBASE_URL, FORGEBASE_TOKEN) to run against a real server")
	}
	url, token := os.Getenv("FORGEBASE_URL"), os.Getenv("FORGEBASE_TOKEN")
	if url == "" || token == "" {
		t.Fatal("FORGEBASE_URL and FORGEBASE_TOKEN must be set")
	}
	k := newKit(t, nil)
	k.failOn(k.configure(map[string]any{"url": url, "token": token}))
	return k
}

func liveName(prefix string) string {
	b := make([]byte, 4)
	_, _ = rand.Read(b)
	return prefix + hex.EncodeToString(b)
}

// cycle runs create → no-diff plan → refresh → import → destroy and returns the created state.
func (k *kit) liveCycle(res string, cfg map[string]any, importID string, update map[string]any) {
	k.t.Helper()
	st := k.create(res, cfg)
	cleaned := false
	defer func() {
		if !cleaned {
			k.destroy(res, st)
		}
	}()
	if k.planChanged(res, st, cfg) {
		p := k.plan(res, st, cfg)
		k.t.Errorf("%s: plan after apply is not empty:\n  state:   %v\n  planned: %v", res, goValue(st), goValue(p.Planned))
	}
	st2, d := k.read(res, st)
	k.failOn(d)
	if st2.IsNull() {
		k.t.Fatalf("%s: refresh lost the resource", res)
	}
	if k.planChanged(res, st2, cfg) {
		k.t.Errorf("%s: plan after refresh is not empty", res)
	}
	if importID != "" {
		imp, d := k.importState(res, importID)
		k.failOn(d)
		if imp.IsNull() {
			k.t.Errorf("%s: import %q found nothing", res, importID)
		}
	}
	if update != nil {
		st3, replace := k.update(res, st2, update)
		if len(replace) > 0 {
			k.t.Fatalf("%s: update wants replacement: %v", res, replace)
		}
		if k.planChanged(res, st3, update) {
			k.t.Errorf("%s: plan after update is not empty", res)
		}
		st2 = st3
	}
	cleaned = true
	k.failOn(k.destroy(res, st2))
	gone, d := k.read(res, st2)
	k.failOn(d)
	// Rules and ACLs belong to a collection or bucket that still exists: after
	// destroy they read back empty rather than 404.
	if !gone.IsNull() && res != "forgebase_collection_rules" && res != "forgebase_storage_bucket_acl" {
		k.t.Errorf("%s: still there after destroy", res)
	}
}

func TestLiveCollection(t *testing.T) {
	k := liveKit(t)
	name := liveName("tf_posts_")
	cfg := func(extra ...any) map[string]any {
		fields := []any{
			map[string]any{"name": "title", "type": "text", "required": true, "options": `{"max_length": 200}`},
			map[string]any{"name": "status", "type": "select", "options": `{"values": ["draft", "live"]}`, "default": `"draft"`},
			map[string]any{"name": "views", "type": "number", "options": `{"integer": true}`},
		}
		return map[string]any{"name": name, "fields": append(fields, extra...)}
	}
	k.liveCycle(collectionRes, cfg(), name, cfg(map[string]any{"name": "published", "type": "date"}))

	// Rename a field in place, keeping its data.
	st := k.create(collectionRes, cfg())
	defer k.destroy(collectionRes, st)
	renamed := cfg()
	f0 := renamed["fields"].([]any)[0].(map[string]any)
	f0["name"], f0["renamed_from"] = "headline", "title"
	st2, replace := k.update(collectionRes, st, renamed)
	if len(replace) > 0 || attr(st2, "fields.0.id") != attr(st, "fields.0.id") {
		t.Errorf("rename lost the field id: %v -> %v (%v)", attr(st, "fields.0.id"), attr(st2, "fields.0.id"), replace)
	}

	// Auth collection: system fields stay out of the way.
	auth := map[string]any{"name": liveName("tf_users_"), "type": "auth", "fields": []any{map[string]any{"name": "nick", "type": "text"}}}
	k.liveCycle(collectionRes, auth, "", map[string]any{"name": auth["name"], "type": "auth", "fields": []any{
		map[string]any{"name": "nick", "type": "text"}, map[string]any{"name": "bio", "type": "text"}}})
}

func TestLiveIndexAndRules(t *testing.T) {
	k := liveKit(t)
	coll := liveName("tf_idx_")
	cst := k.create(collectionRes, map[string]any{"name": coll, "fields": []any{map[string]any{"name": "title", "type": "text"}, map[string]any{"name": "views", "type": "number"}}})
	defer k.destroy(collectionRes, cst)

	name := liveName("tf_ix_")
	k.liveCycle("forgebase_index", map[string]any{"name": name, "collection": coll, "keys": []any{map[string]any{"path": "title"}, map[string]any{"path": "views", "desc": true}}}, name, nil)
	k.liveCycle("forgebase_index", map[string]any{"collection": coll, "unique": true, "keys": []any{map[string]any{"path": "title"}}}, "", nil)

	k.liveCycle("forgebase_collection_rules", map[string]any{
		"collection": coll, "list_rule": "", "view_rule": `@request.auth.id != ""`,
		"field_rules": map[string]any{"title": map[string]any{"write": `@request.auth.id != ""`}},
	}, coll, map[string]any{"collection": coll, "list_rule": `@request.auth.id != ""`})
}

func TestLiveStorage(t *testing.T) {
	k := liveKit(t)
	name := liveName("tf-bucket-")
	k.liveCycle("forgebase_storage_bucket", map[string]any{"name": name, "visibility": "private", "max_object_bytes": 1048576, "allowed_content_types": []any{"image/png", "image/jpeg"}, "force_destroy": true},
		name, map[string]any{"name": name, "visibility": "public", "versioning": true, "force_destroy": true})

	bst := k.create("forgebase_storage_bucket", map[string]any{"name": name + "-acl", "force_destroy": true})
	defer k.destroy("forgebase_storage_bucket", bst)
	k.liveCycle("forgebase_storage_bucket_acl", map[string]any{"bucket": name + "-acl", "entries": []any{
		map[string]any{"grantee": "authenticated", "permission": "read", "prefix": "shared/"},
		map[string]any{"grantee": "role:editors", "permission": "write"},
	}}, name+"-acl", map[string]any{"bucket": name + "-acl", "entries": []any{map[string]any{"grantee": "public", "permission": "read", "prefix": "pub/"}}})
}

func TestLiveQueues(t *testing.T) {
	k := liveKit(t)
	topic := liveName("tf-topic-")
	dead := topic + "-dead"
	dst := k.create("forgebase_queue_topic", map[string]any{"name": dead, "force_destroy": true})
	defer k.destroy("forgebase_queue_topic", dst)
	k.liveCycle("forgebase_queue_topic", map[string]any{"name": topic, "retention_bytes": 1048576, "labels": map[string]any{"team": "x"}, "schema": `{"type":"object"}`, "force_destroy": true},
		topic, map[string]any{"name": topic, "retention_ms": 7200000, "labels": map[string]any{"team": "y"}, "force_destroy": true})

	tst := k.create("forgebase_queue_topic", map[string]any{"name": topic, "force_destroy": true})
	defer k.destroy("forgebase_queue_topic", tst)
	sub := topic + "-sub"
	k.liveCycle("forgebase_queue_subscription", map[string]any{"name": sub, "topic": topic, "max_delivery_attempts": 4, "dead_letter_topic": dead, "start": "earliest"},
		sub, map[string]any{"name": sub, "topic": topic, "ack_deadline_ms": 20000, "start": "earliest"})

	// A function push target; webhooks need a public https URL, so use a function name.
	fn := liveName("tf-pushfn-")
	fst := k.create(fnRes, map[string]any{"name": fn, "source": `import { serve } from "forgebase"; serve(() => new Response("ok"));`, "language": "javascript"})
	defer k.destroy(fnRes, fst)
	k.liveCycle("forgebase_queue_subscription", map[string]any{"name": sub + "-push", "topic": topic, "push": map[string]any{"kind": "function", "endpoint": fn}},
		sub+"-push", nil)

	if hook := os.Getenv("FORGEBASE_LIVE_WEBHOOK_URL"); hook != "" {
		st := k.create("forgebase_queue_subscription", map[string]any{"name": sub + "-hook", "topic": topic, "push": map[string]any{"kind": "webhook", "endpoint": hook}})
		defer k.destroy("forgebase_queue_subscription", st)
		if attr(st, "push_secret") == nil {
			t.Error("webhook subscription returned no secret")
		}
		st2, d := k.read("forgebase_queue_subscription", st)
		k.failOn(d)
		if attr(st2, "push_secret") != attr(st, "push_secret") {
			t.Error("refresh lost the webhook secret")
		}
		cfg := map[string]any{"name": sub + "-hook", "topic": topic, "push": map[string]any{"kind": "webhook", "endpoint": hook}}
		if k.planChanged("forgebase_queue_subscription", st2, cfg) {
			t.Error("plan after apply is not empty")
		}
		// Changing the endpoint rotates the secret; the new one is captured.
		cfg["push"] = map[string]any{"kind": "webhook", "endpoint": hook + "?v=2"}
		st3, replace := k.update("forgebase_queue_subscription", st2, cfg)
		if len(replace) > 0 || attr(st3, "push_secret") == nil || attr(st3, "push_secret") == attr(st, "push_secret") {
			t.Errorf("rotation: replace=%v old=%v new=%v", replace, attr(st, "push_secret"), attr(st3, "push_secret"))
		}
		// An unrelated change keeps the secret.
		cfg["labels"] = map[string]any{"a": "b"}
		st4, _ := k.update("forgebase_queue_subscription", st3, cfg)
		if attr(st4, "push_secret") != attr(st3, "push_secret") {
			t.Error("unrelated update changed the secret")
		}
	}
}

func TestLiveFunction(t *testing.T) {
	k := liveKit(t)
	name := liveName("tf-fn-")
	src := func(v string) string {
		return `import { serve } from "forgebase"; serve(() => new Response("` + v + `"));`
	}
	cfg := func(v string) map[string]any {
		return map[string]any{"name": name, "language": "javascript", "source": src(v), "capabilities": []any{"log", "env"},
			"env": map[string]any{"MODE": "test"}, "secrets": map[string]any{"TOKEN": "s3cr3t"}, "description": "live test"}
	}
	st := k.create(fnRes, cfg("v1"))
	defer k.destroy(fnRes, st)
	if k.planChanged(fnRes, st, cfg("v1")) {
		t.Errorf("plan after apply is not empty: %v", goValue(k.plan(fnRes, st, cfg("v1")).Planned))
	}
	st2, _ := k.update(fnRes, st, cfg("v2"))
	if attr(st2, "active_version") != float64(2) {
		t.Errorf("redeploy: %v", attr(st2, "active_version"))
	}
	if k.planChanged(fnRes, st2, cfg("v2")) {
		t.Error("plan after update is not empty")
	}
	imp, d := k.importState(fnRes, name)
	k.failOn(d)
	if attr(imp, "content_sha256") != attr(st2, "content_sha256") {
		t.Error("import hash differs from state")
	}
	ds, d := k.dataSource("forgebase_function", map[string]any{"name": name})
	k.failOn(d)
	if attr(ds, "active_version") != float64(2) {
		t.Errorf("data source = %v", goValue(ds))
	}
}

func TestLiveFunctionKVRulesAndHooks(t *testing.T) {
	k := liveKit(t)
	coll := liveName("tf_hooked_")
	cst := k.create(collectionRes, map[string]any{"name": coll, "fields": []any{map[string]any{"name": "title", "type": "text"}}})
	defer k.destroy(collectionRes, cst)
	name := liveName("tf-hook-")
	cfg := map[string]any{"name": name, "language": "javascript", "source": `import { serve } from "forgebase"; serve(() => new Response("ok"));`,
		"capabilities": []any{"kv", "db.read"}, "access": "authenticated",
		"limits":   map[string]any{"timeout_ms": 2000},
		"kv_rules": []any{map[string]any{"namespace": "cache", "access": "read"}},
		"hooks":    []any{map[string]any{"collection": coll, "events": []any{"after_create", "before_update"}}}}
	k.liveCycle(fnRes, cfg, name, nil)
}

func TestLiveIdentity(t *testing.T) {
	k := liveKit(t)
	name := liveName("tf-sa-")
	sa := k.create("forgebase_service_account", map[string]any{"name": name, "description": "live"})
	defer k.destroy("forgebase_service_account", sa)
	id := attr(sa, "id").(string)
	k.liveCycle("forgebase_api_key", map[string]any{"service_account_id": id, "name": "ci", "scopes": []any{"collections:read", "storage:write"}, "expires_in_days": 30}, "", nil)
	key := k.create("forgebase_api_key", map[string]any{"service_account_id": id, "name": "ci2", "scopes": []any{"collections:read"}})
	defer k.destroy("forgebase_api_key", key)
	if s, _ := attr(key, "key").(string); !strings.HasPrefix(s, "fbk_") {
		t.Errorf("key = %v", attr(key, "key"))
	}
	imp, d := k.importState("forgebase_api_key", id+"/"+attr(key, "id").(string))
	k.failOn(d)
	if attr(imp, "key") != nil || attr(imp, "name") != "ci2" {
		t.Errorf("imported key = %v", goValue(imp))
	}
	ds, d := k.dataSource("forgebase_service_account", map[string]any{"name": name})
	k.failOn(d)
	if attr(ds, "id") != id {
		t.Errorf("data source = %v", goValue(ds))
	}

	cname := liveName("tf-app-")
	k.liveCycle("forgebase_oauth_client", map[string]any{"name": cname, "redirect_uris": []any{"https://app.example.com/cb"}, "access_token_ttl": 900}, "",
		map[string]any{"name": cname + "-v2", "redirect_uris": []any{"https://app.example.com/cb", "https://app.example.com/cb2"}, "access_token_ttl": 1800, "disabled": true})
	pub := k.create("forgebase_oauth_client", map[string]any{"name": cname + "-pub", "type": "public", "redirect_uris": []any{"https://spa.example.com/"}})
	defer k.destroy("forgebase_oauth_client", pub)
	if attr(pub, "client_secret") != nil {
		t.Error("public client has a secret")
	}

	role := liveName("tf-role-")
	k.liveCycle("forgebase_role", map[string]any{"key": role, "name": "Live", "allow": []any{"collections:read"}, "deny": []any{"collections:delete"}}, role,
		map[string]any{"key": role, "name": "Live", "allow": []any{"collections:*"}})
}

func TestLiveEntitlements(t *testing.T) {
	k := liveKit(t)
	fkey, pkey := liveName("tf_feature_"), liveName("tf_plan_")
	f := k.create("forgebase_feature", map[string]any{"key": fkey, "kind": "metered", "unit": "requests"})
	defer k.destroy("forgebase_feature", f)
	k.liveCycle("forgebase_feature", map[string]any{"key": liveName("tf_feature_"), "kind": "boolean"}, "", nil)
	k.liveCycle("forgebase_plan", map[string]any{"key": pkey, "name": "Live", "trial_days": 7, "features": []any{
		map[string]any{"feature": fkey, "limit": 1000}}}, pkey,
		map[string]any{"key": pkey, "name": "Live", "trial_days": 14, "features": []any{map[string]any{"feature": fkey, "limit": 5000}}})
}

func TestLiveTenancy(t *testing.T) {
	k := liveKit(t)
	slug := liveName("tf-org-")
	st := k.create("forgebase_org", map[string]any{"slug": slug, "name": "Live org"})
	defer k.destroy("forgebase_org", st)
	id := attr(st, "id").(string)
	st2, replace := k.update("forgebase_org", st, map[string]any{"slug": slug, "name": "Live org 2"})
	if len(replace) > 0 || attr(st2, "name") != "Live org 2" {
		t.Errorf("rename: %v %v", goValue(st2), replace)
	}
	k.liveCycle("forgebase_project", map[string]any{"slug": liveName("tf-prj-"), "name": "Web", "org": id}, "", nil)
	ds, d := k.dataSource("forgebase_org", map[string]any{"slug": slug})
	k.failOn(d)
	if attr(ds, "id") != id {
		t.Errorf("data source = %v", goValue(ds))
	}
}

var _ = tftypes.String
