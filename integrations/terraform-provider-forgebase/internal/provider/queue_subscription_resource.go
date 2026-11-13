package provider

import (
	"context"
	"encoding/json"
	"fmt"
	"time"

	"github.com/hashicorp/terraform-plugin-framework-jsontypes/jsontypes"
	"github.com/hashicorp/terraform-plugin-framework-validators/int64validator"
	"github.com/hashicorp/terraform-plugin-framework-validators/stringvalidator"
	"github.com/hashicorp/terraform-plugin-framework/attr"
	"github.com/hashicorp/terraform-plugin-framework/path"
	"github.com/hashicorp/terraform-plugin-framework/resource"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/booldefault"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/boolplanmodifier"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/int64planmodifier"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/mapdefault"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/planmodifier"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/stringplanmodifier"
	"github.com/hashicorp/terraform-plugin-framework/schema/validator"
	"github.com/hashicorp/terraform-plugin-framework/types"
)

var (
	_ resource.Resource                   = (*subscriptionResource)(nil)
	_ resource.ResourceWithConfigure      = (*subscriptionResource)(nil)
	_ resource.ResourceWithImportState    = (*subscriptionResource)(nil)
	_ resource.ResourceWithModifyPlan     = (*subscriptionResource)(nil)
	_ resource.ResourceWithValidateConfig = (*subscriptionResource)(nil)
)

// NewQueueSubscriptionResource returns the forgebase_queue_subscription resource.
func NewQueueSubscriptionResource() resource.Resource { return &subscriptionResource{} }

type subscriptionResource struct{ base }

type pushModel struct {
	Kind     types.String `tfsdk:"kind"`
	Endpoint types.String `tfsdk:"endpoint"`
}

var pushObjType = types.ObjectType{AttrTypes: map[string]attr.Type{"kind": types.StringType, "endpoint": types.StringType}}

type subscriptionModel struct {
	ID                  types.String         `tfsdk:"id"`
	Name                types.String         `tfsdk:"name"`
	Topic               types.String         `tfsdk:"topic"`
	AckDeadlineMs       types.Int64          `tfsdk:"ack_deadline_ms"`
	MaxDeliveryAttempts types.Int64          `tfsdk:"max_delivery_attempts"`
	BackoffBaseMs       types.Int64          `tfsdk:"backoff_base_ms"`
	BackoffMaxMs        types.Int64          `tfsdk:"backoff_max_ms"`
	DeadLetterTopic     types.String         `tfsdk:"dead_letter_topic"`
	Ordering            types.Bool           `tfsdk:"ordering"`
	Filter              jsontypes.Normalized `tfsdk:"filter"`
	Labels              types.Map            `tfsdk:"labels"`
	Push                types.Object         `tfsdk:"push"`
	PushSecret          types.String         `tfsdk:"push_secret"`
	Start               types.String         `tfsdk:"start"`
	CreatedBy           types.String         `tfsdk:"created_by"`
	CreatedAt           types.String         `tfsdk:"created_at"`
	UpdatedAt           types.String         `tfsdk:"updated_at"`
}

type apiSubscription struct {
	Name                string            `json:"name"`
	Topic               string            `json:"topic"`
	AckDeadlineMs       int64             `json:"ack_deadline_ms"`
	MaxDeliveryAttempts *int64            `json:"max_delivery_attempts"`
	BackoffBaseMs       int64             `json:"backoff_base_ms"`
	BackoffMaxMs        int64             `json:"backoff_max_ms"`
	DeadLetterTopic     *string           `json:"dead_letter_topic"`
	Ordering            bool              `json:"ordering"`
	Filter              json.RawMessage   `json:"filter"`
	Labels              map[string]string `json:"labels"`
	Push                *struct {
		Kind     string `json:"kind"`
		Endpoint string `json:"endpoint"`
	} `json:"push"`
	PushSecret *string `json:"push_secret"`
	CreatedBy  string  `json:"created_by"`
	CreatedAt  string  `json:"created_at"`
	UpdatedAt  string  `json:"updated_at"`
}

func (r *subscriptionResource) Metadata(_ context.Context, req resource.MetadataRequest, resp *resource.MetadataResponse) {
	resp.TypeName = req.ProviderTypeName + "_queue_subscription"
}

func (r *subscriptionResource) Configure(_ context.Context, req resource.ConfigureRequest, resp *resource.ConfigureResponse) {
	r.configureResource(req, resp)
}

func (r *subscriptionResource) Schema(_ context.Context, _ resource.SchemaRequest, resp *resource.SchemaResponse) {
	useState := []planmodifier.Int64{int64planmodifier.UseStateForUnknown()}
	rrStr := []planmodifier.String{stringplanmodifier.RequiresReplace()}
	resp.Schema = schema.Schema{
		Description: "A subscription to a topic: a pull subscription (consumers call the pull API) or a push subscription " +
			"(the server delivers to a webhook URL or a function). A webhook subscription's signing secret is returned by the server " +
			"once, when the webhook is set, and is kept in `push_secret` (sensitive).",
		Attributes: map[string]schema.Attribute{
			"id": schema.StringAttribute{Computed: true, PlanModifiers: []planmodifier.String{stringplanmodifier.UseStateForUnknown()}, Description: "The subscription name."},
			"name": schema.StringAttribute{
				Required: true, Description: "Subscription name. Cannot change.",
				Validators:    []validator.String{stringvalidator.RegexMatches(queueNameRE, "must be 1-128 letters, digits, '.', '_' or '-', starting with a letter or digit")},
				PlanModifiers: rrStr,
			},
			"topic": schema.StringAttribute{Required: true, Description: "Topic to subscribe to. Cannot change.", PlanModifiers: rrStr},
			"ack_deadline_ms": schema.Int64Attribute{
				Optional: true, Computed: true, PlanModifiers: useState,
				Description: "Time a consumer has to acknowledge a delivery, in milliseconds (1000-600000; default 10000).",
				Validators:  []validator.Int64{int64validator.Between(1000, 600000)},
			},
			"max_delivery_attempts": schema.Int64Attribute{
				Optional: true, Description: "Attempts before a message goes to the dead-letter list or topic (1-100). Omit for unlimited.",
				Validators: []validator.Int64{int64validator.Between(1, 100)},
			},
			"backoff_base_ms": schema.Int64Attribute{
				Optional: true, Computed: true, PlanModifiers: useState,
				Description: "First redelivery delay in milliseconds (0-600000; default 1000).",
				Validators:  []validator.Int64{int64validator.Between(0, 600000)},
			},
			"backoff_max_ms": schema.Int64Attribute{
				Optional: true, Computed: true, PlanModifiers: useState,
				Description: "Longest redelivery delay in milliseconds (at least `backoff_base_ms`, at most 3600000; default 600000).",
				Validators:  []validator.Int64{int64validator.Between(0, 3600000)},
			},
			"dead_letter_topic": schema.StringAttribute{Optional: true, Description: "Topic that receives messages after `max_delivery_attempts`. Must differ from `topic`."},
			"ordering": schema.BoolAttribute{
				Optional: true, Computed: true, Default: booldefault.StaticBool(false),
				Description: "Deliver messages that share an ordering key one at a time, in order. Cannot change.", PlanModifiers: []planmodifier.Bool{boolplanmodifier.RequiresReplace()},
			},
			"filter": schema.StringAttribute{
				Optional: true, CustomType: jsontypes.NormalizedType{}, PlanModifiers: rrStr,
				Description: "Attribute filter as a JSON document. Cannot change.",
			},
			"labels": schema.MapAttribute{
				Optional: true, Computed: true, ElementType: types.StringType,
				Default:     mapdefault.StaticValue(types.MapValueMust(types.StringType, map[string]attr.Value{})),
				Description: "Free-form labels.",
			},
			"push": schema.SingleNestedAttribute{
				Optional:    true,
				Description: "Push delivery. Omit for a pull subscription. Changing the endpoint of a webhook rotates its secret.",
				Attributes: map[string]schema.Attribute{
					"kind": schema.StringAttribute{
						Required: true, Description: "`webhook` (HTTPS URL) or `function` (name of a ForgeBase function).",
						Validators: []validator.String{stringvalidator.OneOf("webhook", "function")},
					},
					"endpoint": schema.StringAttribute{Required: true, Description: "Webhook URL (must be https:// and a public address) or function name."},
				},
			},
			"push_secret": schema.StringAttribute{
				Computed: true, Sensitive: true,
				Description: "Signing secret of a webhook subscription, returned by the server only when the webhook is created or changed. Stored in state; the server cannot show it again.",
			},
			"start": schema.StringAttribute{
				Optional: true, PlanModifiers: rrStr,
				Description: "Where a new subscription starts reading: `latest` (default), `earliest`, or an RFC 3339 time. Used at creation only.",
			},
			"created_by": schema.StringAttribute{Computed: true, PlanModifiers: []planmodifier.String{stringplanmodifier.UseStateForUnknown()}, Description: "Creator."},
			"created_at": schema.StringAttribute{Computed: true, PlanModifiers: []planmodifier.String{stringplanmodifier.UseStateForUnknown()}, Description: "Creation time."},
			"updated_at": schema.StringAttribute{Computed: true, Description: "Last change."},
		},
	}
}

func (r *subscriptionResource) ValidateConfig(ctx context.Context, req resource.ValidateConfigRequest, resp *resource.ValidateConfigResponse) {
	var m subscriptionModel
	resp.Diagnostics.Append(req.Config.Get(ctx, &m)...)
	if resp.Diagnostics.HasError() {
		return
	}
	if !m.Start.IsNull() && !m.Start.IsUnknown() {
		switch s := m.Start.ValueString(); s {
		case "latest", "earliest":
		default:
			if _, err := time.Parse(time.RFC3339, s); err != nil {
				resp.Diagnostics.AddAttributeError(path.Root("start"), "Invalid start", "Use `latest`, `earliest` or an RFC 3339 time such as 2026-01-02T03:04:05Z.")
			}
		}
	}
	if !m.DeadLetterTopic.IsNull() && !m.DeadLetterTopic.IsUnknown() && !m.Topic.IsUnknown() && m.DeadLetterTopic.Equal(m.Topic) {
		resp.Diagnostics.AddAttributeError(path.Root("dead_letter_topic"), "Invalid dead-letter topic", "The dead-letter topic must differ from the subscription's topic.")
	}
	if !m.BackoffBaseMs.IsNull() && !m.BackoffMaxMs.IsNull() && !m.BackoffBaseMs.IsUnknown() && !m.BackoffMaxMs.IsUnknown() &&
		m.BackoffMaxMs.ValueInt64() < m.BackoffBaseMs.ValueInt64() {
		resp.Diagnostics.AddAttributeError(path.Root("backoff_max_ms"), "Invalid backoff", "backoff_max_ms must be at least backoff_base_ms.")
	}
	if !m.Push.IsNull() && !m.Push.IsUnknown() {
		var p pushModel
		if d := m.Push.As(ctx, &p, basicObjectOptions); !d.HasError() && p.Kind.ValueString() == "webhook" &&
			!p.Endpoint.IsUnknown() && len(p.Endpoint.ValueString()) > 0 && !hasPrefixFold(p.Endpoint.ValueString(), "https://") && !hasPrefixFold(p.Endpoint.ValueString(), "http://") {
			resp.Diagnostics.AddAttributeError(path.Root("push").AtName("endpoint"), "Invalid webhook URL", "A webhook endpoint is a URL: https://… (http:// only when the server allows it).")
		}
	}
	if !m.Filter.IsNull() && !m.Filter.IsUnknown() {
		var obj map[string]any
		if err := json.Unmarshal([]byte(m.Filter.ValueString()), &obj); err != nil {
			resp.Diagnostics.AddAttributeError(path.Root("filter"), "Invalid filter", "`filter` must be a JSON object.")
		}
	}
}

func hasPrefixFold(s, p string) bool {
	return len(s) >= len(p) && (s[:len(p)] == p || equalFoldASCII(s[:len(p)], p))
}

func equalFoldASCII(a, b string) bool {
	for i := 0; i < len(a); i++ {
		x, y := a[i], b[i]
		if 'A' <= x && x <= 'Z' {
			x += 'a' - 'A'
		}
		if 'A' <= y && y <= 'Z' {
			y += 'a' - 'A'
		}
		if x != y {
			return false
		}
	}
	return true
}

// ModifyPlan keeps the stored webhook secret while the push block is unchanged
// and marks it unknown when the webhook changes (the server issues a new one).
func (r *subscriptionResource) ModifyPlan(ctx context.Context, req resource.ModifyPlanRequest, resp *resource.ModifyPlanResponse) {
	if req.State.Raw.IsNull() || req.Plan.Raw.IsNull() {
		return
	}
	var plan, state subscriptionModel
	resp.Diagnostics.Append(req.Plan.Get(ctx, &plan)...)
	resp.Diagnostics.Append(req.State.Get(ctx, &state)...)
	if resp.Diagnostics.HasError() {
		return
	}
	if plan.Push.IsUnknown() {
		return
	}
	if plan.Push.Equal(state.Push) {
		resp.Diagnostics.Append(resp.Plan.SetAttribute(ctx, path.Root("push_secret"), state.PushSecret)...)
		return
	}
	// Push removed or changed to a function: no secret remains. A new webhook: unknown until apply.
	if plan.Push.IsNull() {
		resp.Diagnostics.Append(resp.Plan.SetAttribute(ctx, path.Root("push_secret"), types.StringNull())...)
		return
	}
	var p pushModel
	if d := plan.Push.As(ctx, &p, basicObjectOptions); !d.HasError() && p.Kind.ValueString() == "function" {
		resp.Diagnostics.Append(resp.Plan.SetAttribute(ctx, path.Root("push_secret"), types.StringNull())...)
	}
}

func (r *subscriptionResource) ImportState(ctx context.Context, req resource.ImportStateRequest, resp *resource.ImportStateResponse) {
	resp.Diagnostics.Append(resp.State.SetAttribute(ctx, path.Root("id"), req.ID)...)
}

func subscriptionFromAPI(ctx context.Context, s apiSubscription, prior subscriptionModel) (subscriptionModel, diagList) {
	var diags diagList
	m := subscriptionModel{
		ID: types.StringValue(s.Name), Name: types.StringValue(s.Name), Topic: types.StringValue(s.Topic),
		AckDeadlineMs: types.Int64Value(s.AckDeadlineMs), MaxDeliveryAttempts: nullableInt64(s.MaxDeliveryAttempts),
		BackoffBaseMs: types.Int64Value(s.BackoffBaseMs), BackoffMaxMs: types.Int64Value(s.BackoffMaxMs),
		DeadLetterTopic: nullableString(s.DeadLetterTopic), Ordering: types.BoolValue(s.Ordering),
		Filter: keepJSON(prior.Filter, jsonValue(s.Filter)), Labels: strMap(s.Labels),
		Push: types.ObjectNull(pushObjType.AttrTypes), PushSecret: prior.PushSecret, Start: prior.Start,
		CreatedBy: types.StringValue(s.CreatedBy), CreatedAt: types.StringValue(s.CreatedAt), UpdatedAt: types.StringValue(s.UpdatedAt),
	}
	if s.Labels == nil {
		m.Labels = strMap(map[string]string{})
	}
	if s.Push != nil {
		obj, d := types.ObjectValueFrom(ctx, pushObjType.AttrTypes, pushModel{Kind: types.StringValue(s.Push.Kind), Endpoint: types.StringValue(s.Push.Endpoint)})
		diags.Append(d...)
		m.Push = obj
	} else {
		m.PushSecret = types.StringNull()
	}
	// The secret is only in the create / change response.
	if s.PushSecret != nil && *s.PushSecret != "" {
		m.PushSecret = types.StringValue(*s.PushSecret)
	} else if m.PushSecret.IsUnknown() {
		m.PushSecret = types.StringNull()
	}
	return m, diags
}

func (m subscriptionModel) pushConfig(ctx context.Context) (map[string]any, diagList) {
	var diags diagList
	if m.Push.IsNull() || m.Push.IsUnknown() {
		return nil, diags
	}
	var p pushModel
	diags.Append(m.Push.As(ctx, &p, basicObjectOptions)...)
	return map[string]any{"kind": p.Kind.ValueString(), "endpoint": p.Endpoint.ValueString()}, diags
}

func (r *subscriptionResource) Create(ctx context.Context, req resource.CreateRequest, resp *resource.CreateResponse) {
	var plan subscriptionModel
	resp.Diagnostics.Append(req.Plan.Get(ctx, &plan)...)
	if resp.Diagnostics.HasError() {
		return
	}
	body := map[string]any{"name": plan.Name.ValueString(), "topic": plan.Topic.ValueString(), "ordering": plan.Ordering.ValueBool(), "labels": mapStrings(plan.Labels)}
	for k, v := range map[string]types.Int64{"ack_deadline_ms": plan.AckDeadlineMs, "max_delivery_attempts": plan.MaxDeliveryAttempts, "backoff_base_ms": plan.BackoffBaseMs, "backoff_max_ms": plan.BackoffMaxMs} {
		if p := int64Ptr(v); p != nil {
			body[k] = *p
		}
	}
	if p := strPtr(plan.DeadLetterTopic); p != nil {
		body["dead_letter_topic"] = *p
	}
	if raw := jsonRaw(plan.Filter); raw != nil {
		body["filter"] = raw
	}
	push, d := plan.pushConfig(ctx)
	resp.Diagnostics.Append(d...)
	if push != nil {
		body["push"] = push
	}
	if s := strPtr(plan.Start); s != nil {
		switch *s {
		case "latest", "earliest":
			body["start"] = *s
		default:
			body["start"] = map[string]string{"time": *s}
		}
	}
	if resp.Diagnostics.HasError() {
		return
	}
	var out apiSubscription
	if err := r.c.Post(ctx, "/api/v1/queues/subscriptions", body, &out); err != nil {
		addErr(&resp.Diagnostics, "Error creating subscription", err)
		return
	}
	plan.PushSecret = types.StringNull()
	st, d := subscriptionFromAPI(ctx, out, plan)
	resp.Diagnostics.Append(d...)
	resp.Diagnostics.Append(resp.State.Set(ctx, st)...)
}

func (r *subscriptionResource) Read(ctx context.Context, req resource.ReadRequest, resp *resource.ReadResponse) {
	var prior subscriptionModel
	resp.Diagnostics.Append(req.State.Get(ctx, &prior)...)
	if resp.Diagnostics.HasError() {
		return
	}
	var out apiSubscription
	err := r.c.Get(ctx, "/api/v1/queues/subscriptions/"+seg(prior.ID.ValueString()), nil, &out)
	if removeIfGone(ctx, &resp.State, &resp.Diagnostics, "subscription", err) {
		return
	}
	out.PushSecret = nil // never shown again
	st, d := subscriptionFromAPI(ctx, out, prior)
	resp.Diagnostics.Append(d...)
	resp.Diagnostics.Append(resp.State.Set(ctx, st)...)
}

func (r *subscriptionResource) Update(ctx context.Context, req resource.UpdateRequest, resp *resource.UpdateResponse) {
	var plan, prior subscriptionModel
	resp.Diagnostics.Append(req.Plan.Get(ctx, &plan)...)
	resp.Diagnostics.Append(req.State.Get(ctx, &prior)...)
	if resp.Diagnostics.HasError() {
		return
	}
	attempts := int64(0) // 0 clears the limit
	if p := int64Ptr(plan.MaxDeliveryAttempts); p != nil {
		attempts = *p
	}
	body := map[string]any{"max_delivery_attempts": attempts, "labels": mapStrings(plan.Labels)}
	for k, v := range map[string]types.Int64{"ack_deadline_ms": plan.AckDeadlineMs, "backoff_base_ms": plan.BackoffBaseMs, "backoff_max_ms": plan.BackoffMaxMs} {
		if p := int64Ptr(v); p != nil {
			body[k] = *p
		}
	}
	if p := strPtr(plan.DeadLetterTopic); p != nil {
		body["dead_letter_topic"] = *p
	} else if !prior.DeadLetterTopic.IsNull() {
		body["clear_dead_letter_topic"] = true
	}
	// Only touch push when it changed: setting a webhook issues a new secret.
	if !plan.Push.Equal(prior.Push) {
		push, d := plan.pushConfig(ctx)
		resp.Diagnostics.Append(d...)
		if push != nil {
			body["push"] = push
		} else {
			body["clear_push"] = true
		}
	}
	if resp.Diagnostics.HasError() {
		return
	}
	var out apiSubscription
	if err := r.c.Patch(ctx, "/api/v1/queues/subscriptions/"+seg(prior.ID.ValueString()), body, &out); err != nil {
		addErr(&resp.Diagnostics, "Error updating subscription", err)
		return
	}
	if plan.Push.Equal(prior.Push) {
		out.PushSecret = nil
		plan.PushSecret = prior.PushSecret
	} else {
		plan.PushSecret = types.StringNull()
	}
	st, d := subscriptionFromAPI(ctx, out, plan)
	resp.Diagnostics.Append(d...)
	resp.Diagnostics.Append(resp.State.Set(ctx, st)...)
}

func (r *subscriptionResource) Delete(ctx context.Context, req resource.DeleteRequest, resp *resource.DeleteResponse) {
	var state subscriptionModel
	resp.Diagnostics.Append(req.State.Get(ctx, &state)...)
	if resp.Diagnostics.HasError() {
		return
	}
	if err := r.c.Delete(ctx, "/api/v1/queues/subscriptions/"+seg(state.ID.ValueString()), nil); err != nil && !isGone(err) {
		addErr(&resp.Diagnostics, "Error deleting subscription", err)
	}
}

var _ = fmt.Sprintf
