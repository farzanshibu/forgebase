package provider_test

import (
	"fmt"
	"net/http"
	"strings"
	"sync"
	"testing"
)

type fakeQueue struct {
	mu     sync.Mutex
	topics map[string]map[string]any
	subs   map[string]map[string]any
	n      int
}

func newFakeQueue(m *mockServer) *fakeQueue {
	f := &fakeQueue{topics: map[string]map[string]any{}, subs: map[string]map[string]any{}}
	m.on("POST /api/v1/queues/topics", func(_ *http.Request, b map[string]any) (int, any) {
		f.mu.Lock()
		defer f.mu.Unlock()
		n := b["name"].(string)
		if _, ok := f.topics[n]; ok {
			return 409, apiErr("topic_exists", "topic exists")
		}
		t := map[string]any{"name": n, "retention_ms": orDefault(b["retention_ms"], float64(604800000)), "retention_bytes": nil,
			"max_message_bytes": orDefault(b["max_message_bytes"], float64(262144)), "dedup_window_ms": orDefault(b["dedup_window_ms"], float64(300000)),
			"labels": orDefault(b["labels"], map[string]any{}), "schema": b["schema"], "source": b["source"],
			"created_by": "admin:1", "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z", "published_total": 0, "published_bytes": 0}
		if v, ok := b["retention_bytes"].(float64); ok && v > 0 {
			t["retention_bytes"] = v
		}
		if src, ok := t["source"].(map[string]any); ok && src["ops"] == nil {
			src["ops"] = []any{"create", "update", "delete"}
		}
		f.topics[n] = t
		return 201, t
	})
	m.on("GET /api/v1/queues/topics/{t}", func(r *http.Request, _ map[string]any) (int, any) {
		f.mu.Lock()
		defer f.mu.Unlock()
		if t, ok := f.topics[r.PathValue("t")]; ok {
			return 200, t
		}
		return 404, apiErr("topic_not_found", "no such topic")
	})
	m.on("PATCH /api/v1/queues/topics/{t}", func(r *http.Request, b map[string]any) (int, any) {
		f.mu.Lock()
		defer f.mu.Unlock()
		t, ok := f.topics[r.PathValue("t")]
		if !ok {
			return 404, apiErr("topic_not_found", "no such topic")
		}
		for _, k := range []string{"retention_ms", "max_message_bytes", "dedup_window_ms", "labels"} {
			if b[k] != nil {
				t[k] = b[k]
			}
		}
		if v, ok := b["retention_bytes"].(float64); ok {
			if v == 0 {
				t["retention_bytes"] = nil
			} else {
				t["retention_bytes"] = v
			}
		}
		if b["clear_schema"] == true {
			t["schema"] = nil
		} else if b["schema"] != nil {
			t["schema"] = b["schema"]
		}
		t["updated_at"] = "2026-02-02T00:00:00Z"
		return 200, t
	})
	m.on("DELETE /api/v1/queues/topics/{t}", func(r *http.Request, _ map[string]any) (int, any) {
		f.mu.Lock()
		defer f.mu.Unlock()
		n := r.PathValue("t")
		if _, ok := f.topics[n]; !ok {
			return 404, apiErr("topic_not_found", "no such topic")
		}
		for _, s := range f.subs {
			if s["topic"] == n && r.URL.Query().Get("force") != "true" {
				return 409, apiErr("topic_has_subscriptions", "topic has subscriptions")
			}
		}
		delete(f.topics, n)
		return 204, nil
	})
	m.on("POST /api/v1/queues/subscriptions", func(_ *http.Request, b map[string]any) (int, any) {
		f.mu.Lock()
		defer f.mu.Unlock()
		n := b["name"].(string)
		if _, ok := f.subs[n]; ok {
			return 409, apiErr("subscription_exists", "subscription exists")
		}
		s := map[string]any{"name": n, "topic": b["topic"], "ack_deadline_ms": orDefault(b["ack_deadline_ms"], float64(10000)),
			"max_delivery_attempts": b["max_delivery_attempts"], "backoff_base_ms": orDefault(b["backoff_base_ms"], float64(1000)),
			"backoff_max_ms": orDefault(b["backoff_max_ms"], float64(600000)), "dead_letter_topic": b["dead_letter_topic"],
			"ordering": orDefault(b["ordering"], false), "filter": b["filter"], "labels": orDefault(b["labels"], map[string]any{}),
			"push": b["push"], "created_by": "admin:1", "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z"}
		if p, ok := b["push"].(map[string]any); ok && p["kind"] == "webhook" {
			f.n++
			s["push_secret"] = fmt.Sprintf("whsec_%d", f.n)
		}
		f.subs[n] = s
		out := copyMap(s)
		return 201, out
	})
	m.on("GET /api/v1/queues/subscriptions/{s}", func(r *http.Request, _ map[string]any) (int, any) {
		f.mu.Lock()
		defer f.mu.Unlock()
		s, ok := f.subs[r.PathValue("s")]
		if !ok {
			return 404, apiErr("subscription_not_found", "no such subscription")
		}
		out := copyMap(s)
		delete(out, "push_secret") // never shown again
		return 200, out
	})
	m.on("PATCH /api/v1/queues/subscriptions/{s}", func(r *http.Request, b map[string]any) (int, any) {
		f.mu.Lock()
		defer f.mu.Unlock()
		s, ok := f.subs[r.PathValue("s")]
		if !ok {
			return 404, apiErr("subscription_not_found", "no such subscription")
		}
		for _, k := range []string{"ack_deadline_ms", "backoff_base_ms", "backoff_max_ms", "labels"} {
			if b[k] != nil {
				s[k] = b[k]
			}
		}
		if v, ok := b["max_delivery_attempts"].(float64); ok {
			if v == 0 {
				s["max_delivery_attempts"] = nil
			} else {
				s["max_delivery_attempts"] = v
			}
		}
		if b["clear_dead_letter_topic"] == true {
			s["dead_letter_topic"] = nil
		} else if b["dead_letter_topic"] != nil {
			s["dead_letter_topic"] = b["dead_letter_topic"]
		}
		out := copyMap(s)
		delete(out, "push_secret")
		if b["clear_push"] == true {
			s["push"] = nil
			delete(s, "push_secret")
			out["push"] = nil
		} else if p, ok := b["push"].(map[string]any); ok {
			s["push"], out["push"] = p, p
			delete(s, "push_secret")
			if p["kind"] == "webhook" {
				f.n++
				s["push_secret"] = fmt.Sprintf("whsec_%d", f.n)
				out["push_secret"] = s["push_secret"]
			}
		}
		out["updated_at"] = "2026-02-02T00:00:00Z"
		return 200, out
	})
	m.on("DELETE /api/v1/queues/subscriptions/{s}", func(r *http.Request, _ map[string]any) (int, any) {
		f.mu.Lock()
		defer f.mu.Unlock()
		if _, ok := f.subs[r.PathValue("s")]; !ok {
			return 404, apiErr("subscription_not_found", "no such subscription")
		}
		delete(f.subs, r.PathValue("s"))
		return 204, nil
	})
	return f
}

func copyMap(in map[string]any) map[string]any {
	out := make(map[string]any, len(in))
	for k, v := range in {
		out[k] = v
	}
	return out
}

func TestQueueTopicLifecycle(t *testing.T) {
	m := newMock(t)
	fake := newFakeQueue(m)
	k := newKit(t, m)
	cfg := map[string]any{
		"name": "orders", "retention_bytes": 1048576, "labels": map[string]any{"team": "billing"},
		"schema": `{"type":"object","required":["id"]}`,
	}
	st := k.create("forgebase_queue_topic", cfg)
	body := m.last("POST", "/api/v1/queues/topics").Body
	if body["name"] != "orders" || body["retention_bytes"] != float64(1048576) || body["labels"].(map[string]any)["team"] != "billing" {
		t.Errorf("create body = %v", body)
	}
	if _, set := body["retention_ms"]; set {
		t.Errorf("unset retention must be left to the server: %v", body)
	}
	if attr(st, "retention_ms") != float64(604800000) || attr(st, "max_message_bytes") != float64(262144) || attr(st, "created_by") != "admin:1" {
		t.Errorf("computed defaults = %v", goValue(st))
	}
	if k.planChanged("forgebase_queue_topic", st, cfg) {
		t.Error("plan after apply is not empty")
	}

	// Update: change retention, drop the cap, the schema and a label.
	cfg2 := map[string]any{"name": "orders", "retention_ms": 3600000, "labels": map[string]any{}}
	st2, _ := k.update("forgebase_queue_topic", st, cfg2)
	patch := m.last("PATCH", "/api/v1/queues/topics/orders").Body
	if patch["retention_ms"] != float64(3600000) || patch["retention_bytes"] != float64(0) || patch["clear_schema"] != true {
		t.Errorf("patch = %v", patch)
	}
	if attr(st2, "retention_bytes") != nil || attr(st2, "schema") != nil || attr(st2, "retention_ms") != float64(3600000) {
		t.Errorf("state = %v", goValue(st2))
	}
	if k.planChanged("forgebase_queue_topic", st2, cfg2) {
		t.Error("plan after update is not empty")
	}

	if _, replace := k.update("forgebase_queue_topic", st2, map[string]any{"name": "orders2"}); len(replace) == 0 {
		t.Error("rename must replace")
	}

	// Deleting a topic with subscriptions needs force_destroy.
	k.create("forgebase_queue_subscription", map[string]any{"name": "billing-worker", "topic": "orders"})
	if e := errText(k.destroy("forgebase_queue_topic", st2)); !strings.Contains(e, "topic_has_subscriptions") {
		t.Errorf("delete: %q", e)
	}
	cfg3 := map[string]any{"name": "orders", "retention_ms": 3600000, "labels": map[string]any{}, "force_destroy": true}
	st3, _ := k.update("forgebase_queue_topic", st2, cfg3)
	if e := errText(k.destroy("forgebase_queue_topic", st3)); e != "" {
		t.Fatal(e)
	}
	if q := m.last("DELETE", "/api/v1/queues/topics/orders").Query; q != "force=true" {
		t.Errorf("query = %q", q)
	}
	fake.mu.Lock()
	fake.topics = map[string]map[string]any{}
	fake.mu.Unlock()
	gone, d := k.read("forgebase_queue_topic", st)
	k.failOn(d)
	if !gone.IsNull() {
		t.Error("missing topic must leave state")
	}
}

func TestQueueTopicSourceAndValidation(t *testing.T) {
	m := newMock(t)
	newFakeQueue(m)
	k := newKit(t, m)
	st := k.create("forgebase_queue_topic", map[string]any{"name": "changes", "source": map[string]any{"collection": "posts"}})
	if got := m.last("POST", "/api/v1/queues/topics").Body["source"].(map[string]any); got["collection"] != "posts" {
		t.Errorf("source body = %v", got)
	}
	if attr(st, "source.ops.0") != "create" {
		t.Errorf("server default ops: %v", goValue(st))
	}
	if k.planChanged("forgebase_queue_topic", st, map[string]any{"name": "changes", "source": map[string]any{"collection": "posts"}}) {
		t.Error("plan after apply is not empty")
	}
	if _, replace := k.update("forgebase_queue_topic", st, map[string]any{"name": "changes", "source": map[string]any{"collection": "other"}}); len(replace) == 0 {
		t.Error("changing the source must replace")
	}
	for name, c := range map[string]struct {
		cfg  map[string]any
		want string
	}{
		"name":      {map[string]any{"name": "-bad"}, "must be 1-128"},
		"retention": {map[string]any{"name": "a", "retention_ms": 10}, "at least 60000"},
		"dedup":     {map[string]any{"name": "a", "dedup_window_ms": 5}, "at least 1000"},
	} {
		if e := errText(k.validate("forgebase_queue_topic", c.cfg)); !strings.Contains(e, c.want) {
			t.Errorf("%s: want %q in %q", name, c.want, e)
		}
	}
	if e := k.createErr("forgebase_queue_topic", map[string]any{"name": "changes"}); !strings.Contains(e, "topic_exists") {
		t.Errorf("409: %q", e)
	}
	ds, d := k.dataSource("forgebase_queue_topic", map[string]any{"name": "changes"})
	k.failOn(d)
	if attr(ds, "source.collection") != "posts" {
		t.Errorf("data source = %v", goValue(ds))
	}
	imp, d := k.importState("forgebase_queue_topic", "changes")
	k.failOn(d)
	sameState(t, "topic", st, imp)
	if attr(imp, "name") != "changes" {
		t.Errorf("import = %v", goValue(imp))
	}
}

func TestQueueSubscriptionPullAndUpdate(t *testing.T) {
	m := newMock(t)
	newFakeQueue(m)
	k := newKit(t, m)
	k.create("forgebase_queue_topic", map[string]any{"name": "orders"})
	k.create("forgebase_queue_topic", map[string]any{"name": "orders-dead"})
	cfg := map[string]any{"name": "worker", "topic": "orders", "max_delivery_attempts": 5, "dead_letter_topic": "orders-dead", "start": "earliest"}
	st := k.create("forgebase_queue_subscription", cfg)
	body := m.last("POST", "/api/v1/queues/subscriptions").Body
	if body["start"] != "earliest" || body["max_delivery_attempts"] != float64(5) || body["dead_letter_topic"] != "orders-dead" || body["ordering"] != false {
		t.Errorf("create body = %v", body)
	}
	if _, has := body["push"]; has {
		t.Errorf("a pull subscription has no push block: %v", body)
	}
	if attr(st, "push_secret") != nil || attr(st, "ack_deadline_ms") != float64(10000) {
		t.Errorf("state = %v", goValue(st))
	}
	if k.planChanged("forgebase_queue_subscription", st, cfg) {
		t.Error("plan after apply is not empty")
	}

	cfg2 := map[string]any{"name": "worker", "topic": "orders", "ack_deadline_ms": 30000, "start": "earliest"}
	st2, _ := k.update("forgebase_queue_subscription", st, cfg2)
	patch := m.last("PATCH", "/api/v1/queues/subscriptions/worker").Body
	if patch["ack_deadline_ms"] != float64(30000) || patch["max_delivery_attempts"] != float64(0) || patch["clear_dead_letter_topic"] != true {
		t.Errorf("patch = %v", patch)
	}
	if _, touched := patch["push"]; touched {
		t.Errorf("push must not be sent when unchanged: %v", patch)
	}
	if attr(st2, "max_delivery_attempts") != nil || attr(st2, "dead_letter_topic") != nil {
		t.Errorf("state = %v", goValue(st2))
	}

	// Immutable attributes replace.
	for name, c := range map[string]map[string]any{
		"topic":    {"name": "worker", "topic": "orders-dead", "start": "earliest"},
		"ordering": {"name": "worker", "topic": "orders", "ordering": true, "start": "earliest"},
		"filter":   {"name": "worker", "topic": "orders", "filter": `{"attributes":{"kind":"a"}}`, "start": "earliest"},
	} {
		if _, replace := k.update("forgebase_queue_subscription", st2, c); len(replace) == 0 {
			t.Errorf("changing %s must replace", name)
		}
	}
}

func TestQueueSubscriptionWebhookSecret(t *testing.T) {
	m := newMock(t)
	newFakeQueue(m)
	k := newKit(t, m)
	k.create("forgebase_queue_topic", map[string]any{"name": "orders"})
	cfg := map[string]any{"name": "hook", "topic": "orders", "push": map[string]any{"kind": "webhook", "endpoint": "https://example.com/hook"}}
	st := k.create("forgebase_queue_subscription", cfg)
	secret := attr(st, "push_secret")
	if secret != "whsec_1" {
		t.Fatalf("secret returned once at create must be kept: %v", secret)
	}
	// The sensitive flag is set in the schema.
	for _, a := range k.schemas["forgebase_queue_subscription"].Block.Attributes {
		if a.Name == "push_secret" && !a.Sensitive {
			t.Error("push_secret must be sensitive")
		}
	}

	// A refresh does not lose the secret (the server never shows it again).
	st2, d := k.read("forgebase_queue_subscription", st)
	k.failOn(d)
	if attr(st2, "push_secret") != secret {
		t.Errorf("refresh dropped the secret: %v", attr(st2, "push_secret"))
	}
	if k.planChanged("forgebase_queue_subscription", st2, cfg) {
		t.Error("plan after refresh is not empty")
	}

	// An unrelated change keeps the secret and does not touch push.
	cfgLabels := map[string]any{"name": "hook", "topic": "orders", "labels": map[string]any{"a": "b"}, "push": cfg["push"]}
	st3, _ := k.update("forgebase_queue_subscription", st2, cfgLabels)
	if _, touched := m.last("PATCH", "/api/v1/queues/subscriptions/hook").Body["push"]; touched {
		t.Error("push re-sent although unchanged (it would rotate the secret)")
	}
	if attr(st3, "push_secret") != secret {
		t.Errorf("secret changed on unrelated update: %v", attr(st3, "push_secret"))
	}

	// Changing the endpoint rotates the secret: the new one is captured.
	cfgNew := map[string]any{"name": "hook", "topic": "orders", "labels": map[string]any{"a": "b"}, "push": map[string]any{"kind": "webhook", "endpoint": "https://example.com/v2"}}
	p := k.plan("forgebase_queue_subscription", st3, cfgNew)
	k.failOn(p.Diags)
	if _, isUnknown := goValue(p.Planned).(map[string]any)["push_secret"].(unknown); !isUnknown {
		t.Errorf("planned secret should be unknown when the webhook changes: %v", goValue(p.Planned).(map[string]any)["push_secret"])
	}
	st4, d := k.apply("forgebase_queue_subscription", st3, p)
	k.failOn(d)
	if got := attr(st4, "push_secret"); got != "whsec_2" {
		t.Errorf("rotated secret = %v", got)
	}

	// Switching to a function: no secret remains.
	cfgFn := map[string]any{"name": "hook", "topic": "orders", "labels": map[string]any{"a": "b"}, "push": map[string]any{"kind": "function", "endpoint": "notify"}}
	st5, _ := k.update("forgebase_queue_subscription", st4, cfgFn)
	if attr(st5, "push_secret") != nil {
		t.Errorf("function push has no secret: %v", attr(st5, "push_secret"))
	}
	// Removing push clears it.
	cfgPull := map[string]any{"name": "hook", "topic": "orders", "labels": map[string]any{"a": "b"}}
	st6, _ := k.update("forgebase_queue_subscription", st5, cfgPull)
	if m.last("PATCH", "/api/v1/queues/subscriptions/hook").Body["clear_push"] != true || attr(st6, "push") != nil {
		t.Errorf("clear_push: %v", goValue(st6))
	}
}

func TestQueueSubscriptionErrorsAndValidation(t *testing.T) {
	m := newMock(t)
	fake := newFakeQueue(m)
	k := newKit(t, m)
	k.create("forgebase_queue_topic", map[string]any{"name": "orders"})
	st := k.create("forgebase_queue_subscription", map[string]any{"name": "w", "topic": "orders", "start": "latest"})
	if e := k.createErr("forgebase_queue_subscription", map[string]any{"name": "w", "topic": "orders"}); !strings.Contains(e, "subscription_exists") {
		t.Errorf("409: %q", e)
	}
	imp, d := k.importState("forgebase_queue_subscription", "w")
	k.failOn(d)
	if attr(imp, "topic") != "orders" {
		t.Errorf("import = %v", goValue(imp))
	}
	sameState(t, "subscription", st, imp, "start") // start is not stored by the server
	if e := errText(k.destroy("forgebase_queue_subscription", st)); e != "" {
		t.Fatal(e)
	}
	fake.mu.Lock()
	fake.subs = map[string]map[string]any{}
	fake.mu.Unlock()
	gone, _ := k.read("forgebase_queue_subscription", st)
	if !gone.IsNull() {
		t.Error("missing subscription must leave state")
	}
	for name, c := range map[string]struct {
		cfg  map[string]any
		want string
	}{
		"start":    {map[string]any{"name": "a", "topic": "t", "start": "yesterday"}, "Invalid start"},
		"dlt":      {map[string]any{"name": "a", "topic": "t", "dead_letter_topic": "t"}, "must differ"},
		"backoff":  {map[string]any{"name": "a", "topic": "t", "backoff_base_ms": 5000, "backoff_max_ms": 1000}, "at least backoff_base_ms"},
		"push":     {map[string]any{"name": "a", "topic": "t", "push": map[string]any{"kind": "email", "endpoint": "x"}}, "kind value must be one of"},
		"url":      {map[string]any{"name": "a", "topic": "t", "push": map[string]any{"kind": "webhook", "endpoint": "example.com"}}, "Invalid webhook URL"},
		"attempts": {map[string]any{"name": "a", "topic": "t", "max_delivery_attempts": 500}, "between 1 and 100"},
		"filter":   {map[string]any{"name": "a", "topic": "t", "filter": `"x"`}, "must be a JSON object"},
	} {
		if e := errText(k.validate("forgebase_queue_subscription", c.cfg)); !strings.Contains(e, c.want) {
			t.Errorf("%s: want %q in %q", name, c.want, e)
		}
	}
	if e := errText(k.validate("forgebase_queue_subscription", map[string]any{"name": "a", "topic": "t", "start": "2026-01-02T03:04:05Z"})); e != "" {
		t.Errorf("RFC 3339 start rejected: %s", e)
	}
}
