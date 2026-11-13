package provider

import (
	"context"
	"encoding/json"
	"regexp"

	"github.com/hashicorp/terraform-plugin-framework-jsontypes/jsontypes"
	"github.com/hashicorp/terraform-plugin-framework-validators/int64validator"
	"github.com/hashicorp/terraform-plugin-framework-validators/stringvalidator"
	"github.com/hashicorp/terraform-plugin-framework/attr"
	"github.com/hashicorp/terraform-plugin-framework/path"
	"github.com/hashicorp/terraform-plugin-framework/resource"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/booldefault"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/int64planmodifier"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/mapdefault"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/objectplanmodifier"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/planmodifier"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/stringplanmodifier"
	"github.com/hashicorp/terraform-plugin-framework/schema/validator"
	"github.com/hashicorp/terraform-plugin-framework/types"

	"github.com/farzanshibu/forgebase/integrations/terraform-provider-forgebase/internal/client"
)

var (
	_           resource.Resource                = (*topicResource)(nil)
	_           resource.ResourceWithConfigure   = (*topicResource)(nil)
	_           resource.ResourceWithImportState = (*topicResource)(nil)
	queueNameRE                                  = regexp.MustCompile(`^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$`)
)

// NewQueueTopicResource returns the forgebase_queue_topic resource.
func NewQueueTopicResource() resource.Resource { return &topicResource{} }

type topicResource struct{ base }

type topicSourceModel struct {
	Collection types.String `tfsdk:"collection"`
	Ops        types.List   `tfsdk:"ops"`
}

var topicSourceType = types.ObjectType{AttrTypes: map[string]attr.Type{"collection": types.StringType, "ops": types.ListType{ElemType: types.StringType}}}

type topicModel struct {
	ID              types.String         `tfsdk:"id"`
	Name            types.String         `tfsdk:"name"`
	RetentionMs     types.Int64          `tfsdk:"retention_ms"`
	RetentionBytes  types.Int64          `tfsdk:"retention_bytes"`
	MaxMessageBytes types.Int64          `tfsdk:"max_message_bytes"`
	DedupWindowMs   types.Int64          `tfsdk:"dedup_window_ms"`
	Labels          types.Map            `tfsdk:"labels"`
	Schema          jsontypes.Normalized `tfsdk:"schema"`
	Source          types.Object         `tfsdk:"source"`
	ForceDestroy    types.Bool           `tfsdk:"force_destroy"`
	CreatedBy       types.String         `tfsdk:"created_by"`
	CreatedAt       types.String         `tfsdk:"created_at"`
	UpdatedAt       types.String         `tfsdk:"updated_at"`
}

type apiTopic struct {
	Name            string            `json:"name"`
	RetentionMs     int64             `json:"retention_ms"`
	RetentionBytes  *int64            `json:"retention_bytes"`
	MaxMessageBytes int64             `json:"max_message_bytes"`
	DedupWindowMs   int64             `json:"dedup_window_ms"`
	Labels          map[string]string `json:"labels"`
	Schema          json.RawMessage   `json:"schema"`
	Source          *struct {
		Collection string   `json:"collection"`
		Ops        []string `json:"ops"`
	} `json:"source"`
	CreatedBy string `json:"created_by"`
	CreatedAt string `json:"created_at"`
	UpdatedAt string `json:"updated_at"`
}

func (r *topicResource) Metadata(_ context.Context, req resource.MetadataRequest, resp *resource.MetadataResponse) {
	resp.TypeName = req.ProviderTypeName + "_queue_topic"
}

func (r *topicResource) Configure(_ context.Context, req resource.ConfigureRequest, resp *resource.ConfigureResponse) {
	r.configureResource(req, resp)
}

func (r *topicResource) Schema(_ context.Context, _ resource.SchemaRequest, resp *resource.SchemaResponse) {
	useState := []planmodifier.Int64{int64planmodifier.UseStateForUnknown()}
	resp.Schema = schema.Schema{
		Description: "A durable queue / pub-sub topic. Messages are kept for `retention_ms`; each subscription has its own cursor.",
		Attributes: map[string]schema.Attribute{
			"id": schema.StringAttribute{Computed: true, PlanModifiers: []planmodifier.String{stringplanmodifier.UseStateForUnknown()}, Description: "The topic name."},
			"name": schema.StringAttribute{
				Required: true, Description: "Topic name (letters, digits, `.`, `_`, `-`). Cannot change.",
				Validators:    []validator.String{stringvalidator.RegexMatches(queueNameRE, "must be 1-128 letters, digits, '.', '_' or '-', starting with a letter or digit")},
				PlanModifiers: []planmodifier.String{stringplanmodifier.RequiresReplace()},
			},
			"retention_ms": schema.Int64Attribute{
				Optional: true, Computed: true, PlanModifiers: useState,
				Description: "How long messages are kept, in milliseconds (at least 60000). Server default when omitted.",
				Validators:  []validator.Int64{int64validator.AtLeast(60000)},
			},
			"retention_bytes": schema.Int64Attribute{
				Optional: true, Description: "Size cap per topic in bytes (at least 1024). Omit for no cap.",
				Validators: []validator.Int64{int64validator.AtLeast(1024)},
			},
			"max_message_bytes": schema.Int64Attribute{
				Optional: true, Computed: true, PlanModifiers: useState,
				Description: "Largest accepted message in bytes (at least 64). Server default when omitted.",
				Validators:  []validator.Int64{int64validator.AtLeast(64)},
			},
			"dedup_window_ms": schema.Int64Attribute{
				Optional: true, Computed: true, PlanModifiers: useState,
				Description: "Window in which a repeated `dedup_id` is accepted once, in milliseconds (at least 1000). Server default when omitted.",
				Validators:  []validator.Int64{int64validator.AtLeast(1000)},
			},
			"labels": schema.MapAttribute{
				Optional: true, Computed: true, ElementType: types.StringType,
				Default:     mapdefault.StaticValue(types.MapValueMust(types.StringType, map[string]attr.Value{})),
				Description: "Free-form labels.",
			},
			"schema": schema.StringAttribute{
				Optional: true, CustomType: jsontypes.NormalizedType{},
				Description: "JSON Schema (as a JSON document) every published message must satisfy. Not allowed with `source`.",
			},
			"source": schema.SingleNestedAttribute{
				Optional:      true,
				Description:   "Feed the topic from record changes of a collection. Requires an administrator; cannot change after creation.",
				PlanModifiers: []planmodifier.Object{objectplanmodifier.RequiresReplace()},
				Attributes: map[string]schema.Attribute{
					"collection": schema.StringAttribute{Required: true, Description: "Collection whose record changes are published."},
					"ops": schema.ListAttribute{
						Optional: true, Computed: true, ElementType: types.StringType,
						Description: "Operations to publish (`create`, `update`, `delete`). Server default when omitted.",
					},
				},
			},
			"force_destroy": schema.BoolAttribute{
				Optional: true, Computed: true, Default: booldefault.StaticBool(false),
				Description: "Also delete the topic's subscriptions when destroying it. Without it the server refuses while subscriptions exist.",
			},
			"created_by": schema.StringAttribute{Computed: true, PlanModifiers: []planmodifier.String{stringplanmodifier.UseStateForUnknown()}, Description: "Creator."},
			"created_at": schema.StringAttribute{Computed: true, PlanModifiers: []planmodifier.String{stringplanmodifier.UseStateForUnknown()}, Description: "Creation time."},
			"updated_at": schema.StringAttribute{Computed: true, Description: "Last change."},
		},
	}
}

func (r *topicResource) ImportState(ctx context.Context, req resource.ImportStateRequest, resp *resource.ImportStateResponse) {
	resp.Diagnostics.Append(resp.State.SetAttribute(ctx, path.Root("id"), req.ID)...)
	resp.Diagnostics.Append(resp.State.SetAttribute(ctx, path.Root("force_destroy"), false)...)
}

func topicFromAPI(ctx context.Context, t apiTopic, force types.Bool) (topicModel, diagList) {
	var diags diagList
	m := topicModel{
		ID: types.StringValue(t.Name), Name: types.StringValue(t.Name),
		RetentionMs: types.Int64Value(t.RetentionMs), RetentionBytes: nullableInt64(t.RetentionBytes),
		MaxMessageBytes: types.Int64Value(t.MaxMessageBytes), DedupWindowMs: types.Int64Value(t.DedupWindowMs),
		Labels: strMap(t.Labels), Schema: jsonValue(t.Schema), ForceDestroy: force,
		CreatedBy: types.StringValue(t.CreatedBy), CreatedAt: types.StringValue(t.CreatedAt), UpdatedAt: types.StringValue(t.UpdatedAt),
		Source: types.ObjectNull(topicSourceType.AttrTypes),
	}
	if t.Labels == nil {
		m.Labels = strMap(map[string]string{})
	}
	if force.IsNull() || force.IsUnknown() {
		m.ForceDestroy = types.BoolValue(false)
	}
	if t.Source != nil {
		obj, d := types.ObjectValueFrom(ctx, topicSourceType.AttrTypes, topicSourceModel{
			Collection: types.StringValue(t.Source.Collection), Ops: strList(t.Source.Ops),
		})
		diags.Append(d...)
		m.Source = obj
	}
	return m, diags
}

func (r *topicResource) Create(ctx context.Context, req resource.CreateRequest, resp *resource.CreateResponse) {
	var plan topicModel
	resp.Diagnostics.Append(req.Plan.Get(ctx, &plan)...)
	if resp.Diagnostics.HasError() {
		return
	}
	body := map[string]any{"name": plan.Name.ValueString(), "labels": mapStrings(plan.Labels)}
	if v := int64Ptr(plan.RetentionMs); v != nil {
		body["retention_ms"] = *v
	}
	if v := int64Ptr(plan.RetentionBytes); v != nil {
		body["retention_bytes"] = *v
	}
	if v := int64Ptr(plan.MaxMessageBytes); v != nil {
		body["max_message_bytes"] = *v
	}
	if v := int64Ptr(plan.DedupWindowMs); v != nil {
		body["dedup_window_ms"] = *v
	}
	if raw := jsonRaw(plan.Schema); raw != nil {
		body["schema"] = raw
	}
	if !plan.Source.IsNull() && !plan.Source.IsUnknown() {
		var src topicSourceModel
		resp.Diagnostics.Append(plan.Source.As(ctx, &src, basicObjectOptions)...)
		s := map[string]any{"collection": src.Collection.ValueString()}
		if ops := listStrings(src.Ops); len(ops) > 0 {
			s["ops"] = ops
		}
		body["source"] = s
	}
	if resp.Diagnostics.HasError() {
		return
	}
	var out apiTopic
	if err := r.c.Post(ctx, "/api/v1/queues/topics", body, &out); err != nil {
		addErr(&resp.Diagnostics, "Error creating topic", err)
		return
	}
	st, d := topicFromAPI(ctx, out, plan.ForceDestroy)
	resp.Diagnostics.Append(d...)
	st.Schema = keepJSON(plan.Schema, st.Schema)
	resp.Diagnostics.Append(resp.State.Set(ctx, st)...)
}

// keepJSON keeps the configured text when the server's value is semantically the same.
func keepJSON(cfg, got jsontypes.Normalized) jsontypes.Normalized {
	if !cfg.IsNull() && !cfg.IsUnknown() && !got.IsNull() && jsonEqual([]byte(cfg.ValueString()), []byte(got.ValueString())) {
		return cfg
	}
	return got
}

func (r *topicResource) Read(ctx context.Context, req resource.ReadRequest, resp *resource.ReadResponse) {
	var prior topicModel
	resp.Diagnostics.Append(req.State.Get(ctx, &prior)...)
	if resp.Diagnostics.HasError() {
		return
	}
	var out apiTopic
	err := r.c.Get(ctx, "/api/v1/queues/topics/"+seg(prior.ID.ValueString()), nil, &out)
	if removeIfGone(ctx, &resp.State, &resp.Diagnostics, "topic", err) {
		return
	}
	st, d := topicFromAPI(ctx, out, prior.ForceDestroy)
	resp.Diagnostics.Append(d...)
	st.Schema = keepJSON(prior.Schema, st.Schema)
	resp.Diagnostics.Append(resp.State.Set(ctx, st)...)
}

func (r *topicResource) Update(ctx context.Context, req resource.UpdateRequest, resp *resource.UpdateResponse) {
	var plan, prior topicModel
	resp.Diagnostics.Append(req.Plan.Get(ctx, &plan)...)
	resp.Diagnostics.Append(req.State.Get(ctx, &prior)...)
	if resp.Diagnostics.HasError() {
		return
	}
	retBytes := int64(0) // 0 clears the cap
	if v := int64Ptr(plan.RetentionBytes); v != nil {
		retBytes = *v
	}
	body := map[string]any{"retention_bytes": retBytes, "labels": mapStrings(plan.Labels)}
	if v := int64Ptr(plan.RetentionMs); v != nil {
		body["retention_ms"] = *v
	}
	if v := int64Ptr(plan.MaxMessageBytes); v != nil {
		body["max_message_bytes"] = *v
	}
	if v := int64Ptr(plan.DedupWindowMs); v != nil {
		body["dedup_window_ms"] = *v
	}
	if raw := jsonRaw(plan.Schema); raw != nil {
		body["schema"] = raw
	} else if !prior.Schema.IsNull() {
		body["clear_schema"] = true
	}
	var out apiTopic
	if err := r.c.Patch(ctx, "/api/v1/queues/topics/"+seg(prior.ID.ValueString()), body, &out); err != nil {
		addErr(&resp.Diagnostics, "Error updating topic", err)
		return
	}
	st, d := topicFromAPI(ctx, out, plan.ForceDestroy)
	resp.Diagnostics.Append(d...)
	st.Schema = keepJSON(plan.Schema, st.Schema)
	resp.Diagnostics.Append(resp.State.Set(ctx, st)...)
}

func (r *topicResource) Delete(ctx context.Context, req resource.DeleteRequest, resp *resource.DeleteResponse) {
	var state topicModel
	resp.Diagnostics.Append(req.State.Get(ctx, &state)...)
	if resp.Diagnostics.HasError() {
		return
	}
	var q client.Query
	if state.ForceDestroy.ValueBool() {
		q = client.Query{"force": "true"}
	}
	if err := r.c.Delete(ctx, "/api/v1/queues/topics/"+seg(state.ID.ValueString()), q); err != nil && !isGone(err) {
		addErr(&resp.Diagnostics, "Error deleting topic", err)
	}
}
