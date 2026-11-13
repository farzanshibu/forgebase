package provider

import (
	"context"
	"sort"

	"github.com/hashicorp/terraform-plugin-framework-jsontypes/jsontypes"
	"github.com/hashicorp/terraform-plugin-framework-validators/int64validator"
	"github.com/hashicorp/terraform-plugin-framework-validators/stringvalidator"
	"github.com/hashicorp/terraform-plugin-framework/attr"
	"github.com/hashicorp/terraform-plugin-framework/path"
	"github.com/hashicorp/terraform-plugin-framework/resource"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/booldefault"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/int64default"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/setdefault"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/stringdefault"
	"github.com/hashicorp/terraform-plugin-framework/schema/validator"
	"github.com/hashicorp/terraform-plugin-framework/types"
)

var featureKeyRE = slugRE

// =============================================================================
// forgebase_feature
// =============================================================================

var (
	_ resource.Resource                = (*featureResource)(nil)
	_ resource.ResourceWithConfigure   = (*featureResource)(nil)
	_ resource.ResourceWithImportState = (*featureResource)(nil)
)

// NewFeatureResource returns the forgebase_feature resource.
func NewFeatureResource() resource.Resource { return &featureResource{} }

type featureResource struct{ base }

type featureModel struct {
	ID          types.String `tfsdk:"id"`
	Key         types.String `tfsdk:"key"`
	Name        types.String `tfsdk:"name"`
	Description types.String `tfsdk:"description"`
	Kind        types.String `tfsdk:"kind"`
	Unit        types.String `tfsdk:"unit"`
	Permissions types.Set    `tfsdk:"permissions"`
	CreatedAt   types.String `tfsdk:"created_at"`
	UpdatedAt   types.String `tfsdk:"updated_at"`
}

type apiFeature struct {
	Key         string   `json:"key"`
	Name        string   `json:"name"`
	Description string   `json:"description"`
	Kind        string   `json:"kind"`
	Unit        string   `json:"unit"`
	Permissions []string `json:"permissions"`
	CreatedAt   string   `json:"created_at"`
	UpdatedAt   string   `json:"updated_at"`
}

func (a apiFeature) model() featureModel {
	return featureModel{
		ID: types.StringValue(a.Key), Key: types.StringValue(a.Key), Name: types.StringValue(a.Name),
		Description: types.StringValue(a.Description), Kind: types.StringValue(a.Kind), Unit: types.StringValue(a.Unit),
		Permissions: strSet(a.Permissions), CreatedAt: types.StringValue(a.CreatedAt), UpdatedAt: types.StringValue(a.UpdatedAt),
	}
}

func (r *featureResource) Metadata(_ context.Context, req resource.MetadataRequest, resp *resource.MetadataResponse) {
	resp.TypeName = req.ProviderTypeName + "_feature"
}

func (r *featureResource) Configure(_ context.Context, req resource.ConfigureRequest, resp *resource.ConfigureResponse) {
	r.configureResource(req, resp)
}

func (r *featureResource) Schema(_ context.Context, _ resource.SchemaRequest, resp *resource.SchemaResponse) {
	resp.Schema = schema.Schema{
		Description: "An entitlement feature of the catalog: something a plan can include or limit (an API, a quota, a rate). Requires a platform-administrator credential.",
		Attributes: map[string]schema.Attribute{
			"id": schema.StringAttribute{Computed: true, PlanModifiers: idState, Description: "The feature key."},
			"key": schema.StringAttribute{
				Required: true, PlanModifiers: replace, Description: "Feature key (for example `ai_generation`). Cannot change.",
				Validators: []validator.String{stringvalidator.RegexMatches(featureKeyRE, "must be 1-64 of [a-z0-9_-]")},
			},
			"name":        schema.StringAttribute{Optional: true, Computed: true, PlanModifiers: idState, Description: "Display name (defaults to the key)."},
			"description": schema.StringAttribute{Optional: true, Computed: true, Default: stringdefault.StaticString(""), Description: "Description."},
			"kind": schema.StringAttribute{
				Required: true, Description: "What the feature measures: `boolean`, `metered`, `gauge`, `concurrency` or `rate`.",
				Validators: []validator.String{stringvalidator.OneOf("boolean", "metered", "gauge", "concurrency", "rate")},
			},
			"unit":        schema.StringAttribute{Optional: true, Computed: true, Default: stringdefault.StaticString(""), Description: "Unit shown with limits (`requests`, `bytes`)."},
			"permissions": schema.SetAttribute{Optional: true, Computed: true, ElementType: types.StringType, Default: setdefault.StaticValue(types.SetValueMust(types.StringType, []attr.Value{})), Description: "Permission patterns this feature gates."},
			"created_at":  schema.StringAttribute{Computed: true, PlanModifiers: idState, Description: "Creation time."},
			"updated_at":  schema.StringAttribute{Computed: true, Description: "Last change."},
		},
	}
}

func (r *featureResource) ImportState(ctx context.Context, req resource.ImportStateRequest, resp *resource.ImportStateResponse) {
	resp.Diagnostics.Append(resp.State.SetAttribute(ctx, path.Root("id"), req.ID)...)
	resp.Diagnostics.Append(resp.State.SetAttribute(ctx, path.Root("key"), req.ID)...)
}

func (r *featureResource) put(ctx context.Context, m featureModel) (featureModel, diagList) {
	var diags diagList
	perms := setStrings(m.Permissions)
	if perms == nil {
		perms = []string{}
	}
	body := map[string]any{"kind": m.Kind.ValueString(), "description": m.Description.ValueString(), "unit": m.Unit.ValueString(), "permissions": perms}
	if p := strPtr(m.Name); p != nil && *p != "" {
		body["name"] = *p
	}
	var out apiFeature
	if err := r.c.Put(ctx, "/api/v1/features/"+seg(m.Key.ValueString()), body, &out); err != nil {
		addErr(&diags, "Error saving feature", err)
		return m, diags
	}
	return out.model(), diags
}

func (r *featureResource) Create(ctx context.Context, req resource.CreateRequest, resp *resource.CreateResponse) {
	var plan featureModel
	resp.Diagnostics.Append(req.Plan.Get(ctx, &plan)...)
	if resp.Diagnostics.HasError() {
		return
	}
	// PUT is create-or-replace: do not silently take over an existing feature.
	var existing apiFeature
	if err := r.c.Get(ctx, "/api/v1/features/"+seg(plan.Key.ValueString()), nil, &existing); err == nil {
		resp.Diagnostics.AddError("Feature already exists", "Feature "+plan.Key.ValueString()+" exists already. Import it with `terraform import` instead.")
		return
	}
	st, d := r.put(ctx, plan)
	resp.Diagnostics.Append(d...)
	if !d.HasError() {
		resp.Diagnostics.Append(resp.State.Set(ctx, st)...)
	}
}

func (r *featureResource) Update(ctx context.Context, req resource.UpdateRequest, resp *resource.UpdateResponse) {
	var plan featureModel
	resp.Diagnostics.Append(req.Plan.Get(ctx, &plan)...)
	if resp.Diagnostics.HasError() {
		return
	}
	st, d := r.put(ctx, plan)
	resp.Diagnostics.Append(d...)
	if !d.HasError() {
		resp.Diagnostics.Append(resp.State.Set(ctx, st)...)
	}
}

func (r *featureResource) Read(ctx context.Context, req resource.ReadRequest, resp *resource.ReadResponse) {
	var state featureModel
	resp.Diagnostics.Append(req.State.Get(ctx, &state)...)
	if resp.Diagnostics.HasError() {
		return
	}
	var out apiFeature
	err := r.c.Get(ctx, "/api/v1/features/"+seg(state.ID.ValueString()), nil, &out)
	if removeIfGone(ctx, &resp.State, &resp.Diagnostics, "feature", err) {
		return
	}
	resp.Diagnostics.Append(resp.State.Set(ctx, out.model())...)
}

func (r *featureResource) Delete(ctx context.Context, req resource.DeleteRequest, resp *resource.DeleteResponse) {
	var state featureModel
	resp.Diagnostics.Append(req.State.Get(ctx, &state)...)
	if resp.Diagnostics.HasError() {
		return
	}
	if err := r.c.Delete(ctx, "/api/v1/features/"+seg(state.ID.ValueString()), nil); err != nil && !isGone(err) {
		addErr(&resp.Diagnostics, "Error deleting feature", err)
	}
}

// =============================================================================
// forgebase_plan
// =============================================================================

var (
	_ resource.Resource                = (*planResource)(nil)
	_ resource.ResourceWithConfigure   = (*planResource)(nil)
	_ resource.ResourceWithImportState = (*planResource)(nil)
)

// NewPlanResource returns the forgebase_plan resource.
func NewPlanResource() resource.Resource { return &planResource{} }

type planResource struct{ base }

type planFeatureModel struct {
	Feature    types.String `tfsdk:"feature"`
	Enabled    types.Bool   `tfsdk:"enabled"`
	Limit      types.Int64  `tfsdk:"limit"`
	Unit       types.String `tfsdk:"unit"`
	Period     types.String `tfsdk:"period"`
	WindowSecs types.Int64  `tfsdk:"window_secs"`
	TrialLimit types.Int64  `tfsdk:"trial_limit"`
}

var planFeatureObjType = types.ObjectType{AttrTypes: map[string]attr.Type{
	"feature": types.StringType, "enabled": types.BoolType, "limit": types.Int64Type, "unit": types.StringType,
	"period": types.StringType, "window_secs": types.Int64Type, "trial_limit": types.Int64Type,
}}

type planModel struct {
	ID          types.String         `tfsdk:"id"`
	Key         types.String         `tfsdk:"key"`
	Name        types.String         `tfsdk:"name"`
	Description types.String         `tfsdk:"description"`
	IsDefault   types.Bool           `tfsdk:"is_default"`
	Active      types.Bool           `tfsdk:"active"`
	TrialDays   types.Int64          `tfsdk:"trial_days"`
	Metadata    jsontypes.Normalized `tfsdk:"metadata"`
	Features    types.List           `tfsdk:"features"`
	CreatedAt   types.String         `tfsdk:"created_at"`
	UpdatedAt   types.String         `tfsdk:"updated_at"`
}

type apiPlanFeature struct {
	Feature    string  `json:"feature"`
	Enabled    bool    `json:"enabled"`
	Limit      *int64  `json:"limit"`
	Unit       string  `json:"unit,omitempty"`
	Period     *string `json:"period,omitempty"`
	WindowSecs *int64  `json:"window_secs,omitempty"`
	TrialLimit *int64  `json:"trial_limit,omitempty"`
}

type apiPlan struct {
	Key         string           `json:"key"`
	Name        string           `json:"name"`
	Description string           `json:"description"`
	IsDefault   bool             `json:"is_default"`
	Active      bool             `json:"active"`
	TrialDays   int64            `json:"trial_days"`
	Metadata    map[string]any   `json:"metadata"`
	Features    []apiPlanFeature `json:"features"`
	CreatedAt   string           `json:"created_at"`
	UpdatedAt   string           `json:"updated_at"`
}

func (r *planResource) Metadata(_ context.Context, req resource.MetadataRequest, resp *resource.MetadataResponse) {
	resp.TypeName = req.ProviderTypeName + "_plan"
}

func (r *planResource) Configure(_ context.Context, req resource.ConfigureRequest, resp *resource.ConfigureResponse) {
	r.configureResource(req, resp)
}

func (r *planResource) Schema(_ context.Context, _ resource.SchemaRequest, resp *resource.SchemaResponse) {
	resp.Schema = schema.Schema{
		Description: "An entitlement plan: a named bundle of features and limits that subscriptions grant. Requires a platform-administrator credential. " +
			"The plan's `features` list is replaced as a whole.",
		Attributes: map[string]schema.Attribute{
			"id": schema.StringAttribute{Computed: true, PlanModifiers: idState, Description: "The plan key."},
			"key": schema.StringAttribute{
				Required: true, PlanModifiers: replace, Description: "Plan key (for example `free`, `pro`). Cannot change.",
				Validators: []validator.String{stringvalidator.RegexMatches(slugRE, "must be 1-64 of [a-z0-9_-]")},
			},
			"name":        schema.StringAttribute{Optional: true, Computed: true, PlanModifiers: idState, Description: "Display name (defaults to the key)."},
			"description": schema.StringAttribute{Optional: true, Computed: true, Default: stringdefault.StaticString(""), Description: "Description."},
			"is_default":  schema.BoolAttribute{Optional: true, Computed: true, Default: booldefault.StaticBool(false), Description: "Plan of subjects without an entitled subscription. Making a plan the default unsets the previous default."},
			"active":      schema.BoolAttribute{Optional: true, Computed: true, Default: booldefault.StaticBool(true), Description: "The plan can be assigned."},
			"trial_days":  schema.Int64Attribute{Optional: true, Computed: true, Default: int64default.StaticInt64(0), Description: "Trial length offered with the plan.", Validators: []validator.Int64{int64validator.AtLeast(0)}},
			"metadata": schema.StringAttribute{
				Optional: true, Computed: true, CustomType: jsontypes.NormalizedType{}, Default: stringdefault.StaticString("{}"),
				Description: "Free-form data as a JSON object.",
			},
			"features": schema.ListNestedAttribute{
				Optional: true, Computed: true,
				Description: "Features the plan includes, with their limits.",
				NestedObject: schema.NestedAttributeObject{Attributes: map[string]schema.Attribute{
					"feature": schema.StringAttribute{Required: true, Description: "Feature key (a `forgebase_feature`)."},
					"enabled": schema.BoolAttribute{Optional: true, Computed: true, Default: booldefault.StaticBool(true), Description: "Included."},
					"limit":   schema.Int64Attribute{Optional: true, Description: "Limit per period. Omit for unlimited."},
					"unit":    schema.StringAttribute{Optional: true, Computed: true, Description: "Unit shown with the limit (defaults to the feature's)."},
					"period": schema.StringAttribute{
						Optional: true, Computed: true,
						Description: "Window of the limit: `none`, `hour`, `day`, `month`, `lifetime` or `rolling`. Default `month` for metered features, `none` otherwise.",
						Validators:  []validator.String{stringvalidator.OneOf("none", "hour", "day", "month", "lifetime", "rolling")},
					},
					"window_secs": schema.Int64Attribute{Optional: true, Description: "Length of a `rolling` window in seconds."},
					"trial_limit": schema.Int64Attribute{Optional: true, Description: "Allowance while the subscription is trialing (replaces `limit`)."},
				}},
			},
			"created_at": schema.StringAttribute{Computed: true, PlanModifiers: idState, Description: "Creation time."},
			"updated_at": schema.StringAttribute{Computed: true, Description: "Last change."},
		},
	}
}

func (r *planResource) ImportState(ctx context.Context, req resource.ImportStateRequest, resp *resource.ImportStateResponse) {
	resp.Diagnostics.Append(resp.State.SetAttribute(ctx, path.Root("id"), req.ID)...)
	resp.Diagnostics.Append(resp.State.SetAttribute(ctx, path.Root("key"), req.ID)...)
}

// planFromAPI builds the model; order keeps the feature order of the plan/state.
func planFromAPI(ctx context.Context, a apiPlan, order []planFeatureModel) (planModel, diagList) {
	var diags diagList
	idx := map[string]int{}
	for i, f := range order {
		idx[f.Feature.ValueString()] = i
	}
	feats := append([]apiPlanFeature(nil), a.Features...)
	sort.SliceStable(feats, func(i, j int) bool {
		a, aok := idx[feats[i].Feature]
		b, bok := idx[feats[j].Feature]
		switch {
		case aok && bok:
			return a < b
		case aok != bok:
			return aok
		}
		return feats[i].Feature < feats[j].Feature
	})
	fm := make([]planFeatureModel, len(feats))
	for i, f := range feats {
		fm[i] = planFeatureModel{
			Feature: types.StringValue(f.Feature), Enabled: types.BoolValue(f.Enabled), Limit: nullableInt64(f.Limit),
			Unit: types.StringValue(f.Unit), Period: nullableString(f.Period), WindowSecs: nullableInt64(f.WindowSecs), TrialLimit: nullableInt64(f.TrialLimit),
		}
		if f.Period == nil {
			fm[i].Period = types.StringValue("none")
		}
	}
	l, d := types.ListValueFrom(ctx, planFeatureObjType, fm)
	diags.Append(d...)
	meta := a.Metadata
	if meta == nil {
		meta = map[string]any{}
	}
	return planModel{
		ID: types.StringValue(a.Key), Key: types.StringValue(a.Key), Name: types.StringValue(a.Name), Description: types.StringValue(a.Description),
		IsDefault: types.BoolValue(a.IsDefault), Active: types.BoolValue(a.Active), TrialDays: types.Int64Value(a.TrialDays),
		Metadata: anyJSON(meta), Features: l, CreatedAt: types.StringValue(a.CreatedAt), UpdatedAt: types.StringValue(a.UpdatedAt),
	}, diags
}

func (r *planResource) put(ctx context.Context, m planModel) (planModel, diagList) {
	var diags diagList
	var fm []planFeatureModel
	if !m.Features.IsNull() && !m.Features.IsUnknown() {
		diags.Append(m.Features.ElementsAs(ctx, &fm, false)...)
	}
	feats := make([]map[string]any, 0, len(fm))
	for _, f := range fm {
		e := map[string]any{"feature": f.Feature.ValueString(), "enabled": f.Enabled.ValueBool()}
		if p := int64Ptr(f.Limit); p != nil {
			e["limit"] = *p
		}
		if p := strPtr(f.Unit); p != nil && *p != "" {
			e["unit"] = *p
		}
		if p := strPtr(f.Period); p != nil && *p != "" {
			e["period"] = *p
		}
		if p := int64Ptr(f.WindowSecs); p != nil {
			e["window_secs"] = *p
		}
		if p := int64Ptr(f.TrialLimit); p != nil {
			e["trial_limit"] = *p
		}
		feats = append(feats, e)
	}
	body := map[string]any{
		"description": m.Description.ValueString(), "is_default": m.IsDefault.ValueBool(), "active": m.Active.ValueBool(),
		"trial_days": m.TrialDays.ValueInt64(), "features": feats,
	}
	if p := strPtr(m.Name); p != nil && *p != "" {
		body["name"] = *p
	}
	if raw := jsonRaw(m.Metadata); raw != nil {
		body["metadata"] = raw
	}
	if diags.HasError() {
		return m, diags
	}
	var out apiPlan
	if err := r.c.Put(ctx, "/api/v1/plans/"+seg(m.Key.ValueString()), body, &out); err != nil {
		addErr(&diags, "Error saving plan", err)
		return m, diags
	}
	st, d := planFromAPI(ctx, out, fm)
	diags.Append(d...)
	st.Metadata = keepJSON(m.Metadata, st.Metadata)
	return st, diags
}

func (r *planResource) Create(ctx context.Context, req resource.CreateRequest, resp *resource.CreateResponse) {
	var plan planModel
	resp.Diagnostics.Append(req.Plan.Get(ctx, &plan)...)
	if resp.Diagnostics.HasError() {
		return
	}
	var existing apiPlan
	if err := r.c.Get(ctx, "/api/v1/plans/"+seg(plan.Key.ValueString()), nil, &existing); err == nil {
		resp.Diagnostics.AddError("Plan already exists", "Plan "+plan.Key.ValueString()+" exists already. Import it with `terraform import` instead.")
		return
	}
	st, d := r.put(ctx, plan)
	resp.Diagnostics.Append(d...)
	if !d.HasError() {
		resp.Diagnostics.Append(resp.State.Set(ctx, st)...)
	}
}

func (r *planResource) Update(ctx context.Context, req resource.UpdateRequest, resp *resource.UpdateResponse) {
	var plan planModel
	resp.Diagnostics.Append(req.Plan.Get(ctx, &plan)...)
	if resp.Diagnostics.HasError() {
		return
	}
	st, d := r.put(ctx, plan)
	resp.Diagnostics.Append(d...)
	if !d.HasError() {
		resp.Diagnostics.Append(resp.State.Set(ctx, st)...)
	}
}

func (r *planResource) Read(ctx context.Context, req resource.ReadRequest, resp *resource.ReadResponse) {
	var state planModel
	resp.Diagnostics.Append(req.State.Get(ctx, &state)...)
	if resp.Diagnostics.HasError() {
		return
	}
	var out apiPlan
	err := r.c.Get(ctx, "/api/v1/plans/"+seg(state.ID.ValueString()), nil, &out)
	if removeIfGone(ctx, &resp.State, &resp.Diagnostics, "plan", err) {
		return
	}
	var order []planFeatureModel
	if !state.Features.IsNull() && !state.Features.IsUnknown() {
		resp.Diagnostics.Append(state.Features.ElementsAs(ctx, &order, false)...)
	}
	st, d := planFromAPI(ctx, out, order)
	resp.Diagnostics.Append(d...)
	st.Metadata = keepJSON(state.Metadata, st.Metadata)
	resp.Diagnostics.Append(resp.State.Set(ctx, st)...)
}

func (r *planResource) Delete(ctx context.Context, req resource.DeleteRequest, resp *resource.DeleteResponse) {
	var state planModel
	resp.Diagnostics.Append(req.State.Get(ctx, &state)...)
	if resp.Diagnostics.HasError() {
		return
	}
	if err := r.c.Delete(ctx, "/api/v1/plans/"+seg(state.ID.ValueString()), nil); err != nil && !isGone(err) {
		addErr(&resp.Diagnostics, "Error deleting plan", err)
	}
}
