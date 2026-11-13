package provider

import (
	"context"
	"regexp"
	"sort"

	"github.com/hashicorp/terraform-plugin-framework-validators/int64validator"
	"github.com/hashicorp/terraform-plugin-framework-validators/stringvalidator"
	"github.com/hashicorp/terraform-plugin-framework/path"
	"github.com/hashicorp/terraform-plugin-framework/resource"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/booldefault"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/planmodifier"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/stringdefault"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/stringplanmodifier"
	"github.com/hashicorp/terraform-plugin-framework/schema/validator"
	"github.com/hashicorp/terraform-plugin-framework/types"

	"github.com/farzanshibu/forgebase/integrations/terraform-provider-forgebase/internal/client"
)

var (
	_            resource.Resource                = (*bucketResource)(nil)
	_            resource.ResourceWithConfigure   = (*bucketResource)(nil)
	_            resource.ResourceWithImportState = (*bucketResource)(nil)
	bucketNameRE                                  = regexp.MustCompile(`^[a-z0-9][a-z0-9._-]{0,62}$`)
)

// NewStorageBucketResource returns the forgebase_storage_bucket resource.
func NewStorageBucketResource() resource.Resource { return &bucketResource{} }

type bucketResource struct{ base }

type bucketModel struct {
	ID                  types.String `tfsdk:"id"`
	Name                types.String `tfsdk:"name"`
	Visibility          types.String `tfsdk:"visibility"`
	Versioning          types.Bool   `tfsdk:"versioning"`
	MaxObjectBytes      types.Int64  `tfsdk:"max_object_bytes"`
	AllowedContentTypes types.Set    `tfsdk:"allowed_content_types"`
	ForceDestroy        types.Bool   `tfsdk:"force_destroy"`
	Owner               types.String `tfsdk:"owner"`
	Tenant              types.String `tfsdk:"tenant"`
	CreatedAt           types.String `tfsdk:"created_at"`
	UpdatedAt           types.String `tfsdk:"updated_at"`
}

type apiBucket struct {
	Name                string   `json:"name"`
	Visibility          string   `json:"visibility"`
	Versioning          bool     `json:"versioning"`
	MaxObjectBytes      *int64   `json:"max_object_bytes"`
	AllowedContentTypes []string `json:"allowed_content_types"`
	Owner               string   `json:"owner"`
	Tenant              *string  `json:"tenant"`
	CreatedAt           string   `json:"created_at"`
	UpdatedAt           string   `json:"updated_at"`
}

func (r *bucketResource) Metadata(_ context.Context, req resource.MetadataRequest, resp *resource.MetadataResponse) {
	resp.TypeName = req.ProviderTypeName + "_storage_bucket"
}

func (r *bucketResource) Configure(_ context.Context, req resource.ConfigureRequest, resp *resource.ConfigureResponse) {
	r.configureResource(req, resp)
}

func (r *bucketResource) Schema(_ context.Context, _ resource.SchemaRequest, resp *resource.SchemaResponse) {
	resp.Schema = schema.Schema{
		Description: "An object-storage bucket and its default policy. Per-principal access lives in `forgebase_storage_bucket_acl`.",
		Attributes: map[string]schema.Attribute{
			"id": schema.StringAttribute{Computed: true, PlanModifiers: []planmodifier.String{stringplanmodifier.UseStateForUnknown()}, Description: "The bucket name."},
			"name": schema.StringAttribute{
				Required:      true,
				Description:   "Bucket name: 1-63 characters of `a-z`, `0-9`, `.`, `_` and `-`, starting with a letter or digit. Cannot change.",
				Validators:    []validator.String{stringvalidator.RegexMatches(bucketNameRE, "must be 1-63 characters of a-z, 0-9, '.', '_' and '-', starting with a letter or digit")},
				PlanModifiers: []planmodifier.String{stringplanmodifier.RequiresReplace()},
			},
			"visibility": schema.StringAttribute{
				Optional: true, Computed: true, Default: stringdefault.StaticString("private"),
				Description: "Default read access: `private` (default), `authenticated` or `public`.",
				Validators:  []validator.String{stringvalidator.OneOf("private", "authenticated", "public")},
			},
			"versioning": schema.BoolAttribute{Optional: true, Computed: true, Default: booldefault.StaticBool(false), Description: "Keep earlier versions of overwritten objects."},
			"max_object_bytes": schema.Int64Attribute{
				Optional: true, Description: "Largest object accepted, in bytes. Omit for no limit.",
				Validators: []validator.Int64{int64validator.AtLeast(1)},
			},
			"allowed_content_types": schema.SetAttribute{
				Optional: true, ElementType: types.StringType,
				Description: "Content types uploads may use (for example `image/png`). Omit to allow any.",
			},
			"force_destroy": schema.BoolAttribute{
				Optional: true, Computed: true, Default: booldefault.StaticBool(false),
				Description: "Delete the bucket's objects when destroying it. Without it the server refuses to delete a non-empty bucket.",
			},
			"owner":      schema.StringAttribute{Computed: true, PlanModifiers: []planmodifier.String{stringplanmodifier.UseStateForUnknown()}, Description: "Principal that created the bucket."},
			"tenant":     schema.StringAttribute{Computed: true, PlanModifiers: []planmodifier.String{stringplanmodifier.UseStateForUnknown()}, Description: "Tenant the bucket belongs to, if any."},
			"created_at": schema.StringAttribute{Computed: true, PlanModifiers: []planmodifier.String{stringplanmodifier.UseStateForUnknown()}, Description: "Creation time."},
			"updated_at": schema.StringAttribute{Computed: true, Description: "Last change."},
		},
	}
}

func (r *bucketResource) ImportState(ctx context.Context, req resource.ImportStateRequest, resp *resource.ImportStateResponse) {
	resp.Diagnostics.Append(resp.State.SetAttribute(ctx, path.Root("id"), req.ID)...)
	resp.Diagnostics.Append(resp.State.SetAttribute(ctx, path.Root("force_destroy"), false)...)
}

func bucketFromAPI(b apiBucket, force types.Bool) bucketModel {
	m := bucketModel{
		ID: types.StringValue(b.Name), Name: types.StringValue(b.Name), Visibility: types.StringValue(b.Visibility),
		Versioning: types.BoolValue(b.Versioning), MaxObjectBytes: nullableInt64(b.MaxObjectBytes),
		ForceDestroy: force, Owner: types.StringValue(b.Owner), Tenant: nullableString(b.Tenant),
		CreatedAt: types.StringValue(b.CreatedAt), UpdatedAt: types.StringValue(b.UpdatedAt),
	}
	if m.MaxObjectBytes.ValueInt64() == 0 {
		m.MaxObjectBytes = types.Int64Null()
	}
	if len(b.AllowedContentTypes) == 0 {
		m.AllowedContentTypes = types.SetNull(types.StringType)
	} else {
		m.AllowedContentTypes = strSet(b.AllowedContentTypes)
	}
	if m.ForceDestroy.IsNull() || m.ForceDestroy.IsUnknown() {
		m.ForceDestroy = types.BoolValue(false)
	}
	return m
}

func (r *bucketResource) Create(ctx context.Context, req resource.CreateRequest, resp *resource.CreateResponse) {
	var plan bucketModel
	resp.Diagnostics.Append(req.Plan.Get(ctx, &plan)...)
	if resp.Diagnostics.HasError() {
		return
	}
	body := map[string]any{"name": plan.Name.ValueString(), "visibility": plan.Visibility.ValueString(), "versioning": plan.Versioning.ValueBool()}
	if v := int64Ptr(plan.MaxObjectBytes); v != nil {
		body["max_object_bytes"] = *v
	}
	if ct := setStrings(plan.AllowedContentTypes); len(ct) > 0 {
		body["allowed_content_types"] = ct
	}
	var out apiBucket
	if err := r.c.Post(ctx, "/api/v1/storage/buckets", body, &out); err != nil {
		addErr(&resp.Diagnostics, "Error creating bucket", err)
		return
	}
	resp.Diagnostics.Append(resp.State.Set(ctx, bucketFromAPI(out, plan.ForceDestroy))...)
}

func (r *bucketResource) Read(ctx context.Context, req resource.ReadRequest, resp *resource.ReadResponse) {
	var state bucketModel
	resp.Diagnostics.Append(req.State.Get(ctx, &state)...)
	if resp.Diagnostics.HasError() {
		return
	}
	var out apiBucket
	err := r.c.Get(ctx, "/api/v1/storage/buckets/"+seg(state.ID.ValueString()), nil, &out)
	if removeIfGone(ctx, &resp.State, &resp.Diagnostics, "bucket", err) {
		return
	}
	resp.Diagnostics.Append(resp.State.Set(ctx, bucketFromAPI(out, state.ForceDestroy))...)
}

func (r *bucketResource) Update(ctx context.Context, req resource.UpdateRequest, resp *resource.UpdateResponse) {
	var plan, state bucketModel
	resp.Diagnostics.Append(req.Plan.Get(ctx, &plan)...)
	resp.Diagnostics.Append(req.State.Get(ctx, &state)...)
	if resp.Diagnostics.HasError() {
		return
	}
	// PATCH: 0 clears the size limit and an empty list clears the allow list.
	max := int64(0)
	if v := int64Ptr(plan.MaxObjectBytes); v != nil {
		max = *v
	}
	types_ := setStrings(plan.AllowedContentTypes)
	if types_ == nil {
		types_ = []string{}
	}
	body := map[string]any{
		"visibility": plan.Visibility.ValueString(), "versioning": plan.Versioning.ValueBool(),
		"max_object_bytes": max, "allowed_content_types": types_,
	}
	var out apiBucket
	if err := r.c.Patch(ctx, "/api/v1/storage/buckets/"+seg(state.ID.ValueString()), body, &out); err != nil {
		addErr(&resp.Diagnostics, "Error updating bucket", err)
		return
	}
	resp.Diagnostics.Append(resp.State.Set(ctx, bucketFromAPI(out, plan.ForceDestroy))...)
}

func (r *bucketResource) Delete(ctx context.Context, req resource.DeleteRequest, resp *resource.DeleteResponse) {
	var state bucketModel
	resp.Diagnostics.Append(req.State.Get(ctx, &state)...)
	if resp.Diagnostics.HasError() {
		return
	}
	var q client.Query
	if state.ForceDestroy.ValueBool() {
		q = client.Query{"force": "true"}
	}
	if err := r.c.Delete(ctx, "/api/v1/storage/buckets/"+seg(state.ID.ValueString()), q); err != nil && !isGone(err) {
		addErr(&resp.Diagnostics, "Error deleting bucket", err)
	}
}

func sortedStrings(in []string) []string {
	out := append([]string(nil), in...)
	sort.Strings(out)
	return out
}
