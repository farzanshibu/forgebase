package provider

import (
	"context"
	"regexp"
	"sort"

	"github.com/hashicorp/terraform-plugin-framework-validators/stringvalidator"
	"github.com/hashicorp/terraform-plugin-framework/attr"
	"github.com/hashicorp/terraform-plugin-framework/path"
	"github.com/hashicorp/terraform-plugin-framework/resource"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/planmodifier"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/stringplanmodifier"
	"github.com/hashicorp/terraform-plugin-framework/schema/validator"
	"github.com/hashicorp/terraform-plugin-framework/types"
)

var (
	_         resource.Resource                = (*bucketACLResource)(nil)
	_         resource.ResourceWithConfigure   = (*bucketACLResource)(nil)
	_         resource.ResourceWithImportState = (*bucketACLResource)(nil)
	granteeRE                                  = regexp.MustCompile(`^(public|authenticated|user:[^/\s]+/\S+|service:\S+|role:\S+)$`)
)

// NewStorageBucketACLResource returns the forgebase_storage_bucket_acl resource.
func NewStorageBucketACLResource() resource.Resource { return &bucketACLResource{} }

type bucketACLResource struct{ base }

type aclEntryModel struct {
	Prefix     types.String `tfsdk:"prefix"`
	Grantee    types.String `tfsdk:"grantee"`
	Permission types.String `tfsdk:"permission"`
}

var aclEntryObjType = types.ObjectType{AttrTypes: map[string]attr.Type{
	"prefix": types.StringType, "grantee": types.StringType, "permission": types.StringType,
}}

type bucketACLModel struct {
	ID      types.String `tfsdk:"id"`
	Bucket  types.String `tfsdk:"bucket"`
	Entries types.Set    `tfsdk:"entries"`
}

type apiACLEntry struct {
	Prefix     string `json:"prefix"`
	Grantee    string `json:"grantee"`
	Permission string `json:"permission"`
}

func (r *bucketACLResource) Metadata(_ context.Context, req resource.MetadataRequest, resp *resource.MetadataResponse) {
	resp.TypeName = req.ProviderTypeName + "_storage_bucket_acl"
}

func (r *bucketACLResource) Configure(_ context.Context, req resource.ConfigureRequest, resp *resource.ConfigureResponse) {
	r.configureResource(req, resp)
}

func (r *bucketACLResource) Schema(_ context.Context, _ resource.SchemaRequest, resp *resource.SchemaResponse) {
	resp.Schema = schema.Schema{
		Description: "The complete access-control list of one bucket. The resource owns the whole list: entries added outside Terraform are removed on the next apply. Destroying it empties the list.",
		Attributes: map[string]schema.Attribute{
			"id":     schema.StringAttribute{Computed: true, PlanModifiers: []planmodifier.String{stringplanmodifier.UseStateForUnknown()}, Description: "The bucket name."},
			"bucket": schema.StringAttribute{Required: true, Description: "Bucket name.", PlanModifiers: []planmodifier.String{stringplanmodifier.RequiresReplace()}},
			"entries": schema.SetNestedAttribute{
				Required:    true,
				Description: "Grants. A grant covers keys that start with `prefix` (the whole bucket when omitted).",
				NestedObject: schema.NestedAttributeObject{Attributes: map[string]schema.Attribute{
					"prefix": schema.StringAttribute{Optional: true, Description: "Key prefix the grant covers. Omit for the whole bucket."},
					"grantee": schema.StringAttribute{
						Required:    true,
						Description: "`user:<collection>/<id>`, `service:<id>`, `role:<name>`, `authenticated` or `public`.",
						Validators:  []validator.String{stringvalidator.RegexMatches(granteeRE, "must be user:<collection>/<id>, service:<id>, role:<name>, authenticated or public")},
					},
					"permission": schema.StringAttribute{
						Required:    true,
						Description: "`read`, `write` or `admin`.",
						Validators:  []validator.String{stringvalidator.OneOf("read", "write", "admin")},
					},
				}},
			},
		},
	}
}

func (r *bucketACLResource) ImportState(ctx context.Context, req resource.ImportStateRequest, resp *resource.ImportStateResponse) {
	resp.Diagnostics.Append(resp.State.SetAttribute(ctx, path.Root("id"), req.ID)...)
	resp.Diagnostics.Append(resp.State.SetAttribute(ctx, path.Root("bucket"), req.ID)...)
}

func (r *bucketACLResource) put(ctx context.Context, bucket string, entries types.Set) (bucketACLModel, diagList) {
	var diags diagList
	var em []aclEntryModel
	if !entries.IsNull() && !entries.IsUnknown() {
		diags.Append(entries.ElementsAs(ctx, &em, false)...)
	}
	api := make([]apiACLEntry, 0, len(em))
	for _, e := range em {
		api = append(api, apiACLEntry{Prefix: e.Prefix.ValueString(), Grantee: e.Grantee.ValueString(), Permission: e.Permission.ValueString()})
	}
	var out struct {
		Entries []apiACLEntry `json:"entries"`
	}
	if err := r.c.Put(ctx, "/api/v1/storage/buckets/"+seg(bucket)+"/acl", map[string]any{"entries": api}, &out); err != nil {
		addErr(&diags, "Error setting bucket ACL", err)
		return bucketACLModel{}, diags
	}
	m, d := aclState(ctx, bucket, out.Entries)
	diags.Append(d...)
	return m, diags
}

func aclState(ctx context.Context, bucket string, entries []apiACLEntry) (bucketACLModel, diagList) {
	var diags diagList
	sort.Slice(entries, func(i, j int) bool {
		if entries[i].Prefix != entries[j].Prefix {
			return entries[i].Prefix < entries[j].Prefix
		}
		return entries[i].Grantee < entries[j].Grantee
	})
	em := make([]aclEntryModel, len(entries))
	for i, e := range entries {
		p := types.StringNull()
		if e.Prefix != "" {
			p = types.StringValue(e.Prefix)
		}
		em[i] = aclEntryModel{Prefix: p, Grantee: types.StringValue(e.Grantee), Permission: types.StringValue(e.Permission)}
	}
	set, d := types.SetValueFrom(ctx, aclEntryObjType, em)
	diags.Append(d...)
	return bucketACLModel{ID: types.StringValue(bucket), Bucket: types.StringValue(bucket), Entries: set}, diags
}

func (r *bucketACLResource) Create(ctx context.Context, req resource.CreateRequest, resp *resource.CreateResponse) {
	var plan bucketACLModel
	resp.Diagnostics.Append(req.Plan.Get(ctx, &plan)...)
	if resp.Diagnostics.HasError() {
		return
	}
	st, d := r.put(ctx, plan.Bucket.ValueString(), plan.Entries)
	resp.Diagnostics.Append(d...)
	if !d.HasError() {
		resp.Diagnostics.Append(resp.State.Set(ctx, st)...)
	}
}

func (r *bucketACLResource) Update(ctx context.Context, req resource.UpdateRequest, resp *resource.UpdateResponse) {
	var plan bucketACLModel
	resp.Diagnostics.Append(req.Plan.Get(ctx, &plan)...)
	if resp.Diagnostics.HasError() {
		return
	}
	st, d := r.put(ctx, plan.Bucket.ValueString(), plan.Entries)
	resp.Diagnostics.Append(d...)
	if !d.HasError() {
		resp.Diagnostics.Append(resp.State.Set(ctx, st)...)
	}
}

func (r *bucketACLResource) Read(ctx context.Context, req resource.ReadRequest, resp *resource.ReadResponse) {
	var prior bucketACLModel
	resp.Diagnostics.Append(req.State.Get(ctx, &prior)...)
	if resp.Diagnostics.HasError() {
		return
	}
	var out struct {
		Entries []apiACLEntry `json:"entries"`
	}
	err := r.c.Get(ctx, "/api/v1/storage/buckets/"+seg(prior.ID.ValueString())+"/acl", nil, &out)
	if removeIfGone(ctx, &resp.State, &resp.Diagnostics, "bucket ACL", err) {
		return
	}
	st, d := aclState(ctx, prior.ID.ValueString(), out.Entries)
	resp.Diagnostics.Append(d...)
	resp.Diagnostics.Append(resp.State.Set(ctx, st)...)
}

func (r *bucketACLResource) Delete(ctx context.Context, req resource.DeleteRequest, resp *resource.DeleteResponse) {
	var state bucketACLModel
	resp.Diagnostics.Append(req.State.Get(ctx, &state)...)
	if resp.Diagnostics.HasError() {
		return
	}
	err := r.c.Put(ctx, "/api/v1/storage/buckets/"+seg(state.ID.ValueString())+"/acl", map[string]any{"entries": []apiACLEntry{}}, nil)
	if err != nil && !isGone(err) {
		addErr(&resp.Diagnostics, "Error clearing bucket ACL", err)
	}
}
