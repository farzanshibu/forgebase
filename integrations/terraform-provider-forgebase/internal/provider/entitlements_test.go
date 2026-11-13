package provider_test

import (
	"net/http"
	"strings"
	"sync"
	"testing"
)

func TestFeatureAndPlan(t *testing.T) {
	m := newMock(t)
	var mu sync.Mutex
	features := map[string]map[string]any{}
	plans := map[string]map[string]any{}
	m.on("PUT /api/v1/features/{k}", func(r *http.Request, b map[string]any) (int, any) {
		mu.Lock()
		defer mu.Unlock()
		key := r.PathValue("k")
		f := map[string]any{"key": key, "name": orDefault(b["name"], key), "description": orDefault(b["description"], ""), "kind": b["kind"], "unit": orDefault(b["unit"], ""),
			"permissions": orDefault(b["permissions"], []any{}), "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-02T00:00:00Z"}
		features[key] = f
		return 200, f
	})
	m.on("GET /api/v1/features/{k}", func(r *http.Request, _ map[string]any) (int, any) {
		mu.Lock()
		defer mu.Unlock()
		if f, ok := features[r.PathValue("k")]; ok {
			return 200, f
		}
		return 404, apiErr("not_found", "feature not found")
	})
	m.on("DELETE /api/v1/features/{k}", func(r *http.Request, _ map[string]any) (int, any) {
		mu.Lock()
		defer mu.Unlock()
		delete(features, r.PathValue("k"))
		return 204, nil
	})
	m.on("PUT /api/v1/plans/{k}", func(r *http.Request, b map[string]any) (int, any) {
		mu.Lock()
		defer mu.Unlock()
		key := r.PathValue("k")
		feats := []any{}
		for _, x := range b["features"].([]any) {
			f := copyMap(x.(map[string]any))
			if f["period"] == nil {
				f["period"] = "month"
			}
			if f["unit"] == nil {
				f["unit"] = "requests"
			}
			if f["limit"] == nil {
				f["limit"] = nil
			}
			feats = append(feats, f)
		}
		// The server lists a plan's features sorted by key.
		for i := range feats {
			for j := i + 1; j < len(feats); j++ {
				if feats[j].(map[string]any)["feature"].(string) < feats[i].(map[string]any)["feature"].(string) {
					feats[i], feats[j] = feats[j], feats[i]
				}
			}
		}
		p := map[string]any{"key": key, "name": orDefault(b["name"], key), "description": b["description"], "is_default": b["is_default"], "active": b["active"],
			"trial_days": b["trial_days"], "metadata": orDefault(b["metadata"], map[string]any{}), "features": feats,
			"created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-02T00:00:00Z"}
		plans[key] = p
		return 200, p
	})
	m.on("GET /api/v1/plans/{k}", func(r *http.Request, _ map[string]any) (int, any) {
		mu.Lock()
		defer mu.Unlock()
		if p, ok := plans[r.PathValue("k")]; ok {
			return 200, p
		}
		return 404, apiErr("not_found", "plan not found")
	})
	m.on("DELETE /api/v1/plans/{k}", func(r *http.Request, _ map[string]any) (int, any) {
		mu.Lock()
		defer mu.Unlock()
		if _, ok := plans[r.PathValue("k")]; !ok {
			return 404, apiErr("not_found", "plan not found")
		}
		delete(plans, r.PathValue("k"))
		return 204, nil
	})
	k := newKit(t, m)

	fcfg := map[string]any{"key": "ai_generation", "kind": "metered", "unit": "requests", "permissions": []any{"ai:*"}}
	f := k.create("forgebase_feature", fcfg)
	if body := m.last("PUT", "/api/v1/features/ai_generation").Body; body["kind"] != "metered" || body["unit"] != "requests" {
		t.Errorf("feature body = %v", body)
	}
	if attr(f, "name") != "ai_generation" || k.planChanged("forgebase_feature", f, fcfg) {
		t.Errorf("feature state = %v", goValue(f))
	}
	if e := k.createErr("forgebase_feature", fcfg); !strings.Contains(e, "Feature already exists") {
		t.Errorf("adopt guard: %q", e)
	}
	f2, _ := k.update("forgebase_feature", f, map[string]any{"key": "ai_generation", "kind": "gauge", "name": "AI"})
	if attr(f2, "kind") != "gauge" || attr(f2, "name") != "AI" || attr(f2, "permissions") == nil {
		t.Errorf("feature update = %v", goValue(f2))
	}
	if _, replace := k.update("forgebase_feature", f2, map[string]any{"key": "other", "kind": "gauge"}); len(replace) == 0 {
		t.Error("key change must replace")
	}
	imp, d := k.importState("forgebase_feature", "ai_generation")
	k.failOn(d)
	if attr(imp, "kind") != "gauge" {
		t.Errorf("feature import = %v", goValue(imp))
	}

	pcfg := map[string]any{
		"key": "pro", "name": "Pro", "trial_days": 14, "is_default": false, "metadata": `{"tier": 2}`,
		"features": []any{
			map[string]any{"feature": "storage", "limit": 1000},
			map[string]any{"feature": "ai_generation", "limit": 500, "period": "month", "trial_limit": 10},
		},
	}
	p := k.create("forgebase_plan", pcfg)
	body := m.last("PUT", "/api/v1/plans/pro").Body
	feats := body["features"].([]any)
	if body["trial_days"] != float64(14) || body["active"] != true || len(feats) != 2 || feats[0].(map[string]any)["feature"] != "storage" || feats[0].(map[string]any)["enabled"] != true {
		t.Errorf("plan body = %v", body)
	}
	if attr(p, "features.0.feature") != "storage" || attr(p, "features.1.period") != "month" || attr(p, "features.0.period") != "month" {
		t.Errorf("plan state keeps the configured order and fills server defaults: %v", goValue(p))
	}
	if k.planChanged("forgebase_plan", p, pcfg) {
		t.Error("plan after apply is not empty (server sorts features; the provider must keep the configured order)")
	}
	p2, _ := k.update("forgebase_plan", p, map[string]any{"key": "pro", "name": "Pro", "trial_days": 7, "active": false, "metadata": `{"tier": 2}`,
		"features": []any{map[string]any{"feature": "storage", "limit": 2000}}})
	if attr(p2, "trial_days") != float64(7) || attr(p2, "active") != false || len(goValue(p2).(map[string]any)["features"].([]any)) != 1 {
		t.Errorf("plan update = %v", goValue(p2))
	}
	if e := k.createErr("forgebase_plan", pcfg); !strings.Contains(e, "Plan already exists") {
		t.Errorf("adopt guard: %q", e)
	}
	ds, d := k.dataSource("forgebase_plan", map[string]any{"key": "pro"})
	k.failOn(d)
	if attr(ds, "trial_days") != float64(7) || attr(ds, "features.0.limit") != float64(2000) {
		t.Errorf("plan data source = %v", goValue(ds))
	}
	if e := errText(k.destroy("forgebase_plan", p2)); e != "" {
		t.Fatal(e)
	}
	gone, d := k.read("forgebase_plan", p2)
	k.failOn(d)
	if !gone.IsNull() {
		t.Error("deleted plan must leave state")
	}
	if e := errText(k.destroy("forgebase_feature", f2)); e != "" {
		t.Fatal(e)
	}
	for name, c := range map[string]struct {
		res  string
		cfg  map[string]any
		want string
	}{
		"kind":   {"forgebase_feature", map[string]any{"key": "a", "kind": "counter"}, "kind value must be one of"},
		"period": {"forgebase_plan", map[string]any{"key": "a", "features": []any{map[string]any{"feature": "x", "period": "year"}}}, "period value must be one of"},
		"trial":  {"forgebase_plan", map[string]any{"key": "a", "trial_days": -1}, "at least 0"},
	} {
		if e := errText(k.validate(c.res, c.cfg)); !strings.Contains(e, c.want) {
			t.Errorf("%s: want %q in %q", name, c.want, e)
		}
	}
}
