package provider

import (
	"context"
	"encoding/json"
	"fmt"

	"github.com/hashicorp/terraform-plugin-framework-jsontypes/jsontypes"
	"github.com/hashicorp/terraform-plugin-framework/attr"
	"github.com/hashicorp/terraform-plugin-framework/datasource"
	"github.com/hashicorp/terraform-plugin-framework/datasource/schema"
	"github.com/hashicorp/terraform-plugin-framework/path"
	"github.com/hashicorp/terraform-plugin-framework/types"
)

func dsAdd(d datasource.MetadataRequest, name string) string { return d.ProviderTypeName + "_" + name }

type ds struct{ base }

func (d *ds) Configure(_ context.Context, req datasource.ConfigureRequest, resp *datasource.ConfigureResponse) {
	d.configureDataSource(req, resp)
}

func dsString(desc string) schema.StringAttribute {
	return schema.StringAttribute{Computed: true, Description: desc}
}

// =============================================================================
// forgebase_collection
// =============================================================================

// NewCollectionDataSource returns the forgebase_collection data source.
func NewCollectionDataSource() datasource.DataSource { return &collectionDS{} }

type collectionDS struct{ ds }

func (d *collectionDS) Metadata(_ context.Context, req datasource.MetadataRequest, resp *datasource.MetadataResponse) {
	resp.TypeName = dsAdd(req, "collection")
}

func (d *collectionDS) Schema(_ context.Context, _ datasource.SchemaRequest, resp *datasource.SchemaResponse) {
	resp.Schema = schema.Schema{
		Description: "Looks up a collection by name or id.",
		Attributes: map[string]schema.Attribute{
			"id":            dsString("Server-assigned collection id."),
			"name":          schema.StringAttribute{Required: true, Description: "Collection name or id."},
			"type":          dsString("`base`, `auth` or `view`."),
			"schema_mode":   dsString("`strict` or `flexible`."),
			"versioned":     schema.BoolAttribute{Computed: true, Description: "Every record version is kept."},
			"tenant_scoped": schema.BoolAttribute{Computed: true, Description: "Rows are isolated per tenant."},
			"view_query":    dsString("SELECT of a view."),
			"fields": schema.ListNestedAttribute{Computed: true, Description: "Fields, including system fields.", NestedObject: schema.NestedAttributeObject{Attributes: map[string]schema.Attribute{
				"id": dsString("Field id."), "name": dsString("Field name."), "type": dsString("Field type."),
				"required":     schema.BoolAttribute{Computed: true, Description: "Required."},
				"unique":       schema.BoolAttribute{Computed: true, Description: "Unique."},
				"hidden":       schema.BoolAttribute{Computed: true, Description: "Hidden."},
				"default":      schema.StringAttribute{Computed: true, CustomType: jsontypes.NormalizedType{}, Description: "Default as JSON."},
				"options":      schema.StringAttribute{Computed: true, CustomType: jsontypes.NormalizedType{}, Description: "Type options as JSON."},
				"renamed_from": dsString("Unused in data sources."),
			}}},
			"version": schema.Int64Attribute{Computed: true, Description: "Schema version."},
			"created": dsString("Creation time."),
			"updated": dsString("Last schema change."),
		},
	}
}

func (d *collectionDS) Read(ctx context.Context, req datasource.ReadRequest, resp *datasource.ReadResponse) {
	var cfg collectionModel
	resp.Diagnostics.Append(req.Config.Get(ctx, &cfg)...)
	if resp.Diagnostics.HasError() {
		return
	}
	var out apiCollection
	if err := d.c.Get(ctx, "/api/v1/collections/"+seg(cfg.Name.ValueString()), nil, &out); err != nil {
		addErr(&resp.Diagnostics, "Error reading collection", err)
		return
	}
	st, diags := collectionState(ctx, out, nil, false)
	resp.Diagnostics.Append(diags...)
	resp.Diagnostics.Append(resp.State.Set(ctx, st)...)
}

// =============================================================================
// forgebase_storage_bucket
// =============================================================================

// NewStorageBucketDataSource returns the forgebase_storage_bucket data source.
func NewStorageBucketDataSource() datasource.DataSource { return &bucketDS{} }

type bucketDS struct{ ds }

func (d *bucketDS) Metadata(_ context.Context, req datasource.MetadataRequest, resp *datasource.MetadataResponse) {
	resp.TypeName = dsAdd(req, "storage_bucket")
}

func (d *bucketDS) Schema(_ context.Context, _ datasource.SchemaRequest, resp *datasource.SchemaResponse) {
	resp.Schema = schema.Schema{
		Description: "Looks up a storage bucket by name.",
		Attributes: map[string]schema.Attribute{
			"id": dsString("The bucket name."), "name": schema.StringAttribute{Required: true, Description: "Bucket name."},
			"visibility":            dsString("`private`, `authenticated` or `public`."),
			"versioning":            schema.BoolAttribute{Computed: true, Description: "Versioning is on."},
			"max_object_bytes":      schema.Int64Attribute{Computed: true, Description: "Size limit in bytes, if any."},
			"allowed_content_types": schema.SetAttribute{Computed: true, ElementType: types.StringType, Description: "Allowed content types, if restricted."},
			"force_destroy":         schema.BoolAttribute{Computed: true, Description: "Unused in data sources."},
			"owner":                 dsString("Creator."), "tenant": dsString("Tenant, if any."), "created_at": dsString("Creation time."), "updated_at": dsString("Last change."),
		},
	}
}

func (d *bucketDS) Read(ctx context.Context, req datasource.ReadRequest, resp *datasource.ReadResponse) {
	var cfg bucketModel
	resp.Diagnostics.Append(req.Config.Get(ctx, &cfg)...)
	if resp.Diagnostics.HasError() {
		return
	}
	var out apiBucket
	if err := d.c.Get(ctx, "/api/v1/storage/buckets/"+seg(cfg.Name.ValueString()), nil, &out); err != nil {
		addErr(&resp.Diagnostics, "Error reading bucket", err)
		return
	}
	resp.Diagnostics.Append(resp.State.Set(ctx, bucketFromAPI(out, types.BoolValue(false)))...)
}

// =============================================================================
// forgebase_queue_topic
// =============================================================================

// NewQueueTopicDataSource returns the forgebase_queue_topic data source.
func NewQueueTopicDataSource() datasource.DataSource { return &topicDS{} }

type topicDS struct{ ds }

func (d *topicDS) Metadata(_ context.Context, req datasource.MetadataRequest, resp *datasource.MetadataResponse) {
	resp.TypeName = dsAdd(req, "queue_topic")
}

func (d *topicDS) Schema(_ context.Context, _ datasource.SchemaRequest, resp *datasource.SchemaResponse) {
	resp.Schema = schema.Schema{
		Description: "Looks up a queue topic by name.",
		Attributes: map[string]schema.Attribute{
			"id": dsString("The topic name."), "name": schema.StringAttribute{Required: true, Description: "Topic name."},
			"retention_ms":      schema.Int64Attribute{Computed: true, Description: "Retention in milliseconds."},
			"retention_bytes":   schema.Int64Attribute{Computed: true, Description: "Size cap in bytes, if any."},
			"max_message_bytes": schema.Int64Attribute{Computed: true, Description: "Largest message in bytes."},
			"dedup_window_ms":   schema.Int64Attribute{Computed: true, Description: "Deduplication window in milliseconds."},
			"labels":            schema.MapAttribute{Computed: true, ElementType: types.StringType, Description: "Labels."},
			"schema":            schema.StringAttribute{Computed: true, CustomType: jsontypes.NormalizedType{}, Description: "Message JSON Schema, if any."},
			"source": schema.SingleNestedAttribute{Computed: true, Description: "Record source, if the topic is fed by a collection.", Attributes: map[string]schema.Attribute{
				"collection": dsString("Collection."), "ops": schema.ListAttribute{Computed: true, ElementType: types.StringType, Description: "Operations published."},
			}},
			"force_destroy": schema.BoolAttribute{Computed: true, Description: "Unused in data sources."},
			"created_by":    dsString("Creator."), "created_at": dsString("Creation time."), "updated_at": dsString("Last change."),
		},
	}
}

func (d *topicDS) Read(ctx context.Context, req datasource.ReadRequest, resp *datasource.ReadResponse) {
	var cfg topicModel
	resp.Diagnostics.Append(req.Config.Get(ctx, &cfg)...)
	if resp.Diagnostics.HasError() {
		return
	}
	var out apiTopic
	if err := d.c.Get(ctx, "/api/v1/queues/topics/"+seg(cfg.Name.ValueString()), nil, &out); err != nil {
		addErr(&resp.Diagnostics, "Error reading topic", err)
		return
	}
	st, diags := topicFromAPI(ctx, out, types.BoolValue(false))
	resp.Diagnostics.Append(diags...)
	resp.Diagnostics.Append(resp.State.Set(ctx, st)...)
}

// =============================================================================
// forgebase_function
// =============================================================================

// NewFunctionDataSource returns the forgebase_function data source.
func NewFunctionDataSource() datasource.DataSource { return &functionDS{} }

type functionDS struct{ ds }

type functionDSModel struct {
	ID            types.String `tfsdk:"id"`
	Name          types.String `tfsdk:"name"`
	Language      types.String `tfsdk:"language"`
	Description   types.String `tfsdk:"description"`
	Access        types.String `tfsdk:"access"`
	ContentSHA256 types.String `tfsdk:"content_sha256"`
	Capabilities  types.Set    `tfsdk:"capabilities"`
	ActiveVersion types.Int64  `tfsdk:"active_version"`
	LatestVersion types.Int64  `tfsdk:"latest_version"`
	Principal     types.String `tfsdk:"principal"`
	CreatedAt     types.String `tfsdk:"created_at"`
	UpdatedAt     types.String `tfsdk:"updated_at"`
}

func (d *functionDS) Metadata(_ context.Context, req datasource.MetadataRequest, resp *datasource.MetadataResponse) {
	resp.TypeName = dsAdd(req, "function")
}

func (d *functionDS) Schema(_ context.Context, _ datasource.SchemaRequest, resp *datasource.SchemaResponse) {
	resp.Schema = schema.Schema{
		Description: "Looks up a function and its active version by name.",
		Attributes: map[string]schema.Attribute{
			"id": dsString("The function name."), "name": schema.StringAttribute{Required: true, Description: "Function name."},
			"language": dsString("Language of the active version."), "description": dsString("Description."), "access": dsString("HTTP access mode."),
			"content_sha256": dsString("SHA-256 of the active version's code."),
			"capabilities":   schema.SetAttribute{Computed: true, ElementType: types.StringType, Description: "Capabilities of the active version."},
			"active_version": schema.Int64Attribute{Computed: true, Description: "Active version."},
			"latest_version": schema.Int64Attribute{Computed: true, Description: "Newest version."},
			"principal":      dsString("Identity the function runs as."), "created_at": dsString("Creation time."), "updated_at": dsString("Last change."),
		},
	}
}

func (d *functionDS) Read(ctx context.Context, req datasource.ReadRequest, resp *datasource.ReadResponse) {
	var cfg functionDSModel
	resp.Diagnostics.Append(req.Config.Get(ctx, &cfg)...)
	if resp.Diagnostics.HasError() {
		return
	}
	fr := &functionResource{base: d.base}
	f, v, _, _, _, err := fr.readParts(ctx, cfg.Name.ValueString())
	if err != nil {
		addErr(&resp.Diagnostics, "Error reading function", err)
		return
	}
	st := functionDSModel{
		ID: types.StringValue(f.Name), Name: types.StringValue(f.Name), Language: types.StringValue(v.Language),
		Description: types.StringValue(f.Description), Access: types.StringValue(f.Access), ContentSHA256: types.StringValue(v.SHA256),
		Capabilities: strSet(v.Capabilities), ActiveVersion: types.Int64Value(f.ActiveVersion), LatestVersion: types.Int64Value(f.LatestVersion),
		Principal: types.StringValue(f.Principal), CreatedAt: types.StringValue(f.CreatedAt), UpdatedAt: types.StringValue(f.UpdatedAt),
	}
	resp.Diagnostics.Append(resp.State.Set(ctx, st)...)
}

// =============================================================================
// forgebase_service_account
// =============================================================================

// NewServiceAccountDataSource returns the forgebase_service_account data source.
func NewServiceAccountDataSource() datasource.DataSource { return &serviceAccountDS{} }

type serviceAccountDS struct{ ds }

func (d *serviceAccountDS) Metadata(_ context.Context, req datasource.MetadataRequest, resp *datasource.MetadataResponse) {
	resp.TypeName = dsAdd(req, "service_account")
}

func (d *serviceAccountDS) Schema(_ context.Context, _ datasource.SchemaRequest, resp *datasource.SchemaResponse) {
	resp.Schema = schema.Schema{
		Description: "Looks up a service account by name.",
		Attributes: map[string]schema.Attribute{
			"id": dsString("Service account id."), "name": schema.StringAttribute{Required: true, Description: "Service account name."},
			"description": dsString("Description."), "role": dsString("`service` or `admin`."), "realm": dsString("Realm."),
			"disabled": schema.BoolAttribute{Computed: true, Description: "Disabled."}, "created_by": dsString("Creator."), "created_at": dsString("Creation time."),
		},
	}
}

func (d *serviceAccountDS) Read(ctx context.Context, req datasource.ReadRequest, resp *datasource.ReadResponse) {
	var cfg serviceAccountModel
	resp.Diagnostics.Append(req.Config.Get(ctx, &cfg)...)
	if resp.Diagnostics.HasError() {
		return
	}
	var l items[apiServiceAccount]
	if err := d.c.Get(ctx, "/api/auth/service-accounts", nil, &l); err != nil {
		addErr(&resp.Diagnostics, "Error listing service accounts", err)
		return
	}
	for _, a := range l.Items {
		if a.Name == cfg.Name.ValueString() {
			resp.Diagnostics.Append(resp.State.Set(ctx, a.model())...)
			return
		}
	}
	resp.Diagnostics.AddAttributeError(path.Root("name"), "Service account not found", fmt.Sprintf("No service account is named %q.", cfg.Name.ValueString()))
}

// =============================================================================
// forgebase_org
// =============================================================================

// NewOrgDataSource returns the forgebase_org data source.
func NewOrgDataSource() datasource.DataSource { return &orgDS{} }

type orgDS struct{ ds }

func (d *orgDS) Metadata(_ context.Context, req datasource.MetadataRequest, resp *datasource.MetadataResponse) {
	resp.TypeName = dsAdd(req, "org")
}

func (d *orgDS) Schema(_ context.Context, _ datasource.SchemaRequest, resp *datasource.SchemaResponse) {
	resp.Schema = schema.Schema{
		Description: "Looks up an organization the caller can see, by slug.",
		Attributes: map[string]schema.Attribute{
			"id": dsString("Organization id."), "slug": schema.StringAttribute{Required: true, Description: "Organization slug."},
			"name": dsString("Display name."), "realm_id": dsString("Realm."),
		},
	}
}

func (d *orgDS) Read(ctx context.Context, req datasource.ReadRequest, resp *datasource.ReadResponse) {
	var cfg orgModel
	resp.Diagnostics.Append(req.Config.Get(ctx, &cfg)...)
	if resp.Diagnostics.HasError() {
		return
	}
	var raw json.RawMessage
	if err := d.c.Get(ctx, "/api/v1/orgs", nil, &raw); err != nil {
		addErr(&resp.Diagnostics, "Error listing organizations", err)
		return
	}
	orgs, err := decodeList[apiOrg](raw)
	if err != nil {
		addErr(&resp.Diagnostics, "Error decoding organizations", err)
		return
	}
	for _, o := range orgs {
		if o.Slug == cfg.Slug.ValueString() {
			resp.Diagnostics.Append(resp.State.Set(ctx, o.model())...)
			return
		}
	}
	resp.Diagnostics.AddAttributeError(path.Root("slug"), "Organization not found", fmt.Sprintf("No organization has slug %q.", cfg.Slug.ValueString()))
}

// =============================================================================
// forgebase_project
// =============================================================================

// NewProjectDataSource returns the forgebase_project data source.
func NewProjectDataSource() datasource.DataSource { return &projectDS{} }

type projectDS struct{ ds }

func (d *projectDS) Metadata(_ context.Context, req datasource.MetadataRequest, resp *datasource.MetadataResponse) {
	resp.TypeName = dsAdd(req, "project")
}

func (d *projectDS) Schema(_ context.Context, _ datasource.SchemaRequest, resp *datasource.SchemaResponse) {
	resp.Schema = schema.Schema{
		Description: "Looks up a project and its environments by id.",
		Attributes: map[string]schema.Attribute{
			"id":   schema.StringAttribute{Required: true, Description: "Project id."},
			"slug": dsString("Slug."), "name": dsString("Display name."), "org": dsString("Organization id, if any."), "realm_id": dsString("Realm."),
			"environments": schema.ListNestedAttribute{Computed: true, Description: "Environments.", NestedObject: schema.NestedAttributeObject{Attributes: map[string]schema.Attribute{
				"id": dsString("Environment (tenant) id."), "name": dsString("Environment name."),
			}}},
		},
	}
}

func (d *projectDS) Read(ctx context.Context, req datasource.ReadRequest, resp *datasource.ReadResponse) {
	var cfg projectModel
	resp.Diagnostics.Append(req.Config.Get(ctx, &cfg)...)
	if resp.Diagnostics.HasError() {
		return
	}
	var out apiProject
	if err := d.c.Get(ctx, "/api/v1/projects/"+seg(cfg.ID.ValueString()), nil, &out); err != nil {
		addErr(&resp.Diagnostics, "Error reading project", err)
		return
	}
	st, diags := out.model(ctx)
	resp.Diagnostics.Append(diags...)
	resp.Diagnostics.Append(resp.State.Set(ctx, st)...)
}

// =============================================================================
// forgebase_plan
// =============================================================================

// NewPlanDataSource returns the forgebase_plan data source.
func NewPlanDataSource() datasource.DataSource { return &planDS{} }

type planDS struct{ ds }

func (d *planDS) Metadata(_ context.Context, req datasource.MetadataRequest, resp *datasource.MetadataResponse) {
	resp.TypeName = dsAdd(req, "plan")
}

func (d *planDS) Schema(_ context.Context, _ datasource.SchemaRequest, resp *datasource.SchemaResponse) {
	resp.Schema = schema.Schema{
		Description: "Looks up an entitlement plan by key.",
		Attributes: map[string]schema.Attribute{
			"id": dsString("The plan key."), "key": schema.StringAttribute{Required: true, Description: "Plan key."},
			"name": dsString("Display name."), "description": dsString("Description."),
			"is_default": schema.BoolAttribute{Computed: true, Description: "Default plan."}, "active": schema.BoolAttribute{Computed: true, Description: "Assignable."},
			"trial_days": schema.Int64Attribute{Computed: true, Description: "Trial length."},
			"metadata":   schema.StringAttribute{Computed: true, CustomType: jsontypes.NormalizedType{}, Description: "Metadata as JSON."},
			"features": schema.ListNestedAttribute{Computed: true, Description: "Features of the plan.", NestedObject: schema.NestedAttributeObject{Attributes: map[string]schema.Attribute{
				"feature": dsString("Feature key."), "enabled": schema.BoolAttribute{Computed: true, Description: "Included."},
				"limit": schema.Int64Attribute{Computed: true, Description: "Limit."}, "unit": dsString("Unit."), "period": dsString("Period."),
				"window_secs": schema.Int64Attribute{Computed: true, Description: "Rolling window."}, "trial_limit": schema.Int64Attribute{Computed: true, Description: "Trial allowance."},
			}}},
			"created_at": dsString("Creation time."), "updated_at": dsString("Last change."),
		},
	}
}

func (d *planDS) Read(ctx context.Context, req datasource.ReadRequest, resp *datasource.ReadResponse) {
	var cfg planModel
	resp.Diagnostics.Append(req.Config.Get(ctx, &cfg)...)
	if resp.Diagnostics.HasError() {
		return
	}
	var out apiPlan
	if err := d.c.Get(ctx, "/api/v1/plans/"+seg(cfg.Key.ValueString()), nil, &out); err != nil {
		addErr(&resp.Diagnostics, "Error reading plan", err)
		return
	}
	st, diags := planFromAPI(ctx, out, nil)
	resp.Diagnostics.Append(diags...)
	resp.Diagnostics.Append(resp.State.Set(ctx, st)...)
}

var _ = attr.Value(nil)
