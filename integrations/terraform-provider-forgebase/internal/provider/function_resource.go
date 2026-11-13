package provider

import (
	"context"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"fmt"
	"os"
	"regexp"
	"sort"

	"github.com/hashicorp/terraform-plugin-framework-validators/resourcevalidator"
	"github.com/hashicorp/terraform-plugin-framework-validators/setvalidator"
	"github.com/hashicorp/terraform-plugin-framework-validators/stringvalidator"
	"github.com/hashicorp/terraform-plugin-framework/attr"
	"github.com/hashicorp/terraform-plugin-framework/path"
	"github.com/hashicorp/terraform-plugin-framework/resource"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/int64planmodifier"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/listdefault"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/mapdefault"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/objectplanmodifier"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/planmodifier"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/setdefault"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/stringplanmodifier"
	"github.com/hashicorp/terraform-plugin-framework/schema/validator"
	"github.com/hashicorp/terraform-plugin-framework/types"
)

var (
	_              resource.Resource                   = (*functionResource)(nil)
	_              resource.ResourceWithConfigure      = (*functionResource)(nil)
	_              resource.ResourceWithImportState    = (*functionResource)(nil)
	_              resource.ResourceWithModifyPlan     = (*functionResource)(nil)
	_              resource.ResourceWithValidateConfig = (*functionResource)(nil)
	functionNameRE                                     = regexp.MustCompile(`^[a-z][a-z0-9_-]{0,47}$`)
)

// functionCapabilities are the capabilities the server accepts at deploy.
var functionCapabilities = []string{
	"db.read", "db.write", "kv", "log", "env", "secrets", "fetch", "vector.read", "vector.write",
	"storage.read", "storage.write", "queues.publish", "jobs.enqueue", "realtime.publish", "service_role",
}

// NewFunctionResource returns the forgebase_function resource.
func NewFunctionResource() resource.Resource { return &functionResource{} }

type functionResource struct{ base }

type limitsModel struct {
	Fuel           types.Int64 `tfsdk:"fuel"`
	MemoryBytes    types.Int64 `tfsdk:"memory_bytes"`
	TimeoutMs      types.Int64 `tfsdk:"timeout_ms"`
	MaxConcurrency types.Int64 `tfsdk:"max_concurrency"`
}

var limitsObjType = types.ObjectType{AttrTypes: map[string]attr.Type{
	"fuel": types.Int64Type, "memory_bytes": types.Int64Type, "timeout_ms": types.Int64Type, "max_concurrency": types.Int64Type,
}}

type hookModel struct {
	Collection types.String `tfsdk:"collection"`
	Events     types.Set    `tfsdk:"events"`
}

var hookObjType = types.ObjectType{AttrTypes: map[string]attr.Type{
	"collection": types.StringType, "events": types.SetType{ElemType: types.StringType},
}}

type kvRuleModel struct {
	Namespace types.String `tfsdk:"namespace"`
	Access    types.String `tfsdk:"access"`
}

var kvRuleObjType = types.ObjectType{AttrTypes: map[string]attr.Type{"namespace": types.StringType, "access": types.StringType}}

type functionModel struct {
	ID            types.String `tfsdk:"id"`
	Name          types.String `tfsdk:"name"`
	Language      types.String `tfsdk:"language"`
	Source        types.String `tfsdk:"source"`
	SourceFile    types.String `tfsdk:"source_file"`
	WasmFile      types.String `tfsdk:"wasm_file"`
	ContentSHA256 types.String `tfsdk:"content_sha256"`
	Description   types.String `tfsdk:"description"`
	Access        types.String `tfsdk:"access"`
	Capabilities  types.Set    `tfsdk:"capabilities"`
	FetchHosts    types.Set    `tfsdk:"fetch_hosts"`
	Env           types.Map    `tfsdk:"env"`
	Secrets       types.Map    `tfsdk:"secrets"`
	Hooks         types.List   `tfsdk:"hooks"`
	KVRules       types.Set    `tfsdk:"kv_rules"`
	Limits        types.Object `tfsdk:"limits"`
	ActiveVersion types.Int64  `tfsdk:"active_version"`
	LatestVersion types.Int64  `tfsdk:"latest_version"`
	Principal     types.String `tfsdk:"principal"`
	CreatedAt     types.String `tfsdk:"created_at"`
	UpdatedAt     types.String `tfsdk:"updated_at"`
}

type apiFunction struct {
	ID            string `json:"id"`
	Name          string `json:"name"`
	Description   string `json:"description"`
	Access        string `json:"access"`
	ActiveVersion int64  `json:"active_version"`
	LatestVersion int64  `json:"latest_version"`
	Principal     string `json:"principal"`
	CreatedAt     string `json:"created_at"`
	UpdatedAt     string `json:"updated_at"`
}

type apiVersion struct {
	Version      int64             `json:"version"`
	SHA256       string            `json:"sha256"`
	Active       bool              `json:"active"`
	Language     string            `json:"language"`
	Capabilities []string          `json:"capabilities"`
	FetchHosts   []string          `json:"fetch_hosts"`
	Env          map[string]string `json:"env"`
	Hooks        []struct {
		Collection string   `json:"collection"`
		Events     []string `json:"events"`
	} `json:"hooks"`
	Limits struct {
		Fuel           int64 `json:"fuel"`
		MemoryBytes    int64 `json:"memory_bytes"`
		TimeoutMs      int64 `json:"timeout_ms"`
		MaxConcurrency int64 `json:"max_concurrency"`
	} `json:"limits"`
}

func (r *functionResource) Metadata(_ context.Context, req resource.MetadataRequest, resp *resource.MetadataResponse) {
	resp.TypeName = req.ProviderTypeName + "_function"
}

func (r *functionResource) Configure(_ context.Context, req resource.ConfigureRequest, resp *resource.ConfigureResponse) {
	r.configureResource(req, resp)
}

func (r *functionResource) Schema(_ context.Context, _ resource.SchemaRequest, resp *resource.SchemaResponse) {
	useStateInt := []planmodifier.Int64{int64planmodifier.UseStateForUnknown()}
	useStateStr := []planmodifier.String{stringplanmodifier.UseStateForUnknown()}
	resp.Schema = schema.Schema{
		Description: "A serverless function: a JavaScript or TypeScript module, or a WebAssembly module. Every change to the code or configuration " +
			"deploys a new version and activates it. The provider hashes the code (`content_sha256`) so a changed file shows in the plan, and a " +
			"version changed outside Terraform shows as drift.",
		Attributes: map[string]schema.Attribute{
			"id": schema.StringAttribute{Computed: true, PlanModifiers: useStateStr, Description: "The function name."},
			"name": schema.StringAttribute{
				Required:      true,
				Description:   "Function name: 1-48 characters, a lower-case letter, then lower-case letters, digits, `_` or `-`. Cannot change.",
				Validators:    []validator.String{stringvalidator.RegexMatches(functionNameRE, "must be 1-48 characters: a lower-case letter, then lower-case letters, digits, _ or -")},
				PlanModifiers: []planmodifier.String{stringplanmodifier.RequiresReplace()},
			},
			"language": schema.StringAttribute{
				Optional: true, Computed: true, PlanModifiers: useStateStr,
				Description: "`typescript` (default for `source`), `javascript` or `wasm` (implied by `wasm_file`).",
				Validators:  []validator.String{stringvalidator.OneOf("typescript", "javascript", "wasm")},
			},
			"source":      schema.StringAttribute{Optional: true, Description: "Inline JavaScript/TypeScript source of a single-file ES module that calls `serve()` from `\"forgebase\"` (at most 1 MiB). Exactly one of `source`, `source_file` and `wasm_file`."},
			"source_file": schema.StringAttribute{Optional: true, Description: "Path to the JavaScript/TypeScript source file, read when planning and applying. Changes to the file's content are detected by hash."},
			"wasm_file":   schema.StringAttribute{Optional: true, Description: "Path to a compiled WebAssembly module, read when planning and applying. Changes to the file's content are detected by hash."},
			"content_sha256": schema.StringAttribute{
				Computed:    true,
				Description: "SHA-256 (hex) of the source text or WebAssembly bytes. Compared with the active version on the server to detect drift.",
			},
			"description": schema.StringAttribute{Optional: true, Computed: true, PlanModifiers: useStateStr, Description: "Description."},
			"access": schema.StringAttribute{
				Optional: true, Computed: true, PlanModifiers: useStateStr,
				Description: "Who may invoke the function over HTTP: `public`, `authenticated` or `permission` (default).",
				Validators:  []validator.String{stringvalidator.OneOf("public", "authenticated", "permission")},
			},
			"capabilities": schema.SetAttribute{
				Optional: true, Computed: true, ElementType: types.StringType,
				Default:     setdefault.StaticValue(types.SetValueMust(types.StringType, []attr.Value{})),
				Description: "Granted capabilities: " + joinCode(functionCapabilities) + ".",
				Validators:  []validator.Set{setvalidator.ValueStringsAre(stringvalidator.OneOf(functionCapabilities...))},
			},
			"fetch_hosts": schema.SetAttribute{
				Optional: true, Computed: true, ElementType: types.StringType,
				Default:     setdefault.StaticValue(types.SetValueMust(types.StringType, []attr.Value{})),
				Description: "Hosts the function may call with `fetch` (needs the `fetch` capability).",
			},
			"env": schema.MapAttribute{
				Optional: true, Computed: true, ElementType: types.StringType,
				Default:     mapdefault.StaticValue(types.MapValueMust(types.StringType, map[string]attr.Value{})),
				Description: "Plain (non-secret) environment values, readable with the `env` capability. Use `secrets` for credentials.",
			},
			"secrets": schema.MapAttribute{
				Optional: true, Sensitive: true, ElementType: types.StringType,
				Description: "Secret values by name, readable with the `secrets` capability. The server never returns values, so only the presence of each name is checked for drift; " +
					"to rotate a secret, change its value. Secrets not listed here are left alone, except those this resource set earlier and no longer lists, which are deleted.",
			},
			"hooks": schema.ListNestedAttribute{
				Optional: true, Computed: true,
				Default:     listdefault.StaticValue(types.ListValueMust(hookObjType, []attr.Value{})),
				Description: "Record hooks: run the function around record changes of a collection.",
				NestedObject: schema.NestedAttributeObject{Attributes: map[string]schema.Attribute{
					"collection": schema.StringAttribute{Required: true, Description: "Collection name."},
					"events": schema.SetAttribute{
						Required: true, ElementType: types.StringType,
						Description: "`before_create`, `after_create`, `before_update`, `after_update`, `before_delete`, `after_delete`.",
						Validators: []validator.Set{setvalidator.ValueStringsAre(stringvalidator.OneOf(
							"before_create", "after_create", "before_update", "after_update", "before_delete", "after_delete"))},
					},
				}},
			},
			"kv_rules": schema.SetNestedAttribute{
				Optional: true, Computed: true,
				Description: "Key-value namespaces a JavaScript function may reach (deny by default; needs the `kv` capability and an administrator credential).",
				Default:     setdefault.StaticValue(types.SetValueMust(kvRuleObjType, []attr.Value{})),
				NestedObject: schema.NestedAttributeObject{Attributes: map[string]schema.Attribute{
					"namespace": schema.StringAttribute{Required: true, Description: "Key-value namespace."},
					"access": schema.StringAttribute{
						Required: true, Description: "`read` or `write`.", Validators: []validator.String{stringvalidator.OneOf("read", "write")},
					},
				}},
			},
			"limits": schema.SingleNestedAttribute{
				Optional: true, Computed: true,
				PlanModifiers: []planmodifier.Object{objectplanmodifier.UseStateForUnknown()},
				Description:   "Resource limits. Unset values use the server defaults.",
				Attributes: map[string]schema.Attribute{
					"fuel":            schema.Int64Attribute{Optional: true, Computed: true, PlanModifiers: useStateInt, Description: "Instruction budget per invocation."},
					"memory_bytes":    schema.Int64Attribute{Optional: true, Computed: true, PlanModifiers: useStateInt, Description: "Memory limit in bytes."},
					"timeout_ms":      schema.Int64Attribute{Optional: true, Computed: true, PlanModifiers: useStateInt, Description: "Time limit per invocation in milliseconds."},
					"max_concurrency": schema.Int64Attribute{Optional: true, Computed: true, PlanModifiers: useStateInt, Description: "Concurrent invocations."},
				},
			},
			"active_version": schema.Int64Attribute{Computed: true, Description: "Version currently active."},
			"latest_version": schema.Int64Attribute{Computed: true, Description: "Newest deployed version."},
			"principal":      schema.StringAttribute{Computed: true, PlanModifiers: useStateStr, Description: "Identity the function runs as."},
			"created_at":     schema.StringAttribute{Computed: true, PlanModifiers: useStateStr, Description: "Creation time."},
			"updated_at":     schema.StringAttribute{Computed: true, Description: "Last change."},
		},
	}
}

func joinCode(items []string) string {
	s := ""
	for i, it := range items {
		if i > 0 {
			s += ", "
		}
		s += "`" + it + "`"
	}
	return s
}

// ConfigValidators requires exactly one code source.
func (r *functionResource) ConfigValidators(context.Context) []resource.ConfigValidator {
	return []resource.ConfigValidator{
		resourcevalidator.ExactlyOneOf(path.MatchRoot("source"), path.MatchRoot("source_file"), path.MatchRoot("wasm_file")),
	}
}

func (r *functionResource) ValidateConfig(ctx context.Context, req resource.ValidateConfigRequest, resp *resource.ValidateConfigResponse) {
	var m functionModel
	resp.Diagnostics.Append(req.Config.Get(ctx, &m)...)
	if resp.Diagnostics.HasError() || m.Language.IsUnknown() {
		return
	}
	lang := m.Language.ValueString()
	switch {
	case !m.WasmFile.IsNull() && lang != "" && lang != "wasm":
		resp.Diagnostics.AddAttributeError(path.Root("language"), "Language mismatch", "`wasm_file` is WebAssembly; set `language = \"wasm\"` or omit it.")
	case (!m.Source.IsNull() || !m.SourceFile.IsNull()) && lang == "wasm":
		resp.Diagnostics.AddAttributeError(path.Root("language"), "Language mismatch", "`source` and `source_file` hold JavaScript or TypeScript; use `wasm_file` for WebAssembly.")
	}
	if !m.Source.IsNull() && !m.Source.IsUnknown() && len(m.Source.ValueString()) > 1<<20 {
		resp.Diagnostics.AddAttributeError(path.Root("source"), "Source too large", "Function source is limited to 1 MiB.")
	}
}

// code returns the function's code bytes, whether it is wasm, and ok=false
// when the content is not known yet.
func (m functionModel) code() (data []byte, wasm bool, ok bool, err error) {
	switch {
	case !m.Source.IsNull():
		if m.Source.IsUnknown() {
			return nil, false, false, nil
		}
		return []byte(m.Source.ValueString()), false, true, nil
	case !m.SourceFile.IsNull():
		if m.SourceFile.IsUnknown() {
			return nil, false, false, nil
		}
		b, err := os.ReadFile(m.SourceFile.ValueString())
		return b, false, err == nil, err
	case !m.WasmFile.IsNull():
		if m.WasmFile.IsUnknown() {
			return nil, true, false, nil
		}
		b, err := os.ReadFile(m.WasmFile.ValueString())
		return b, true, err == nil, err
	}
	return nil, false, false, nil
}

func sha256Hex(b []byte) string {
	sum := sha256.Sum256(b)
	return hex.EncodeToString(sum[:])
}

// ModifyPlan computes content_sha256 from the configured code so that a
// changed file or source shows in the plan.
func (r *functionResource) ModifyPlan(ctx context.Context, req resource.ModifyPlanRequest, resp *resource.ModifyPlanResponse) {
	if req.Plan.Raw.IsNull() {
		return
	}
	var plan functionModel
	resp.Diagnostics.Append(req.Plan.Get(ctx, &plan)...)
	if resp.Diagnostics.HasError() {
		return
	}
	data, _, ok, err := plan.code()
	if err != nil {
		resp.Diagnostics.AddAttributeError(path.Root("source_file"), "Cannot read function code", err.Error())
		return
	}
	if !ok {
		return
	}
	resp.Diagnostics.Append(resp.Plan.SetAttribute(ctx, path.Root("content_sha256"), sha256Hex(data))...)
	// A new deploy bumps the versions and the update time.
	if !req.State.Raw.IsNull() {
		var state functionModel
		resp.Diagnostics.Append(req.State.Get(ctx, &state)...)
		if resp.Diagnostics.HasError() {
			return
		}
		if sha256Hex(data) == state.ContentSHA256.ValueString() && !r.needsDeploy(plan, state) {
			resp.Diagnostics.Append(resp.Plan.SetAttribute(ctx, path.Root("active_version"), state.ActiveVersion)...)
			resp.Diagnostics.Append(resp.Plan.SetAttribute(ctx, path.Root("latest_version"), state.LatestVersion)...)
			resp.Diagnostics.Append(resp.Plan.SetAttribute(ctx, path.Root("updated_at"), state.UpdatedAt)...)
		}
	}
}

func (r *functionResource) ImportState(ctx context.Context, req resource.ImportStateRequest, resp *resource.ImportStateResponse) {
	resp.Diagnostics.Append(resp.State.SetAttribute(ctx, path.Root("id"), req.ID)...)
}

// needsDeploy reports whether anything that goes into a new version changed.
func (r *functionResource) needsDeploy(plan, state functionModel) bool {
	known := func(p, s attr.Value) bool { return p.IsUnknown() || p.Equal(s) }
	if plan.ContentSHA256.ValueString() != state.ContentSHA256.ValueString() {
		return true
	}
	return !(known(plan.Language, state.Language) && known(plan.Description, state.Description) && known(plan.Access, state.Access) &&
		known(plan.Capabilities, state.Capabilities) && known(plan.FetchHosts, state.FetchHosts) && known(plan.Env, state.Env) &&
		known(plan.Hooks, state.Hooks) && known(plan.Limits, state.Limits))
}

// deployBody builds the deploy request.
func (m functionModel) deployBody(ctx context.Context) (map[string]any, diagList) {
	var diags diagList
	data, wasm, ok, err := m.code()
	if err != nil {
		diags.AddError("Cannot read function code", err.Error())
		return nil, diags
	}
	if !ok {
		diags.AddError("Function code is not known", "`source`, `source_file` or `wasm_file` is unknown at apply time.")
		return nil, diags
	}
	if want := m.ContentSHA256; !want.IsUnknown() && !want.IsNull() && want.ValueString() != sha256Hex(data) {
		diags.AddError("Function code changed since the plan",
			"The code on disk no longer matches the planned content_sha256. Run `terraform plan` again.")
		return nil, diags
	}
	body := map[string]any{
		"name": m.Name.ValueString(), "activate": true,
		"capabilities": setStrings(m.Capabilities), "fetch_hosts": setStrings(m.FetchHosts), "env": mapStrings(m.Env),
	}
	if wasm {
		body["wasm"] = base64.StdEncoding.EncodeToString(data)
		body["language"] = "wasm"
	} else {
		body["source"] = string(data)
		if l := strPtr(m.Language); l != nil && *l != "" {
			body["language"] = *l
		}
	}
	if p := strPtr(m.Description); p != nil {
		body["description"] = *p
	}
	if p := strPtr(m.Access); p != nil && *p != "" {
		body["access"] = *p
	}
	hooks := []map[string]any{}
	if !m.Hooks.IsNull() && !m.Hooks.IsUnknown() {
		var hm []hookModel
		diags.Append(m.Hooks.ElementsAs(ctx, &hm, false)...)
		for _, h := range hm {
			hooks = append(hooks, map[string]any{"collection": h.Collection.ValueString(), "events": setStrings(h.Events)})
		}
	}
	body["hooks"] = hooks
	if !m.Limits.IsNull() && !m.Limits.IsUnknown() {
		var l limitsModel
		diags.Append(m.Limits.As(ctx, &l, basicObjectOptions)...)
		lim := map[string]any{}
		for k, v := range map[string]types.Int64{"fuel": l.Fuel, "memory_bytes": l.MemoryBytes, "timeout_ms": l.TimeoutMs, "max_concurrency": l.MaxConcurrency} {
			if p := int64Ptr(v); p != nil {
				lim[k] = *p
			}
		}
		body["limits"] = lim
	}
	return body, diags
}

type apiDeployed struct {
	Function apiFunction `json:"function"`
	Version  apiVersion  `json:"version"`
}

// functionFromAPI builds the model. sec and kv are the managed secrets map and
// kv rules; hint keeps the code-source attributes (they are not stored on the server).
func functionFromAPI(ctx context.Context, f apiFunction, v apiVersion, hint functionModel, secrets types.Map, kv []apiKVRule) (functionModel, diagList) {
	var diags diagList
	m := functionModel{
		ID: types.StringValue(f.Name), Name: types.StringValue(f.Name),
		Source: hint.Source, SourceFile: hint.SourceFile, WasmFile: hint.WasmFile,
		ContentSHA256: types.StringValue(v.SHA256), Description: types.StringValue(f.Description), Access: types.StringValue(f.Access),
		Language:     types.StringValue(v.Language),
		Capabilities: strSet(v.Capabilities), FetchHosts: strSet(v.FetchHosts), Env: strMap(v.Env), Secrets: secrets,
		ActiveVersion: types.Int64Value(f.ActiveVersion), LatestVersion: types.Int64Value(f.LatestVersion),
		Principal: types.StringValue(f.Principal), CreatedAt: types.StringValue(f.CreatedAt), UpdatedAt: types.StringValue(f.UpdatedAt),
	}
	if v.Env == nil {
		m.Env = strMap(map[string]string{})
	}
	hooks := make([]hookModel, 0, len(v.Hooks))
	for _, h := range v.Hooks {
		hooks = append(hooks, hookModel{Collection: types.StringValue(h.Collection), Events: strSet(h.Events)})
	}
	hl, d := types.ListValueFrom(ctx, hookObjType, hooks)
	diags.Append(d...)
	m.Hooks = hl
	rules := make([]kvRuleModel, 0, len(kv))
	for _, k := range kv {
		rules = append(rules, kvRuleModel{Namespace: types.StringValue(k.Namespace), Access: types.StringValue(k.Access)})
	}
	ks, d := types.SetValueFrom(ctx, kvRuleObjType, rules)
	diags.Append(d...)
	m.KVRules = ks
	lo, d := types.ObjectValueFrom(ctx, limitsObjType.AttrTypes, limitsModel{
		Fuel: types.Int64Value(v.Limits.Fuel), MemoryBytes: types.Int64Value(v.Limits.MemoryBytes),
		TimeoutMs: types.Int64Value(v.Limits.TimeoutMs), MaxConcurrency: types.Int64Value(v.Limits.MaxConcurrency),
	})
	diags.Append(d...)
	m.Limits = lo
	return m, diags
}

type apiKVRule struct {
	Namespace string `json:"namespace"`
	Access    string `json:"access"`
}

func kvRulesFromModel(ctx context.Context, s types.Set) ([]apiKVRule, diagList) {
	var diags diagList
	out := []apiKVRule{}
	if s.IsNull() || s.IsUnknown() {
		return out, diags
	}
	var ms []kvRuleModel
	diags.Append(s.ElementsAs(ctx, &ms, false)...)
	for _, k := range ms {
		out = append(out, apiKVRule{Namespace: k.Namespace.ValueString(), Access: k.Access.ValueString()})
	}
	sort.Slice(out, func(i, j int) bool { return out[i].Namespace < out[j].Namespace })
	return out, diags
}

// applySecrets sets the secrets in want and deletes the names in prior that are no longer wanted.
func (r *functionResource) applySecrets(ctx context.Context, name string, want, prior map[string]string) error {
	for k, v := range want {
		if pv, had := prior[k]; had && pv == v {
			continue
		}
		if err := r.c.Put(ctx, "/api/v1/functions/"+seg(name)+"/secrets/"+seg(k), map[string]string{"value": v}, nil); err != nil {
			return fmt.Errorf("set secret %q: %w", k, err)
		}
	}
	for k := range prior {
		if _, keep := want[k]; !keep {
			if err := r.c.Delete(ctx, "/api/v1/functions/"+seg(name)+"/secrets/"+seg(k), nil); err != nil && !isGone(err) {
				return fmt.Errorf("delete secret %q: %w", k, err)
			}
		}
	}
	return nil
}

func (r *functionResource) putKV(ctx context.Context, name string, rules []apiKVRule) error {
	return r.c.Put(ctx, "/api/v1/functions/"+seg(name)+"/kv-rules", map[string]any{"items": rules}, nil)
}

func (r *functionResource) Create(ctx context.Context, req resource.CreateRequest, resp *resource.CreateResponse) {
	var plan functionModel
	resp.Diagnostics.Append(req.Plan.Get(ctx, &plan)...)
	if resp.Diagnostics.HasError() {
		return
	}
	body, d := plan.deployBody(ctx)
	resp.Diagnostics.Append(d...)
	if resp.Diagnostics.HasError() {
		return
	}
	var out apiDeployed
	if err := r.c.Post(ctx, "/api/v1/functions", body, &out); err != nil {
		addErr(&resp.Diagnostics, "Error deploying function", err)
		return
	}
	rules, d := kvRulesFromModel(ctx, plan.KVRules)
	resp.Diagnostics.Append(d...)
	st, d := functionFromAPI(ctx, out.Function, out.Version, plan, types.MapNull(types.StringType), rules)
	resp.Diagnostics.Append(d...)
	// Save what exists now; a failure below leaves the resource tainted.
	resp.Diagnostics.Append(resp.State.Set(ctx, st)...)
	if resp.Diagnostics.HasError() {
		return
	}
	name := out.Function.Name
	if secrets := mapStrings(plan.Secrets); len(secrets) > 0 {
		if err := r.applySecrets(ctx, name, secrets, nil); err != nil {
			addErr(&resp.Diagnostics, "Error setting function secrets", err)
			return
		}
	}
	st.Secrets = plan.Secrets
	if len(rules) > 0 {
		if err := r.putKV(ctx, name, rules); err != nil {
			addErr(&resp.Diagnostics, "Error setting function kv rules", err)
			return
		}
	}
	resp.Diagnostics.Append(resp.State.Set(ctx, st)...)
}

func (r *functionResource) readParts(ctx context.Context, name string) (apiFunction, apiVersion, []string, []apiKVRule, bool, error) {
	var f apiFunction
	if err := r.c.Get(ctx, "/api/v1/functions/"+seg(name), nil, &f); err != nil {
		return f, apiVersion{}, nil, nil, false, err
	}
	var vs items[apiVersion]
	if err := r.c.Get(ctx, "/api/v1/functions/"+seg(name)+"/versions", nil, &vs); err != nil {
		return f, apiVersion{}, nil, nil, false, err
	}
	var v apiVersion
	for _, cand := range vs.Items {
		if cand.Active || cand.Version == f.ActiveVersion {
			v = cand
			break
		}
	}
	var sn items[struct {
		Name string `json:"name"`
	}]
	if err := r.c.Get(ctx, "/api/v1/functions/"+seg(name)+"/secrets", nil, &sn); err != nil {
		return f, v, nil, nil, false, err
	}
	names := make([]string, 0, len(sn.Items))
	for _, s := range sn.Items {
		names = append(names, s.Name)
	}
	var kv items[apiKVRule]
	// kv-rules need an administrator; a refusal means "rules not visible".
	if err := r.c.Get(ctx, "/api/v1/functions/"+seg(name)+"/kv-rules", nil, &kv); err != nil {
		if isForbidden(err) {
			return f, v, names, nil, false, nil
		}
		return f, v, names, nil, false, err
	}
	return f, v, names, kv.Items, true, nil
}

func (r *functionResource) Read(ctx context.Context, req resource.ReadRequest, resp *resource.ReadResponse) {
	var prior functionModel
	resp.Diagnostics.Append(req.State.Get(ctx, &prior)...)
	if resp.Diagnostics.HasError() {
		return
	}
	f, v, names, kv, kvKnown, err := r.readParts(ctx, prior.ID.ValueString())
	if removeIfGone(ctx, &resp.State, &resp.Diagnostics, "function", err) {
		return
	}
	// Secrets: keep the values this resource set, minus those gone from the server.
	secrets := types.MapNull(types.StringType)
	if !prior.Secrets.IsNull() {
		have := map[string]bool{}
		for _, n := range names {
			have[n] = true
		}
		kept := map[string]string{}
		for k, val := range mapStrings(prior.Secrets) {
			if have[k] {
				kept[k] = val
			}
		}
		secrets = strMap(kept)
	}
	if !kvKnown {
		kv, _ = kvRulesFromModel(ctx, prior.KVRules)
	}
	hint := prior
	if prior.Source.IsNull() && prior.SourceFile.IsNull() && prior.WasmFile.IsNull() && v.Language != "wasm" {
		// Imported: show the deployed source so it can be copied into the configuration.
		var vs struct {
			Source string `json:"source"`
		}
		if err := r.c.Get(ctx, fmt.Sprintf("/api/v1/functions/%s/versions/%d/source", seg(f.Name), v.Version), nil, &vs); err == nil {
			hint.Source = types.StringValue(vs.Source)
		}
	}
	st, d := functionFromAPI(ctx, f, v, hint, secrets, kv)
	resp.Diagnostics.Append(d...)
	resp.Diagnostics.Append(resp.State.Set(ctx, st)...)
}

func (r *functionResource) Update(ctx context.Context, req resource.UpdateRequest, resp *resource.UpdateResponse) {
	var plan, prior functionModel
	resp.Diagnostics.Append(req.Plan.Get(ctx, &plan)...)
	resp.Diagnostics.Append(req.State.Get(ctx, &prior)...)
	if resp.Diagnostics.HasError() {
		return
	}
	name := prior.ID.ValueString()
	f := apiFunction{ID: prior.ID.ValueString(), Name: name}
	var v apiVersion
	if r.needsDeploy(plan, prior) {
		body, d := plan.deployBody(ctx)
		resp.Diagnostics.Append(d...)
		if resp.Diagnostics.HasError() {
			return
		}
		var out apiDeployed
		if err := r.c.Post(ctx, "/api/v1/functions", body, &out); err != nil {
			addErr(&resp.Diagnostics, "Error deploying function", err)
			return
		}
		f, v = out.Function, out.Version
	} else {
		var err error
		f, v, _, _, _, err = r.readParts(ctx, name)
		if err != nil {
			addErr(&resp.Diagnostics, "Error reading function", err)
			return
		}
	}
	rules, d := kvRulesFromModel(ctx, plan.KVRules)
	resp.Diagnostics.Append(d...)
	secretsState := prior.Secrets
	st, d := functionFromAPI(ctx, f, v, plan, secretsState, rules)
	resp.Diagnostics.Append(d...)
	resp.Diagnostics.Append(resp.State.Set(ctx, st)...)
	if resp.Diagnostics.HasError() {
		return
	}
	if !plan.Secrets.Equal(prior.Secrets) {
		if err := r.applySecrets(ctx, name, mapStrings(plan.Secrets), mapStrings(prior.Secrets)); err != nil {
			addErr(&resp.Diagnostics, "Error updating function secrets", err)
			return
		}
		st.Secrets = plan.Secrets
	}
	if !plan.KVRules.Equal(prior.KVRules) {
		if err := r.putKV(ctx, name, rules); err != nil {
			addErr(&resp.Diagnostics, "Error updating function kv rules", err)
			return
		}
	}
	resp.Diagnostics.Append(resp.State.Set(ctx, st)...)
}

func (r *functionResource) Delete(ctx context.Context, req resource.DeleteRequest, resp *resource.DeleteResponse) {
	var state functionModel
	resp.Diagnostics.Append(req.State.Get(ctx, &state)...)
	if resp.Diagnostics.HasError() {
		return
	}
	if err := r.c.Delete(ctx, "/api/v1/functions/"+seg(state.ID.ValueString()), nil); err != nil && !isGone(err) {
		addErr(&resp.Diagnostics, "Error deleting function", err)
	}
}
