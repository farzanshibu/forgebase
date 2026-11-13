package provider

import (
	"context"
	"encoding/json"
	"strings"

	"github.com/hashicorp/terraform-plugin-framework-validators/stringvalidator"
	"github.com/hashicorp/terraform-plugin-framework/attr"
	"github.com/hashicorp/terraform-plugin-framework/path"
	"github.com/hashicorp/terraform-plugin-framework/resource"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/setdefault"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/stringdefault"
	"github.com/hashicorp/terraform-plugin-framework/schema/validator"
	"github.com/hashicorp/terraform-plugin-framework/types"
)

// =============================================================================
// forgebase_role
// =============================================================================

var (
	_ resource.Resource                = (*roleResource)(nil)
	_ resource.ResourceWithConfigure   = (*roleResource)(nil)
	_ resource.ResourceWithImportState = (*roleResource)(nil)
)

// NewRoleResource returns the forgebase_role resource.
func NewRoleResource() resource.Resource { return &roleResource{} }

type roleResource struct{ base }

type roleModel struct {
	ID          types.String `tfsdk:"id"`
	Key         types.String `tfsdk:"key"`
	Org         types.String `tfsdk:"org"`
	Name        types.String `tfsdk:"name"`
	Description types.String `tfsdk:"description"`
	Allow       types.Set    `tfsdk:"allow"`
	Deny        types.Set    `tfsdk:"deny"`
	System      types.Bool   `tfsdk:"system"`
}

type apiRole struct {
	ID          string   `json:"id"`
	OrgID       *string  `json:"org_id"`
	Key         string   `json:"key"`
	Name        string   `json:"name"`
	Description string   `json:"description"`
	Allow       []string `json:"allow"`
	Deny        []string `json:"deny"`
	System      bool     `json:"system"`
}

func (r *roleResource) Metadata(_ context.Context, req resource.MetadataRequest, resp *resource.MetadataResponse) {
	resp.TypeName = req.ProviderTypeName + "_role"
}

func (r *roleResource) Configure(_ context.Context, req resource.ConfigureRequest, resp *resource.ConfigureResponse) {
	r.configureResource(req, resp)
}

func (r *roleResource) Schema(_ context.Context, _ resource.SchemaRequest, resp *resource.SchemaResponse) {
	empty := setdefault.StaticValue(types.SetValueMust(types.StringType, []attr.Value{}))
	resp.Schema = schema.Schema{
		Description: "A custom role: a named set of permission patterns. Without `org` it is a realm role (needs a super-admin credential); with `org` it is " +
			"an organization role. Roles seeded by the server can be imported, but only super-admins may change them.",
		Attributes: map[string]schema.Attribute{
			"id": schema.StringAttribute{Computed: true, PlanModifiers: idState, Description: "`<key>` for a realm role, `<org>/<key>` for an organization role."},
			"key": schema.StringAttribute{
				Required: true, PlanModifiers: replace, Description: "Role key: 1-64 of `a-z`, `0-9`, `-`, `_`. Cannot change.",
				Validators: []validator.String{stringvalidator.RegexMatches(slugRE, "must be 1-64 of [a-z0-9_-]")},
			},
			"org":         schema.StringAttribute{Optional: true, PlanModifiers: replace, Description: "Organization id for an organization role. Omit for a realm role."},
			"name":        schema.StringAttribute{Optional: true, Computed: true, PlanModifiers: idState, Description: "Display name (defaults to the key)."},
			"description": schema.StringAttribute{Optional: true, Computed: true, Default: stringdefault.StaticString(""), Description: "Description."},
			"allow":       schema.SetAttribute{Optional: true, Computed: true, ElementType: types.StringType, Default: empty, Description: "Permission patterns granted (for example `collections:read`, `storage:*`)."},
			"deny":        schema.SetAttribute{Optional: true, Computed: true, ElementType: types.StringType, Default: empty, Description: "Permission patterns denied. A deny wins over an allow."},
			"system":      schema.BoolAttribute{Computed: true, Description: "Seeded by the server."},
		},
	}
}

func roleID(org, key string) string {
	if org == "" {
		return key
	}
	return org + "/" + key
}

func (r *roleResource) basePath(org string) string {
	if org == "" {
		return "/api/v1/roles"
	}
	return "/api/v1/orgs/" + seg(org) + "/roles"
}

func (r *roleResource) ImportState(ctx context.Context, req resource.ImportStateRequest, resp *resource.ImportStateResponse) {
	org, key := "", req.ID
	if o, k, ok := splitID(req.ID); ok {
		org, key = o, k
	}
	resp.Diagnostics.Append(resp.State.SetAttribute(ctx, path.Root("id"), req.ID)...)
	resp.Diagnostics.Append(resp.State.SetAttribute(ctx, path.Root("key"), key)...)
	if org != "" {
		resp.Diagnostics.Append(resp.State.SetAttribute(ctx, path.Root("org"), org)...)
	}
}

func (a apiRole) model(org string) roleModel {
	m := roleModel{
		ID: types.StringValue(roleID(org, a.Key)), Key: types.StringValue(a.Key), Org: types.StringNull(),
		Name: types.StringValue(a.Name), Description: types.StringValue(a.Description),
		Allow: strSet(a.Allow), Deny: strSet(a.Deny), System: types.BoolValue(a.System),
	}
	if org != "" {
		m.Org = types.StringValue(org)
	}
	return m
}

func (r *roleResource) put(ctx context.Context, m roleModel) (roleModel, diagList) {
	var diags diagList
	org := m.Org.ValueString()
	allow, deny := setStrings(m.Allow), setStrings(m.Deny)
	if allow == nil {
		allow = []string{}
	}
	if deny == nil {
		deny = []string{}
	}
	body := map[string]any{"key": m.Key.ValueString(), "description": m.Description.ValueString(), "allow": allow, "deny": deny}
	if p := strPtr(m.Name); p != nil && *p != "" {
		body["name"] = *p
	}
	var out apiRole
	if err := r.c.Put(ctx, r.basePath(org), body, &out); err != nil {
		addErr(&diags, "Error saving role", err)
		return m, diags
	}
	return out.model(org), diags
}

func (r *roleResource) Create(ctx context.Context, req resource.CreateRequest, resp *resource.CreateResponse) {
	var plan roleModel
	resp.Diagnostics.Append(req.Plan.Get(ctx, &plan)...)
	if resp.Diagnostics.HasError() {
		return
	}
	// PUT replaces a role of the same key: refuse to adopt an existing one silently.
	if existing, err := r.lookup(ctx, plan.Org.ValueString(), plan.Key.ValueString()); err == nil && existing != nil {
		resp.Diagnostics.AddError("Role already exists",
			"A role with key "+plan.Key.ValueString()+" exists already. Import it with `terraform import` instead.")
		return
	}
	st, d := r.put(ctx, plan)
	resp.Diagnostics.Append(d...)
	if !d.HasError() {
		resp.Diagnostics.Append(resp.State.Set(ctx, st)...)
	}
}

func (r *roleResource) Update(ctx context.Context, req resource.UpdateRequest, resp *resource.UpdateResponse) {
	var plan roleModel
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

func (r *roleResource) lookup(ctx context.Context, org, key string) (*apiRole, error) {
	var raw json.RawMessage
	if err := r.c.Get(ctx, r.basePath(org), nil, &raw); err != nil {
		return nil, err
	}
	roles, err := decodeList[apiRole](raw)
	if err != nil {
		return nil, err
	}
	for i := range roles {
		own := ""
		if roles[i].OrgID != nil {
			own = *roles[i].OrgID
		}
		if roles[i].Key == key && own == org {
			return &roles[i], nil
		}
	}
	return nil, nil
}

func (r *roleResource) Read(ctx context.Context, req resource.ReadRequest, resp *resource.ReadResponse) {
	var state roleModel
	resp.Diagnostics.Append(req.State.Get(ctx, &state)...)
	if resp.Diagnostics.HasError() {
		return
	}
	org := state.Org.ValueString()
	found, err := r.lookup(ctx, org, state.Key.ValueString())
	if err != nil {
		removeIfGone(ctx, &resp.State, &resp.Diagnostics, "role", err)
		return
	}
	if found == nil {
		resp.State.RemoveResource(ctx)
		return
	}
	resp.Diagnostics.Append(resp.State.Set(ctx, found.model(org))...)
}

func (r *roleResource) Delete(ctx context.Context, req resource.DeleteRequest, resp *resource.DeleteResponse) {
	var state roleModel
	resp.Diagnostics.Append(req.State.Get(ctx, &state)...)
	if resp.Diagnostics.HasError() {
		return
	}
	if err := r.c.Delete(ctx, r.basePath(state.Org.ValueString())+"/"+seg(state.Key.ValueString()), nil); err != nil && !isGone(err) {
		addErr(&resp.Diagnostics, "Error deleting role", err)
	}
}

// =============================================================================
// forgebase_org
// =============================================================================

var (
	_ resource.Resource                = (*orgResource)(nil)
	_ resource.ResourceWithConfigure   = (*orgResource)(nil)
	_ resource.ResourceWithImportState = (*orgResource)(nil)
)

// NewOrgResource returns the forgebase_org resource.
func NewOrgResource() resource.Resource { return &orgResource{} }

type orgResource struct{ base }

type orgModel struct {
	ID      types.String `tfsdk:"id"`
	Slug    types.String `tfsdk:"slug"`
	Name    types.String `tfsdk:"name"`
	RealmID types.String `tfsdk:"realm_id"`
}

type apiOrg struct {
	ID      string `json:"id"`
	RealmID string `json:"realm_id"`
	Slug    string `json:"slug"`
	Name    string `json:"name"`
}

func (a apiOrg) model() orgModel {
	return orgModel{ID: types.StringValue(a.ID), Slug: types.StringValue(a.Slug), Name: types.StringValue(a.Name), RealmID: types.StringValue(a.RealmID)}
}

func (r *orgResource) Metadata(_ context.Context, req resource.MetadataRequest, resp *resource.MetadataResponse) {
	resp.TypeName = req.ProviderTypeName + "_org"
}

func (r *orgResource) Configure(_ context.Context, req resource.ConfigureRequest, resp *resource.ConfigureResponse) {
	r.configureResource(req, resp)
}

func (r *orgResource) Schema(_ context.Context, _ resource.SchemaRequest, resp *resource.SchemaResponse) {
	resp.Schema = schema.Schema{
		Description: "An organization: the owner of projects, teams, members and roles. The credential that creates it becomes its owner.",
		Attributes: map[string]schema.Attribute{
			"id": schema.StringAttribute{Computed: true, PlanModifiers: idState, Description: "Server-assigned organization id."},
			"slug": schema.StringAttribute{
				Required: true, PlanModifiers: replace, Description: "URL-safe name: 1-64 of `a-z`, `0-9`, `-`, `_`. Cannot change.",
				Validators: []validator.String{stringvalidator.RegexMatches(slugRE, "must be 1-64 of [a-z0-9_-]")},
			},
			"name":     schema.StringAttribute{Required: true, Description: "Display name (1-100 characters).", Validators: []validator.String{stringvalidator.LengthBetween(1, 100)}},
			"realm_id": schema.StringAttribute{Computed: true, PlanModifiers: idState, Description: "Realm the organization belongs to."},
		},
	}
}

func (r *orgResource) ImportState(ctx context.Context, req resource.ImportStateRequest, resp *resource.ImportStateResponse) {
	resp.Diagnostics.Append(resp.State.SetAttribute(ctx, path.Root("id"), req.ID)...)
}

func (r *orgResource) Create(ctx context.Context, req resource.CreateRequest, resp *resource.CreateResponse) {
	var plan orgModel
	resp.Diagnostics.Append(req.Plan.Get(ctx, &plan)...)
	if resp.Diagnostics.HasError() {
		return
	}
	var out apiOrg
	if err := r.c.Post(ctx, "/api/v1/orgs", map[string]string{"slug": plan.Slug.ValueString(), "name": plan.Name.ValueString()}, &out); err != nil {
		addErr(&resp.Diagnostics, "Error creating organization", err)
		return
	}
	resp.Diagnostics.Append(resp.State.Set(ctx, out.model())...)
}

func (r *orgResource) Read(ctx context.Context, req resource.ReadRequest, resp *resource.ReadResponse) {
	var state orgModel
	resp.Diagnostics.Append(req.State.Get(ctx, &state)...)
	if resp.Diagnostics.HasError() {
		return
	}
	var out *apiOrg
	err := r.c.Get(ctx, "/api/v1/orgs/"+seg(state.ID.ValueString()), nil, &out)
	if err != nil {
		removeIfGone(ctx, &resp.State, &resp.Diagnostics, "organization", err)
		return
	}
	if out == nil { // the server answers `null` for an unknown id in some paths
		resp.State.RemoveResource(ctx)
		return
	}
	resp.Diagnostics.Append(resp.State.Set(ctx, out.model())...)
}

func (r *orgResource) Update(ctx context.Context, req resource.UpdateRequest, resp *resource.UpdateResponse) {
	var plan, state orgModel
	resp.Diagnostics.Append(req.Plan.Get(ctx, &plan)...)
	resp.Diagnostics.Append(req.State.Get(ctx, &state)...)
	if resp.Diagnostics.HasError() {
		return
	}
	var out apiOrg
	if err := r.c.Patch(ctx, "/api/v1/orgs/"+seg(state.ID.ValueString()), map[string]string{"name": plan.Name.ValueString()}, &out); err != nil {
		addErr(&resp.Diagnostics, "Error updating organization", err)
		return
	}
	resp.Diagnostics.Append(resp.State.Set(ctx, out.model())...)
}

func (r *orgResource) Delete(ctx context.Context, req resource.DeleteRequest, resp *resource.DeleteResponse) {
	var state orgModel
	resp.Diagnostics.Append(req.State.Get(ctx, &state)...)
	if resp.Diagnostics.HasError() {
		return
	}
	if err := r.c.Delete(ctx, "/api/v1/orgs/"+seg(state.ID.ValueString()), nil); err != nil && !isGone(err) {
		addErr(&resp.Diagnostics, "Error deleting organization", err)
	}
}

// =============================================================================
// forgebase_project
// =============================================================================

var (
	_ resource.Resource                = (*projectResource)(nil)
	_ resource.ResourceWithConfigure   = (*projectResource)(nil)
	_ resource.ResourceWithImportState = (*projectResource)(nil)
)

// NewProjectResource returns the forgebase_project resource.
func NewProjectResource() resource.Resource { return &projectResource{} }

type projectResource struct{ base }

type environmentModel struct {
	ID   types.String `tfsdk:"id"`
	Name types.String `tfsdk:"name"`
}

var environmentObjType = types.ObjectType{AttrTypes: map[string]attr.Type{"id": types.StringType, "name": types.StringType}}

type projectModel struct {
	ID           types.String `tfsdk:"id"`
	Slug         types.String `tfsdk:"slug"`
	Name         types.String `tfsdk:"name"`
	Org          types.String `tfsdk:"org"`
	RealmID      types.String `tfsdk:"realm_id"`
	Environments types.List   `tfsdk:"environments"`
}

type apiProject struct {
	ID           string  `json:"id"`
	RealmID      string  `json:"realm_id"`
	OrgID        *string `json:"org_id"`
	Slug         string  `json:"slug"`
	Name         string  `json:"name"`
	Environments []struct {
		ID   string `json:"id"`
		Name string `json:"name"`
	} `json:"environments"`
}

func (a apiProject) model(ctx context.Context) (projectModel, diagList) {
	envs := make([]environmentModel, len(a.Environments))
	for i, e := range a.Environments {
		envs[i] = environmentModel{ID: types.StringValue(e.ID), Name: types.StringValue(e.Name)}
	}
	l, d := types.ListValueFrom(ctx, environmentObjType, envs)
	m := projectModel{
		ID: types.StringValue(a.ID), Slug: types.StringValue(a.Slug), Name: types.StringValue(a.Name),
		Org: nullableString(a.OrgID), RealmID: types.StringValue(a.RealmID), Environments: l,
	}
	return m, d
}

func (r *projectResource) Metadata(_ context.Context, req resource.MetadataRequest, resp *resource.MetadataResponse) {
	resp.TypeName = req.ProviderTypeName + "_project"
}

func (r *projectResource) Configure(_ context.Context, req resource.ConfigureRequest, resp *resource.ConfigureResponse) {
	r.configureResource(req, resp)
}

func (r *projectResource) Schema(_ context.Context, _ resource.SchemaRequest, resp *resource.SchemaResponse) {
	resp.Schema = schema.Schema{
		Description: "A project: a group of environments (tenants). A new project gets `development`, `staging` and `production` environments. " +
			"The server has no update call for projects, so every change replaces it. Environments cannot be added or removed individually through the provider (the API only creates them).",
		Attributes: map[string]schema.Attribute{
			"id": schema.StringAttribute{Computed: true, PlanModifiers: idState, Description: "Server-assigned project id."},
			"slug": schema.StringAttribute{
				Required: true, PlanModifiers: replace, Description: "URL-safe name: 1-64 of `a-z`, `0-9`, `-`, `_`.",
				Validators: []validator.String{stringvalidator.RegexMatches(slugRE, "must be 1-64 of [a-z0-9_-]")},
			},
			"name":     schema.StringAttribute{Required: true, PlanModifiers: replace, Description: "Display name (1-100 characters).", Validators: []validator.String{stringvalidator.LengthBetween(1, 100)}},
			"org":      schema.StringAttribute{Optional: true, PlanModifiers: replace, Description: "Organization id. Omit for a personal project of the calling user."},
			"realm_id": schema.StringAttribute{Computed: true, PlanModifiers: idState, Description: "Realm the project belongs to."},
			"environments": schema.ListNestedAttribute{
				Computed:    true,
				Description: "Environments of the project; use an environment's `id` as the tenant (`X-Forgebase-Tenant`).",
				NestedObject: schema.NestedAttributeObject{Attributes: map[string]schema.Attribute{
					"id":   schema.StringAttribute{Computed: true, Description: "Environment (tenant) id."},
					"name": schema.StringAttribute{Computed: true, Description: "Environment name."},
				}},
			},
		},
	}
}

func (r *projectResource) ImportState(ctx context.Context, req resource.ImportStateRequest, resp *resource.ImportStateResponse) {
	resp.Diagnostics.Append(resp.State.SetAttribute(ctx, path.Root("id"), req.ID)...)
}

func (r *projectResource) Create(ctx context.Context, req resource.CreateRequest, resp *resource.CreateResponse) {
	var plan projectModel
	resp.Diagnostics.Append(req.Plan.Get(ctx, &plan)...)
	if resp.Diagnostics.HasError() {
		return
	}
	p := "/api/v1/projects"
	if o := strPtr(plan.Org); o != nil {
		p = "/api/v1/orgs/" + seg(*o) + "/projects"
	}
	var created apiProject
	if err := r.c.Post(ctx, p, map[string]string{"slug": plan.Slug.ValueString(), "name": plan.Name.ValueString()}, &created); err != nil {
		addErr(&resp.Diagnostics, "Error creating project", err)
		return
	}
	// The create answer may omit the environments; the project read has them.
	var full apiProject
	if err := r.c.Get(ctx, "/api/v1/projects/"+seg(created.ID), nil, &full); err == nil && full.ID != "" {
		created = full
	}
	st, d := created.model(ctx)
	resp.Diagnostics.Append(d...)
	resp.Diagnostics.Append(resp.State.Set(ctx, st)...)
}

func (r *projectResource) Read(ctx context.Context, req resource.ReadRequest, resp *resource.ReadResponse) {
	var state projectModel
	resp.Diagnostics.Append(req.State.Get(ctx, &state)...)
	if resp.Diagnostics.HasError() {
		return
	}
	var out *apiProject
	err := r.c.Get(ctx, "/api/v1/projects/"+seg(state.ID.ValueString()), nil, &out)
	if err != nil {
		removeIfGone(ctx, &resp.State, &resp.Diagnostics, "project", err)
		return
	}
	if out == nil {
		resp.State.RemoveResource(ctx)
		return
	}
	st, d := out.model(ctx)
	resp.Diagnostics.Append(d...)
	resp.Diagnostics.Append(resp.State.Set(ctx, st)...)
}

func (r *projectResource) Update(_ context.Context, _ resource.UpdateRequest, resp *resource.UpdateResponse) {
	resp.Diagnostics.AddError("Project cannot be updated in place", "Every attribute forces replacement; this is a provider bug.")
}

func (r *projectResource) Delete(ctx context.Context, req resource.DeleteRequest, resp *resource.DeleteResponse) {
	var state projectModel
	resp.Diagnostics.Append(req.State.Get(ctx, &state)...)
	if resp.Diagnostics.HasError() {
		return
	}
	if err := r.c.Delete(ctx, "/api/v1/projects/"+seg(state.ID.ValueString()), nil); err != nil && !isGone(err) {
		addErr(&resp.Diagnostics, "Error deleting project", err)
	}
}

var _ = strings.TrimSpace
