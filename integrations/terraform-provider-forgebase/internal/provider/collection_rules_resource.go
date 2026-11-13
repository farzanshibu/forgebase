package provider

import (
	"context"

	"github.com/hashicorp/terraform-plugin-framework/attr"
	"github.com/hashicorp/terraform-plugin-framework/path"
	"github.com/hashicorp/terraform-plugin-framework/resource"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/planmodifier"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/stringplanmodifier"
	"github.com/hashicorp/terraform-plugin-framework/types"
)

var (
	_ resource.Resource                = (*collectionRulesResource)(nil)
	_ resource.ResourceWithConfigure   = (*collectionRulesResource)(nil)
	_ resource.ResourceWithImportState = (*collectionRulesResource)(nil)
)

// NewCollectionRulesResource returns the forgebase_collection_rules resource.
func NewCollectionRulesResource() resource.Resource { return &collectionRulesResource{} }

type collectionRulesResource struct{ base }

type fieldRuleModel struct {
	Read  types.String `tfsdk:"read"`
	Write types.String `tfsdk:"write"`
}

var fieldRuleObjType = types.ObjectType{AttrTypes: map[string]attr.Type{"read": types.StringType, "write": types.StringType}}

type collectionRulesModel struct {
	ID         types.String `tfsdk:"id"`
	Collection types.String `tfsdk:"collection"`
	ListRule   types.String `tfsdk:"list_rule"`
	ViewRule   types.String `tfsdk:"view_rule"`
	CreateRule types.String `tfsdk:"create_rule"`
	UpdateRule types.String `tfsdk:"update_rule"`
	DeleteRule types.String `tfsdk:"delete_rule"`
	Fields     types.Map    `tfsdk:"field_rules"`
}

type apiPolicy struct {
	Collection string                  `json:"collection"`
	ListRule   *string                 `json:"list_rule"`
	ViewRule   *string                 `json:"view_rule"`
	CreateRule *string                 `json:"create_rule"`
	UpdateRule *string                 `json:"update_rule"`
	DeleteRule *string                 `json:"delete_rule"`
	Fields     map[string]apiFieldRule `json:"fields"`
}

type apiFieldRule struct {
	Read  *string `json:"read"`
	Write *string `json:"write"`
}

func (r *collectionRulesResource) Metadata(_ context.Context, req resource.MetadataRequest, resp *resource.MetadataResponse) {
	resp.TypeName = req.ProviderTypeName + "_collection_rules"
}

func (r *collectionRulesResource) Configure(_ context.Context, req resource.ConfigureRequest, resp *resource.ConfigureResponse) {
	r.configureResource(req, resp)
}

func (r *collectionRulesResource) Schema(_ context.Context, _ resource.SchemaRequest, resp *resource.SchemaResponse) {
	rule := func(what string) schema.StringAttribute {
		return schema.StringAttribute{
			Optional: true,
			Description: "Rule for " + what + ". Omit (null) to lock it: only privileged callers pass. " +
				"An empty string `\"\"` opens it to everyone. Any other value is a filter expression (for example `@request.auth.id != \"\"`).",
		}
	}
	resp.Schema = schema.Schema{
		Description: "The access rules of one collection (list, view, create, update, delete, and per-field read/write rules). " +
			"Requires a super-admin credential. Destroying the resource removes every rule, which locks the collection again.",
		Attributes: map[string]schema.Attribute{
			"id":          schema.StringAttribute{Computed: true, PlanModifiers: []planmodifier.String{stringplanmodifier.UseStateForUnknown()}, Description: "The collection name."},
			"collection":  schema.StringAttribute{Required: true, Description: "Collection name.", PlanModifiers: []planmodifier.String{stringplanmodifier.RequiresReplace()}},
			"list_rule":   rule("listing and searching records"),
			"view_rule":   rule("reading one record"),
			"create_rule": rule("creating records"),
			"update_rule": rule("updating records"),
			"delete_rule": rule("deleting records"),
			"field_rules": schema.MapNestedAttribute{
				Optional:    true,
				Description: "Per-field rules keyed by field name. Field rules may only use `@request` values, not record contents.",
				NestedObject: schema.NestedAttributeObject{Attributes: map[string]schema.Attribute{
					"read":  rule("reading the field"),
					"write": rule("writing the field"),
				}},
			},
		},
	}
}

func (r *collectionRulesResource) ImportState(ctx context.Context, req resource.ImportStateRequest, resp *resource.ImportStateResponse) {
	resp.Diagnostics.Append(resp.State.SetAttribute(ctx, path.Root("id"), req.ID)...)
	resp.Diagnostics.Append(resp.State.SetAttribute(ctx, path.Root("collection"), req.ID)...)
}

func (m collectionRulesModel) body(ctx context.Context) (map[string]any, diagList) {
	var diags diagList
	body := map[string]any{
		"list_rule": strPtr(m.ListRule), "view_rule": strPtr(m.ViewRule), "create_rule": strPtr(m.CreateRule),
		"update_rule": strPtr(m.UpdateRule), "delete_rule": strPtr(m.DeleteRule),
	}
	fields := map[string]apiFieldRule{}
	if !m.Fields.IsNull() && !m.Fields.IsUnknown() {
		var fr map[string]fieldRuleModel
		diags.Append(m.Fields.ElementsAs(ctx, &fr, false)...)
		for k, v := range fr {
			fields[k] = apiFieldRule{Read: strPtr(v.Read), Write: strPtr(v.Write)}
		}
	}
	body["fields"] = fields
	return body, diags
}

func rulesState(ctx context.Context, p apiPolicy, planFields types.Map) (collectionRulesModel, diagList) {
	var diags diagList
	m := collectionRulesModel{
		ID: types.StringValue(p.Collection), Collection: types.StringValue(p.Collection),
		ListRule: nullableString(p.ListRule), ViewRule: nullableString(p.ViewRule), CreateRule: nullableString(p.CreateRule),
		UpdateRule: nullableString(p.UpdateRule), DeleteRule: nullableString(p.DeleteRule),
	}
	// An empty field-rule set is null, so `field_rules` may be omitted.
	if len(p.Fields) == 0 {
		m.Fields = types.MapNull(fieldRuleObjType)
		return m, diags
	}
	fr := map[string]fieldRuleModel{}
	for k, v := range p.Fields {
		if v.Read == nil && v.Write == nil {
			continue
		}
		fr[k] = fieldRuleModel{Read: nullableString(v.Read), Write: nullableString(v.Write)}
	}
	if len(fr) == 0 {
		m.Fields = types.MapNull(fieldRuleObjType)
		return m, diags
	}
	mv, d := types.MapValueFrom(ctx, fieldRuleObjType, fr)
	diags.Append(d...)
	m.Fields = mv
	return m, diags
}

func (r *collectionRulesResource) put(ctx context.Context, plan collectionRulesModel) (collectionRulesModel, diagList) {
	body, diags := plan.body(ctx)
	if diags.HasError() {
		return plan, diags
	}
	var out apiPolicy
	if err := r.c.Put(ctx, "/api/v1/policies/"+seg(plan.Collection.ValueString()), body, &out); err != nil {
		addErr(&diags, "Error setting collection rules", err)
		return plan, diags
	}
	if out.Collection == "" {
		out.Collection = plan.Collection.ValueString()
	}
	st, d := rulesState(ctx, out, plan.Fields)
	diags.Append(d...)
	return st, diags
}

func (r *collectionRulesResource) Create(ctx context.Context, req resource.CreateRequest, resp *resource.CreateResponse) {
	var plan collectionRulesModel
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

func (r *collectionRulesResource) Update(ctx context.Context, req resource.UpdateRequest, resp *resource.UpdateResponse) {
	var plan collectionRulesModel
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

func (r *collectionRulesResource) Read(ctx context.Context, req resource.ReadRequest, resp *resource.ReadResponse) {
	var prior collectionRulesModel
	resp.Diagnostics.Append(req.State.Get(ctx, &prior)...)
	if resp.Diagnostics.HasError() {
		return
	}
	var out apiPolicy
	err := r.c.Get(ctx, "/api/v1/policies/"+seg(prior.ID.ValueString()), nil, &out)
	if removeIfGone(ctx, &resp.State, &resp.Diagnostics, "collection rules", err) {
		return
	}
	if out.Collection == "" {
		out.Collection = prior.ID.ValueString()
	}
	st, d := rulesState(ctx, out, prior.Fields)
	resp.Diagnostics.Append(d...)
	resp.Diagnostics.Append(resp.State.Set(ctx, st)...)
}

func (r *collectionRulesResource) Delete(ctx context.Context, req resource.DeleteRequest, resp *resource.DeleteResponse) {
	var state collectionRulesModel
	resp.Diagnostics.Append(req.State.Get(ctx, &state)...)
	if resp.Diagnostics.HasError() {
		return
	}
	if err := r.c.Delete(ctx, "/api/v1/policies/"+seg(state.ID.ValueString()), nil); err != nil && !isGone(err) {
		addErr(&resp.Diagnostics, "Error removing collection rules", err)
	}
}
