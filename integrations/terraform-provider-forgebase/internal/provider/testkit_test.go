package provider_test

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"math/big"
	"net/http"
	"net/http/httptest"
	"sort"
	"strings"
	"sync"
	"testing"

	"github.com/hashicorp/terraform-plugin-framework/providerserver"
	"github.com/hashicorp/terraform-plugin-go/tfprotov6"
	"github.com/hashicorp/terraform-plugin-go/tftypes"

	"github.com/farzanshibu/forgebase/integrations/terraform-provider-forgebase/internal/provider"
)

// ---- mock ForgeBase server ----------------------------------------------------

type recorded struct {
	Method string
	Path   string
	Query  string
	Auth   string
	Body   map[string]any
}

// mockServer is an httptest server with Go 1.22 method+path patterns.
type mockServer struct {
	t    *testing.T
	mux  *http.ServeMux
	srv  *httptest.Server
	mu   sync.Mutex
	reqs []recorded
}

func newMock(t *testing.T) *mockServer {
	t.Helper()
	m := &mockServer{t: t, mux: http.NewServeMux()}
	m.srv = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		raw, _ := io.ReadAll(r.Body)
		rec := recorded{Method: r.Method, Path: r.URL.EscapedPath(), Query: r.URL.RawQuery, Auth: r.Header.Get("Authorization")}
		if len(raw) > 0 {
			_ = json.Unmarshal(raw, &rec.Body)
		}
		m.mu.Lock()
		m.reqs = append(m.reqs, rec)
		m.mu.Unlock()
		r.Body = io.NopCloser(strings.NewReader(string(raw)))
		m.mux.ServeHTTP(w, r)
	}))
	t.Cleanup(m.srv.Close)
	return m
}

// on registers a handler. The handler returns a status and a body (nil for none).
func (m *mockServer) on(pattern string, h func(r *http.Request, body map[string]any) (int, any)) {
	m.mux.HandleFunc(pattern, func(w http.ResponseWriter, r *http.Request) {
		var body map[string]any
		raw, _ := io.ReadAll(r.Body)
		if len(raw) > 0 {
			_ = json.Unmarshal(raw, &body)
		}
		code, out := h(r, body)
		if out != nil {
			w.Header().Set("Content-Type", "application/json")
		}
		w.WriteHeader(code)
		if out != nil {
			_ = json.NewEncoder(w).Encode(out)
		}
	})
}

func apiErr(code, msg string) map[string]any {
	return map[string]any{"error": map[string]any{"code": code, "message": msg, "request_id": "req-1"}}
}

// requests returns the recorded requests matching "METHOD /path-prefix".
func (m *mockServer) requests(method, pathPrefix string) []recorded {
	m.mu.Lock()
	defer m.mu.Unlock()
	var out []recorded
	for _, r := range m.reqs {
		if r.Method == method && strings.HasPrefix(r.Path, pathPrefix) {
			out = append(out, r)
		}
	}
	return out
}

func (m *mockServer) last(method, pathPrefix string) recorded {
	m.t.Helper()
	rs := m.requests(method, pathPrefix)
	if len(rs) == 0 {
		m.t.Fatalf("no %s %s request; got %v", method, pathPrefix, m.summary())
	}
	return rs[len(rs)-1]
}

func (m *mockServer) summary() []string {
	m.mu.Lock()
	defer m.mu.Unlock()
	out := make([]string, len(m.reqs))
	for i, r := range m.reqs {
		out[i] = r.Method + " " + r.Path
	}
	return out
}

func (m *mockServer) reset() {
	m.mu.Lock()
	m.reqs = nil
	m.mu.Unlock()
}

// ---- protocol kit ---------------------------------------------------------------

type unknown struct{}

type kit struct {
	t       *testing.T
	srv     tfprotov6.ProviderServer
	schemas map[string]*tfprotov6.Schema
	dsch    map[string]*tfprotov6.Schema
	provSch *tfprotov6.Schema
}

func newKit(t *testing.T, m *mockServer) *kit {
	t.Helper()
	srv := providerserver.NewProtocol6(provider.New("test")())()
	k := &kit{t: t, srv: srv}
	resp, err := srv.GetProviderSchema(context.Background(), &tfprotov6.GetProviderSchemaRequest{})
	if err != nil {
		t.Fatal(err)
	}
	k.failOn(resp.Diagnostics)
	k.schemas, k.dsch, k.provSch = resp.ResourceSchemas, resp.DataSourceSchemas, resp.Provider
	if m != nil {
		k.configure(map[string]any{"url": m.srv.URL, "token": "test-token"})
	}
	return k
}

func (k *kit) failOn(d []*tfprotov6.Diagnostic) {
	k.t.Helper()
	for _, x := range d {
		if x.Severity == tfprotov6.DiagnosticSeverityError {
			k.t.Fatalf("unexpected error diagnostic: %s: %s", x.Summary, x.Detail)
		}
	}
}

func errText(d []*tfprotov6.Diagnostic) string {
	var parts []string
	for _, x := range d {
		if x.Severity == tfprotov6.DiagnosticSeverityError {
			parts = append(parts, x.Summary+": "+x.Detail)
		}
	}
	return strings.Join(parts, "\n")
}

func (k *kit) configure(cfg map[string]any) []*tfprotov6.Diagnostic {
	k.t.Helper()
	typ := k.provSch.ValueType()
	dv, err := tfprotov6.NewDynamicValue(typ, toTF(typ, cfg))
	if err != nil {
		k.t.Fatal(err)
	}
	resp, err := k.srv.ConfigureProvider(context.Background(), &tfprotov6.ConfigureProviderRequest{Config: &dv, TerraformVersion: "1.9.0"})
	if err != nil {
		k.t.Fatal(err)
	}
	return resp.Diagnostics
}

func (k *kit) resType(name string) tftypes.Type {
	k.t.Helper()
	s, ok := k.schemas[name]
	if !ok {
		k.t.Fatalf("unknown resource %s", name)
	}
	return s.ValueType()
}

func (k *kit) dv(typ tftypes.Type, v tftypes.Value) *tfprotov6.DynamicValue {
	k.t.Helper()
	d, err := tfprotov6.NewDynamicValue(typ, v)
	if err != nil {
		k.t.Fatal(err)
	}
	return &d
}

func (k *kit) undv(typ tftypes.Type, d *tfprotov6.DynamicValue) tftypes.Value {
	k.t.Helper()
	if d == nil {
		return tftypes.NewValue(typ, nil)
	}
	v, err := d.Unmarshal(typ)
	if err != nil {
		k.t.Fatal(err)
	}
	return v
}

// validate returns the diagnostics of ValidateResourceConfig.
func (k *kit) validate(res string, cfg map[string]any) []*tfprotov6.Diagnostic {
	k.t.Helper()
	typ := k.resType(res)
	resp, err := k.srv.ValidateResourceConfig(context.Background(), &tfprotov6.ValidateResourceConfigRequest{
		TypeName: res, Config: k.dv(typ, toTF(typ, cfg)),
	})
	if err != nil {
		k.t.Fatal(err)
	}
	return resp.Diagnostics
}

// planResult is the outcome of PlanResourceChange.
type planResult struct {
	Planned         tftypes.Value
	Config          tftypes.Value
	Private         []byte
	RequiresReplace []string
	Diags           []*tfprotov6.Diagnostic
}

// plan plans cfg against prior (a null value for create).
func (k *kit) plan(res string, prior tftypes.Value, cfg map[string]any) planResult {
	k.t.Helper()
	typ := k.resType(res)
	config := toTF(typ, cfg)
	proposed := proposedNew(k.schemas[res].Block, typ, prior, config)
	resp, err := k.srv.PlanResourceChange(context.Background(), &tfprotov6.PlanResourceChangeRequest{
		TypeName: res, PriorState: k.dv(typ, prior), ProposedNewState: k.dv(typ, proposed), Config: k.dv(typ, config),
	})
	if err != nil {
		k.t.Fatal(err)
	}
	pr := planResult{Config: config, Private: resp.PlannedPrivate, Diags: resp.Diagnostics}
	if len(errText(resp.Diagnostics)) == 0 {
		pr.Planned = k.undv(typ, resp.PlannedState)
	}
	for _, p := range resp.RequiresReplace {
		pr.RequiresReplace = append(pr.RequiresReplace, p.String())
	}
	return pr
}

// apply applies a plan; prior is null for create, planned null for destroy.
func (k *kit) apply(res string, prior tftypes.Value, p planResult) (tftypes.Value, []*tfprotov6.Diagnostic) {
	k.t.Helper()
	typ := k.resType(res)
	resp, err := k.srv.ApplyResourceChange(context.Background(), &tfprotov6.ApplyResourceChangeRequest{
		TypeName: res, PriorState: k.dv(typ, prior), PlannedState: k.dv(typ, p.Planned), Config: k.dv(typ, p.Config), PlannedPrivate: p.Private,
	})
	if err != nil {
		k.t.Fatal(err)
	}
	return k.undv(typ, resp.NewState), resp.Diagnostics
}

// create plans and applies cfg from scratch and fails the test on any error.
func (k *kit) create(res string, cfg map[string]any) tftypes.Value {
	k.t.Helper()
	typ := k.resType(res)
	if d := k.validate(res, cfg); errText(d) != "" {
		k.t.Fatalf("validate: %s", errText(d))
	}
	p := k.plan(res, tftypes.NewValue(typ, nil), cfg)
	k.failOn(p.Diags)
	st, d := k.apply(res, tftypes.NewValue(typ, nil), p)
	k.failOn(d)
	return st
}

// createErr plans and applies cfg and returns the error text.
func (k *kit) createErr(res string, cfg map[string]any) string {
	k.t.Helper()
	typ := k.resType(res)
	p := k.plan(res, tftypes.NewValue(typ, nil), cfg)
	if e := errText(p.Diags); e != "" {
		return e
	}
	_, d := k.apply(res, tftypes.NewValue(typ, nil), p)
	return errText(d)
}

// update plans cfg against state and applies it, failing on errors. It
// returns the new state and the paths that force replacement.
func (k *kit) update(res string, state tftypes.Value, cfg map[string]any) (tftypes.Value, []string) {
	k.t.Helper()
	p := k.plan(res, state, cfg)
	k.failOn(p.Diags)
	if len(p.RequiresReplace) > 0 {
		return state, p.RequiresReplace
	}
	st, d := k.apply(res, state, p)
	k.failOn(d)
	return st, nil
}

// planChanged reports whether re-planning the same config shows a diff.
func (k *kit) planChanged(res string, state tftypes.Value, cfg map[string]any) bool {
	k.t.Helper()
	p := k.plan(res, state, cfg)
	k.failOn(p.Diags)
	return !p.Planned.Equal(state)
}

func (k *kit) destroy(res string, state tftypes.Value) []*tfprotov6.Diagnostic {
	k.t.Helper()
	typ := k.resType(res)
	null := tftypes.NewValue(typ, nil)
	_, d := k.apply(res, state, planResult{Planned: null, Config: null})
	return d
}

func (k *kit) read(res string, state tftypes.Value) (tftypes.Value, []*tfprotov6.Diagnostic) {
	k.t.Helper()
	typ := k.resType(res)
	resp, err := k.srv.ReadResource(context.Background(), &tfprotov6.ReadResourceRequest{TypeName: res, CurrentState: k.dv(typ, state)})
	if err != nil {
		k.t.Fatal(err)
	}
	return k.undv(typ, resp.NewState), resp.Diagnostics
}

func (k *kit) importState(res, id string) (tftypes.Value, []*tfprotov6.Diagnostic) {
	k.t.Helper()
	typ := k.resType(res)
	resp, err := k.srv.ImportResourceState(context.Background(), &tfprotov6.ImportResourceStateRequest{TypeName: res, ID: id})
	if err != nil {
		k.t.Fatal(err)
	}
	if len(errText(resp.Diagnostics)) > 0 || len(resp.ImportedResources) == 0 {
		return tftypes.NewValue(typ, nil), resp.Diagnostics
	}
	st := k.undv(typ, resp.ImportedResources[0].State)
	return k.read(res, st)
}

func (k *kit) dataSource(name string, cfg map[string]any) (tftypes.Value, []*tfprotov6.Diagnostic) {
	k.t.Helper()
	s, ok := k.dsch[name]
	if !ok {
		k.t.Fatalf("unknown data source %s", name)
	}
	typ := s.ValueType()
	resp, err := k.srv.ReadDataSource(context.Background(), &tfprotov6.ReadDataSourceRequest{TypeName: name, Config: k.dv(typ, toTF(typ, cfg))})
	if err != nil {
		k.t.Fatal(err)
	}
	return k.undv(typ, resp.State), resp.Diagnostics
}

// ---- value conversion -------------------------------------------------------------

// toTF converts Go natives to a tftypes value of type typ; missing object keys become null.
func toTF(typ tftypes.Type, v any) tftypes.Value {
	if v == nil {
		return tftypes.NewValue(typ, nil)
	}
	if _, ok := v.(unknown); ok {
		return tftypes.NewValue(typ, tftypes.UnknownValue)
	}
	switch {
	case typ.Is(tftypes.String):
		return tftypes.NewValue(typ, v.(string))
	case typ.Is(tftypes.Bool):
		return tftypes.NewValue(typ, v.(bool))
	case typ.Is(tftypes.Number):
		switch n := v.(type) {
		case int:
			return tftypes.NewValue(typ, big.NewFloat(float64(n)))
		case int64:
			return tftypes.NewValue(typ, big.NewFloat(float64(n)))
		case float64:
			return tftypes.NewValue(typ, big.NewFloat(n))
		}
	case typ.Is(tftypes.DynamicPseudoType):
		return tftypes.NewValue(tftypes.String, fmt.Sprint(v))
	}
	switch t := typ.(type) {
	case tftypes.Object:
		in := v.(map[string]any)
		for k := range in {
			if _, ok := t.AttributeTypes[k]; !ok {
				panic("unknown attribute in test config: " + k)
			}
		}
		out := map[string]tftypes.Value{}
		for name, at := range t.AttributeTypes {
			out[name] = toTF(at, in[name])
		}
		return tftypes.NewValue(t, out)
	case tftypes.List:
		items := v.([]any)
		out := make([]tftypes.Value, len(items))
		for i, it := range items {
			out[i] = toTF(t.ElementType, it)
		}
		return tftypes.NewValue(t, out)
	case tftypes.Set:
		items := v.([]any)
		out := make([]tftypes.Value, len(items))
		for i, it := range items {
			out[i] = toTF(t.ElementType, it)
		}
		return tftypes.NewValue(t, out)
	case tftypes.Map:
		in := v.(map[string]any)
		out := map[string]tftypes.Value{}
		for k, it := range in {
			out[k] = toTF(t.ElementType, it)
		}
		return tftypes.NewValue(t, out)
	}
	panic(fmt.Sprintf("toTF: unsupported %T for %v", v, typ))
}

// goValue converts a tftypes value to Go natives for assertions.
func goValue(v tftypes.Value) any {
	if !v.IsKnown() {
		return unknown{}
	}
	if v.IsNull() {
		return nil
	}
	typ := v.Type()
	switch {
	case typ.Is(tftypes.String):
		var s string
		_ = v.As(&s)
		return s
	case typ.Is(tftypes.Bool):
		var b bool
		_ = v.As(&b)
		return b
	case typ.Is(tftypes.Number):
		var n big.Float
		_ = v.As(&n)
		f, _ := n.Float64()
		return f
	}
	switch typ.(type) {
	case tftypes.Object, tftypes.Map:
		var m map[string]tftypes.Value
		_ = v.As(&m)
		out := map[string]any{}
		for k, e := range m {
			out[k] = goValue(e)
		}
		return out
	case tftypes.List, tftypes.Set, tftypes.Tuple:
		var l []tftypes.Value
		_ = v.As(&l)
		out := make([]any, len(l))
		for i, e := range l {
			out[i] = goValue(e)
		}
		if _, isSet := typ.(tftypes.Set); isSet {
			sort.Slice(out, func(i, j int) bool { return fmt.Sprint(out[i]) < fmt.Sprint(out[j]) })
		}
		return out
	}
	return nil
}

// attr reads a dotted path ("fields.0.name") from a state value as Go natives.
func attr(v tftypes.Value, p string) any {
	cur := goValue(v)
	for _, part := range strings.Split(p, ".") {
		switch c := cur.(type) {
		case map[string]any:
			cur = c[part]
		case []any:
			var i int
			fmt.Sscanf(part, "%d", &i)
			if i >= len(c) {
				return nil
			}
			cur = c[i]
		default:
			return nil
		}
	}
	return cur
}

// proposedNew mimics Terraform core's ProposedNew: config values, with computed
// attributes that are null in the config taking their prior value (matched by
// index inside lists of objects).
func proposedNew(block *tfprotov6.SchemaBlock, typ tftypes.Type, prior, config tftypes.Value) tftypes.Value {
	if prior.IsNull() || !prior.IsKnown() || config.IsNull() {
		return config
	}
	var pm, cm map[string]tftypes.Value
	_ = prior.As(&pm)
	_ = config.As(&cm)
	out := map[string]tftypes.Value{}
	attrs := map[string]*tfprotov6.SchemaAttribute{}
	for _, a := range block.Attributes {
		attrs[a.Name] = a
	}
	for name, cv := range cm {
		a := attrs[name]
		pv := pm[name]
		switch {
		case a == nil:
			out[name] = cv
		case cv.IsNull() && a.Computed:
			out[name] = pv
		case a.NestedType != nil && !cv.IsNull():
			out[name] = mergeNested(a.NestedType, a.NestedType.ValueType(), pv, cv)
		default:
			out[name] = cv
		}
	}
	return tftypes.NewValue(typ, out)
}

func mergeNested(nt *tfprotov6.SchemaObject, typ tftypes.Type, prior, config tftypes.Value) tftypes.Value {
	if prior.IsNull() || !prior.IsKnown() || !config.IsKnown() {
		return config
	}
	attrs := map[string]*tfprotov6.SchemaAttribute{}
	for _, a := range nt.Attributes {
		attrs[a.Name] = a
	}
	mergeObj := func(p, c tftypes.Value, ot tftypes.Type) tftypes.Value {
		if p.IsNull() || c.IsNull() {
			return c
		}
		var pm, cm map[string]tftypes.Value
		_ = p.As(&pm)
		_ = c.As(&cm)
		out := map[string]tftypes.Value{}
		for name, cv := range cm {
			a := attrs[name]
			switch {
			case a != nil && cv.IsNull() && a.Computed:
				out[name] = pm[name]
			case a != nil && a.NestedType != nil && !cv.IsNull():
				out[name] = mergeNested(a.NestedType, a.NestedType.ValueType(), pm[name], cv)
			default:
				out[name] = cv
			}
		}
		return tftypes.NewValue(ot, out)
	}
	switch t := typ.(type) {
	case tftypes.List:
		var pl, cl []tftypes.Value
		_ = prior.As(&pl)
		_ = config.As(&cl)
		out := make([]tftypes.Value, len(cl))
		for i, c := range cl {
			if i < len(pl) {
				out[i] = mergeObj(pl[i], c, t.ElementType)
			} else {
				out[i] = c
			}
		}
		return tftypes.NewValue(t, out)
	case tftypes.Object:
		return mergeObj(prior, config, t)
	}
	return config
}

func tftypesNull(k *kit, res string) tftypes.Value { return tftypes.NewValue(k.resType(res), nil) }
