package provider

import (
	"context"
	"encoding/json"
	"regexp"

	"github.com/hashicorp/terraform-plugin-framework-validators/int64validator"
	"github.com/hashicorp/terraform-plugin-framework-validators/setvalidator"
	"github.com/hashicorp/terraform-plugin-framework-validators/stringvalidator"
	"github.com/hashicorp/terraform-plugin-framework/attr"
	"github.com/hashicorp/terraform-plugin-framework/path"
	"github.com/hashicorp/terraform-plugin-framework/resource"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/booldefault"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/int64planmodifier"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/planmodifier"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/setdefault"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/stringdefault"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/stringplanmodifier"
	"github.com/hashicorp/terraform-plugin-framework/schema/validator"
	"github.com/hashicorp/terraform-plugin-framework/types"
)

var (
	slugRE  = regexp.MustCompile(`^[a-z0-9_-]{1,64}$`)
	scopeRE = regexp.MustCompile(`^(\*|[a-z0-9_-]+:(\*|read|write)(:[^\s:]+)?)$`)
)

var (
	idState = []planmodifier.String{stringplanmodifier.UseStateForUnknown()}
	replace = []planmodifier.String{stringplanmodifier.RequiresReplace()}
)

// =============================================================================
// forgebase_service_account
// =============================================================================

var (
	_ resource.Resource                = (*serviceAccountResource)(nil)
	_ resource.ResourceWithConfigure   = (*serviceAccountResource)(nil)
	_ resource.ResourceWithImportState = (*serviceAccountResource)(nil)
)

// NewServiceAccountResource returns the forgebase_service_account resource.
func NewServiceAccountResource() resource.Resource { return &serviceAccountResource{} }

type serviceAccountResource struct{ base }

type serviceAccountModel struct {
	ID          types.String `tfsdk:"id"`
	Name        types.String `tfsdk:"name"`
	Description types.String `tfsdk:"description"`
	Role        types.String `tfsdk:"role"`
	Realm       types.String `tfsdk:"realm"`
	Disabled    types.Bool   `tfsdk:"disabled"`
	CreatedBy   types.String `tfsdk:"created_by"`
	CreatedAt   types.String `tfsdk:"created_at"`
}

type apiServiceAccount struct {
	ID          string `json:"id"`
	Name        string `json:"name"`
	Description string `json:"description"`
	Role        string `json:"role"`
	CreatedBy   string `json:"created_by"`
	CreatedAt   string `json:"created_at"`
	Disabled    bool   `json:"disabled"`
	Realm       string `json:"realm"`
}

func (a apiServiceAccount) model() serviceAccountModel {
	return serviceAccountModel{
		ID: types.StringValue(a.ID), Name: types.StringValue(a.Name), Description: types.StringValue(a.Description),
		Role: types.StringValue(a.Role), Realm: types.StringValue(a.Realm), Disabled: types.BoolValue(a.Disabled),
		CreatedBy: types.StringValue(a.CreatedBy), CreatedAt: types.StringValue(a.CreatedAt),
	}
}

func (r *serviceAccountResource) Metadata(_ context.Context, req resource.MetadataRequest, resp *resource.MetadataResponse) {
	resp.TypeName = req.ProviderTypeName + "_service_account"
}

func (r *serviceAccountResource) Configure(_ context.Context, req resource.ConfigureRequest, resp *resource.ConfigureResponse) {
	r.configureResource(req, resp)
}

func (r *serviceAccountResource) Schema(_ context.Context, _ resource.SchemaRequest, resp *resource.SchemaResponse) {
	resp.Schema = schema.Schema{
		Description: "A service account: a machine identity that owns API keys (`forgebase_api_key`). The server has no update call for service accounts, so every change replaces it.",
		Attributes: map[string]schema.Attribute{
			"id": schema.StringAttribute{Computed: true, PlanModifiers: idState, Description: "Server-assigned id."},
			"name": schema.StringAttribute{
				Required: true, PlanModifiers: replace, Description: "Unique name (1-100 printable characters).",
				Validators: []validator.String{stringvalidator.LengthBetween(1, 100)},
			},
			"description": schema.StringAttribute{
				Optional: true, Computed: true, Default: stringdefault.StaticString(""), PlanModifiers: replace,
				Description: "Free text (at most 1000 characters).", Validators: []validator.String{stringvalidator.LengthAtMost(1000)},
			},
			"role": schema.StringAttribute{
				Optional: true, Computed: true, Default: stringdefault.StaticString("service"), PlanModifiers: replace,
				Description: "`service` (default) or `admin` (a platform administrator; only platform administrators may create one).",
				Validators:  []validator.String{stringvalidator.OneOf("service", "admin")},
			},
			"realm":      schema.StringAttribute{Computed: true, PlanModifiers: idState, Description: "Realm the account belongs to."},
			"disabled":   schema.BoolAttribute{Computed: true, Description: "Whether the account is disabled (its keys stop working)."},
			"created_by": schema.StringAttribute{Computed: true, PlanModifiers: idState, Description: "Creator."},
			"created_at": schema.StringAttribute{Computed: true, PlanModifiers: idState, Description: "Creation time."},
		},
	}
}

func (r *serviceAccountResource) ImportState(ctx context.Context, req resource.ImportStateRequest, resp *resource.ImportStateResponse) {
	resp.Diagnostics.Append(resp.State.SetAttribute(ctx, path.Root("id"), req.ID)...)
}

func (r *serviceAccountResource) Create(ctx context.Context, req resource.CreateRequest, resp *resource.CreateResponse) {
	var plan serviceAccountModel
	resp.Diagnostics.Append(req.Plan.Get(ctx, &plan)...)
	if resp.Diagnostics.HasError() {
		return
	}
	var out apiServiceAccount
	body := map[string]any{"name": plan.Name.ValueString(), "description": plan.Description.ValueString(), "role": plan.Role.ValueString()}
	if err := r.c.Post(ctx, "/api/auth/service-accounts", body, &out); err != nil {
		addErr(&resp.Diagnostics, "Error creating service account", err)
		return
	}
	resp.Diagnostics.Append(resp.State.Set(ctx, out.model())...)
}

func (r *serviceAccountResource) Read(ctx context.Context, req resource.ReadRequest, resp *resource.ReadResponse) {
	var state serviceAccountModel
	resp.Diagnostics.Append(req.State.Get(ctx, &state)...)
	if resp.Diagnostics.HasError() {
		return
	}
	var out apiServiceAccount
	err := r.c.Get(ctx, "/api/auth/service-accounts/"+seg(state.ID.ValueString()), nil, &out)
	if removeIfGone(ctx, &resp.State, &resp.Diagnostics, "service account", err) {
		return
	}
	resp.Diagnostics.Append(resp.State.Set(ctx, out.model())...)
}

func (r *serviceAccountResource) Update(_ context.Context, _ resource.UpdateRequest, resp *resource.UpdateResponse) {
	resp.Diagnostics.AddError("Service account cannot be updated in place", "Every attribute forces replacement; this is a provider bug.")
}

func (r *serviceAccountResource) Delete(ctx context.Context, req resource.DeleteRequest, resp *resource.DeleteResponse) {
	var state serviceAccountModel
	resp.Diagnostics.Append(req.State.Get(ctx, &state)...)
	if resp.Diagnostics.HasError() {
		return
	}
	if err := r.c.Delete(ctx, "/api/auth/service-accounts/"+seg(state.ID.ValueString()), nil); err != nil && !isGone(err) {
		addErr(&resp.Diagnostics, "Error deleting service account", err)
	}
}

// =============================================================================
// forgebase_api_key
// =============================================================================

var (
	_ resource.Resource                = (*apiKeyResource)(nil)
	_ resource.ResourceWithConfigure   = (*apiKeyResource)(nil)
	_ resource.ResourceWithImportState = (*apiKeyResource)(nil)
)

// NewAPIKeyResource returns the forgebase_api_key resource.
func NewAPIKeyResource() resource.Resource { return &apiKeyResource{} }

type apiKeyResource struct{ base }

type apiKeyModel struct {
	ID               types.String `tfsdk:"id"`
	ServiceAccountID types.String `tfsdk:"service_account_id"`
	Name             types.String `tfsdk:"name"`
	Scopes           types.Set    `tfsdk:"scopes"`
	ExpiresInDays    types.Int64  `tfsdk:"expires_in_days"`
	Tenant           types.String `tfsdk:"tenant"`
	Key              types.String `tfsdk:"key"`
	CreatedAt        types.String `tfsdk:"created_at"`
	ExpiresAt        types.String `tfsdk:"expires_at"`
}

type apiKeyView struct {
	ID        string   `json:"id"`
	Name      string   `json:"name"`
	Scopes    []string `json:"scopes"`
	CreatedAt string   `json:"created_at"`
	ExpiresAt *string  `json:"expires_at"`
	Revoked   bool     `json:"revoked"`
	Tenant    *string  `json:"tenant"`
}

func (r *apiKeyResource) Metadata(_ context.Context, req resource.MetadataRequest, resp *resource.MetadataResponse) {
	resp.TypeName = req.ProviderTypeName + "_api_key"
}

func (r *apiKeyResource) Configure(_ context.Context, req resource.ConfigureRequest, resp *resource.ConfigureResponse) {
	r.configureResource(req, resp)
}

func (r *apiKeyResource) Schema(_ context.Context, _ resource.SchemaRequest, resp *resource.SchemaResponse) {
	resp.Schema = schema.Schema{
		Description: "An API key of a service account. The key secret (`key`) is returned by the server once, at creation, and is kept in state as a sensitive value; " +
			"it cannot be read back or recovered later, so an imported key has no `key`. Keys cannot be changed: every change creates a new key and revokes the old one. " +
			"A key revoked outside Terraform is dropped from state, so the next apply issues a new one.",
		Attributes: map[string]schema.Attribute{
			"id":                 schema.StringAttribute{Computed: true, PlanModifiers: idState, Description: "Key id (also visible inside the key)."},
			"service_account_id": schema.StringAttribute{Required: true, PlanModifiers: replace, Description: "Id of the owning service account."},
			"name":               schema.StringAttribute{Required: true, PlanModifiers: replace, Description: "Key name (1-100 printable characters).", Validators: []validator.String{stringvalidator.LengthBetween(1, 100)}},
			"scopes": schema.SetAttribute{
				Required: true, ElementType: types.StringType,
				Description: "Scopes narrowing what the key may do: `*`, `<area>:*`, `<area>:read`, `<area>:write`, or `<area>:read|write:<name>`. `write` does not imply `read`.",
				Validators: []validator.Set{
					setvalidator.SizeAtLeast(1),
					setvalidator.ValueStringsAre(stringvalidator.RegexMatches(scopeRE, "must be `*`, `<area>:*`, `<area>:read|write` or `<area>:read|write:<name>`")),
				},
			},
			"expires_in_days": schema.Int64Attribute{
				Optional: true, Description: "Lifetime in days (1-3650). Omit for a key that does not expire.",
				Validators:    []validator.Int64{int64validator.Between(1, 3650)},
				PlanModifiers: []planmodifier.Int64{int64planmodifier.RequiresReplace()},
			},
			"tenant":     schema.StringAttribute{Optional: true, PlanModifiers: replace, Description: "Bind the key to one tenant (environment id)."},
			"key":        schema.StringAttribute{Computed: true, Sensitive: true, PlanModifiers: idState, Description: "The key secret (`fbk_…`). Only known when Terraform created the key."},
			"created_at": schema.StringAttribute{Computed: true, PlanModifiers: idState, Description: "Creation time."},
			"expires_at": schema.StringAttribute{Computed: true, PlanModifiers: idState, Description: "Expiry time, if the key expires."},
		},
	}
}

// ImportState accepts `<service_account_id>/<key_id>`.
func (r *apiKeyResource) ImportState(ctx context.Context, req resource.ImportStateRequest, resp *resource.ImportStateResponse) {
	sa, id, ok := splitID(req.ID)
	if !ok {
		resp.Diagnostics.AddError("Invalid import id", "Use `<service_account_id>/<key_id>`.")
		return
	}
	resp.Diagnostics.Append(resp.State.SetAttribute(ctx, path.Root("service_account_id"), sa)...)
	resp.Diagnostics.Append(resp.State.SetAttribute(ctx, path.Root("id"), id)...)
	resp.Diagnostics.Append(resp.State.SetAttribute(ctx, path.Root("key"), types.StringNull())...)
}

func (r *apiKeyResource) Create(ctx context.Context, req resource.CreateRequest, resp *resource.CreateResponse) {
	var plan apiKeyModel
	resp.Diagnostics.Append(req.Plan.Get(ctx, &plan)...)
	if resp.Diagnostics.HasError() {
		return
	}
	body := map[string]any{"name": plan.Name.ValueString(), "scopes": setStrings(plan.Scopes)}
	if p := int64Ptr(plan.ExpiresInDays); p != nil {
		body["expires_in_days"] = *p
	}
	if p := strPtr(plan.Tenant); p != nil {
		body["tenant"] = *p
	}
	var out struct {
		ID  string `json:"id"`
		Key string `json:"key"`
	}
	if err := r.c.Post(ctx, "/api/auth/service-accounts/"+seg(plan.ServiceAccountID.ValueString())+"/keys", body, &out); err != nil {
		addErr(&resp.Diagnostics, "Error creating API key", err)
		return
	}
	plan.ID, plan.Key = types.StringValue(out.ID), types.StringValue(out.Key)
	plan.CreatedAt, plan.ExpiresAt = types.StringNull(), types.StringNull()
	// Fill created_at / expires_at from the key list.
	if v, err := r.find(ctx, plan.ServiceAccountID.ValueString(), out.ID); err == nil && v != nil {
		plan.CreatedAt = types.StringValue(v.CreatedAt)
		plan.ExpiresAt = nullableString(v.ExpiresAt)
	}
	resp.Diagnostics.Append(resp.State.Set(ctx, plan)...)
}

func (r *apiKeyResource) find(ctx context.Context, sa, id string) (*apiKeyView, error) {
	var l items[apiKeyView]
	if err := r.c.Get(ctx, "/api/auth/service-accounts/"+seg(sa)+"/keys", nil, &l); err != nil {
		return nil, err
	}
	for i := range l.Items {
		if l.Items[i].ID == id {
			return &l.Items[i], nil
		}
	}
	return nil, nil
}

func (r *apiKeyResource) Read(ctx context.Context, req resource.ReadRequest, resp *resource.ReadResponse) {
	var state apiKeyModel
	resp.Diagnostics.Append(req.State.Get(ctx, &state)...)
	if resp.Diagnostics.HasError() {
		return
	}
	v, err := r.find(ctx, state.ServiceAccountID.ValueString(), state.ID.ValueString())
	if err != nil {
		if removeIfGone(ctx, &resp.State, &resp.Diagnostics, "API key", err) {
			return
		}
	}
	if v == nil || v.Revoked {
		resp.State.RemoveResource(ctx)
		return
	}
	state.Name = types.StringValue(v.Name)
	state.Scopes = strSet(v.Scopes)
	state.CreatedAt = types.StringValue(v.CreatedAt)
	state.ExpiresAt = nullableString(v.ExpiresAt)
	state.Tenant = nullableString(v.Tenant)
	resp.Diagnostics.Append(resp.State.Set(ctx, state)...)
}

func (r *apiKeyResource) Update(_ context.Context, _ resource.UpdateRequest, resp *resource.UpdateResponse) {
	resp.Diagnostics.AddError("API key cannot be updated in place", "Every attribute forces replacement; this is a provider bug.")
}

func (r *apiKeyResource) Delete(ctx context.Context, req resource.DeleteRequest, resp *resource.DeleteResponse) {
	var state apiKeyModel
	resp.Diagnostics.Append(req.State.Get(ctx, &state)...)
	if resp.Diagnostics.HasError() {
		return
	}
	if err := r.c.Delete(ctx, "/api/auth/api-keys/"+seg(state.ID.ValueString()), nil); err != nil && !isGone(err) {
		addErr(&resp.Diagnostics, "Error revoking API key", err)
	}
}

// =============================================================================
// forgebase_oauth_client
// =============================================================================

var (
	_ resource.Resource                = (*oauthClientResource)(nil)
	_ resource.ResourceWithConfigure   = (*oauthClientResource)(nil)
	_ resource.ResourceWithImportState = (*oauthClientResource)(nil)
)

// NewOAuthClientResource returns the forgebase_oauth_client resource.
func NewOAuthClientResource() resource.Resource { return &oauthClientResource{} }

type oauthClientResource struct{ base }

type oauthClientModel struct {
	ID                     types.String `tfsdk:"id"`
	Realm                  types.String `tfsdk:"realm"`
	Name                   types.String `tfsdk:"name"`
	Description            types.String `tfsdk:"description"`
	Type                   types.String `tfsdk:"type"`
	RedirectURIs           types.Set    `tfsdk:"redirect_uris"`
	PostLogoutRedirectURIs types.Set    `tfsdk:"post_logout_redirect_uris"`
	GrantTypes             types.Set    `tfsdk:"grant_types"`
	Scopes                 types.Set    `tfsdk:"scopes"`
	SkipConsent            types.Bool   `tfsdk:"skip_consent"`
	AccessTokenTTL         types.Int64  `tfsdk:"access_token_ttl"`
	RefreshTokenTTL        types.Int64  `tfsdk:"refresh_token_ttl"`
	Disabled               types.Bool   `tfsdk:"disabled"`
	ClientSecret           types.String `tfsdk:"client_secret"`
	CreatedAt              types.String `tfsdk:"created_at"`
	UpdatedAt              types.String `tfsdk:"updated_at"`
}

type apiOAuthClient struct {
	ClientID               string   `json:"client_id"`
	Realm                  string   `json:"realm"`
	Name                   string   `json:"name"`
	Description            *string  `json:"description"`
	Type                   string   `json:"type"`
	RedirectURIs           []string `json:"redirect_uris"`
	PostLogoutRedirectURIs []string `json:"post_logout_redirect_uris"`
	GrantTypes             []string `json:"grant_types"`
	Scopes                 []string `json:"scopes"`
	SkipConsent            bool     `json:"skip_consent"`
	AccessTokenTTL         *int64   `json:"access_token_ttl"`
	RefreshTokenTTL        *int64   `json:"refresh_token_ttl"`
	Disabled               bool     `json:"disabled"`
	CreatedAt              string   `json:"created_at"`
	UpdatedAt              string   `json:"updated_at"`
}

var (
	defaultGrantTypes = []attr.Value{types.StringValue("authorization_code"), types.StringValue("refresh_token")}
	defaultScopes     = []attr.Value{types.StringValue("openid"), types.StringValue("profile"), types.StringValue("email"), types.StringValue("offline_access")}
	grantTypeNames    = []string{"authorization_code", "refresh_token", "client_credentials", "urn:ietf:params:oauth:grant-type:device_code"}
)

func (r *oauthClientResource) Metadata(_ context.Context, req resource.MetadataRequest, resp *resource.MetadataResponse) {
	resp.TypeName = req.ProviderTypeName + "_oauth_client"
}

func (r *oauthClientResource) Configure(_ context.Context, req resource.ConfigureRequest, resp *resource.ConfigureResponse) {
	r.configureResource(req, resp)
}

func ttlClearReplaces(_ context.Context, req planmodifier.Int64Request, resp *int64planmodifier.RequiresReplaceIfFuncResponse) {
	// The server keeps an override once set; going back to the default needs a new client.
	resp.RequiresReplace = !req.StateValue.IsNull() && req.PlanValue.IsNull()
}

func (r *oauthClientResource) Schema(_ context.Context, _ resource.SchemaRequest, resp *resource.SchemaResponse) {
	ttl := func(desc string, lo, hi int64) schema.Int64Attribute {
		return schema.Int64Attribute{
			Optional: true, Description: desc + " Removing a set value replaces the client (the server cannot clear an override).",
			Validators: []validator.Int64{int64validator.Between(lo, hi)},
			PlanModifiers: []planmodifier.Int64{int64planmodifier.RequiresReplaceIf(ttlClearReplaces,
				"Replace when a lifetime override is removed.", "Replace when a lifetime override is removed.")},
		}
	}
	resp.Schema = schema.Schema{
		Description: "An OAuth 2.0 / OpenID Connect client app of ForgeBase's identity provider. A confidential client's secret is returned by the server once, " +
			"at creation, and is kept in `client_secret` (sensitive). Rotating the secret is not a Terraform operation; replace the resource " +
			"(`terraform apply -replace=…`) or call `POST /api/auth/clients/{id}/secret` for a graceful rotation, then update consumers.",
		Attributes: map[string]schema.Attribute{
			"id": schema.StringAttribute{Computed: true, PlanModifiers: idState, Description: "The `client_id`."},
			"realm": schema.StringAttribute{
				Optional: true, Computed: true, PlanModifiers: []planmodifier.String{stringplanmodifier.RequiresReplace(), stringplanmodifier.UseStateForUnknown()},
				Description: "Realm of the client. Defaults to the provider's realm, else `default`. Cannot change.",
			},
			"name":        schema.StringAttribute{Required: true, Description: "Display name shown on the consent screen."},
			"description": schema.StringAttribute{Optional: true, Description: "Description."},
			"type": schema.StringAttribute{
				Optional: true, Computed: true, Default: stringdefault.StaticString("confidential"), PlanModifiers: replace,
				Description: "`confidential` (keeps a secret; default) or `public` (browser or native app; PKCE required). Cannot change.",
				Validators:  []validator.String{stringvalidator.OneOf("confidential", "public")},
			},
			"redirect_uris": schema.SetAttribute{
				Optional: true, Computed: true, ElementType: types.StringType,
				Default:     setdefault.StaticValue(types.SetValueMust(types.StringType, []attr.Value{})),
				Description: "Allowed redirect URIs (exact match, at most 20). Required for the `authorization_code` grant.",
				Validators:  []validator.Set{setvalidator.SizeAtMost(20)},
			},
			"post_logout_redirect_uris": schema.SetAttribute{
				Optional: true, Computed: true, ElementType: types.StringType,
				Default:     setdefault.StaticValue(types.SetValueMust(types.StringType, []attr.Value{})),
				Description: "Allowed post-logout redirect URIs (exact match, at most 20).",
				Validators:  []validator.Set{setvalidator.SizeAtMost(20)},
			},
			"grant_types": schema.SetAttribute{
				Optional: true, Computed: true, ElementType: types.StringType,
				Default:     setdefault.StaticValue(types.SetValueMust(types.StringType, defaultGrantTypes)),
				Description: "Allowed grants: " + joinCode(grantTypeNames) + ". Default `authorization_code` and `refresh_token`.",
				Validators:  []validator.Set{setvalidator.SizeAtLeast(1), setvalidator.ValueStringsAre(stringvalidator.OneOf(grantTypeNames...))},
			},
			"scopes": schema.SetAttribute{
				Optional: true, Computed: true, ElementType: types.StringType,
				Default:     setdefault.StaticValue(types.SetValueMust(types.StringType, defaultScopes)),
				Description: "Scopes the client may request: `openid`, `profile`, `email`, `phone`, `offline_access` or a ForgeBase API scope. Default `openid profile email offline_access`.",
			},
			"skip_consent":      schema.BoolAttribute{Optional: true, Computed: true, Default: booldefault.StaticBool(false), Description: "First-party app: no consent screen."},
			"access_token_ttl":  ttl("Access-token lifetime in seconds (60-86400).", 60, 86400),
			"refresh_token_ttl": ttl("Refresh-token family lifetime in seconds (300-31536000).", 300, 31536000),
			"disabled":          schema.BoolAttribute{Optional: true, Computed: true, Default: booldefault.StaticBool(false), Description: "Refuse new authorizations and tokens."},
			"client_secret": schema.StringAttribute{
				Computed: true, Sensitive: true, PlanModifiers: idState,
				Description: "Secret of a confidential client. Only known when Terraform created the client (null for public clients and imported ones).",
			},
			"created_at": schema.StringAttribute{Computed: true, PlanModifiers: idState, Description: "Creation time."},
			"updated_at": schema.StringAttribute{Computed: true, Description: "Last change."},
		},
	}
}

func (r *oauthClientResource) ImportState(ctx context.Context, req resource.ImportStateRequest, resp *resource.ImportStateResponse) {
	resp.Diagnostics.Append(resp.State.SetAttribute(ctx, path.Root("id"), req.ID)...)
	resp.Diagnostics.Append(resp.State.SetAttribute(ctx, path.Root("client_secret"), types.StringNull())...)
}

func (a apiOAuthClient) model(secret types.String) oauthClientModel {
	desc := types.StringNull()
	if a.Description != nil && *a.Description != "" {
		desc = types.StringValue(*a.Description)
	}
	return oauthClientModel{
		ID: types.StringValue(a.ClientID), Realm: types.StringValue(a.Realm), Name: types.StringValue(a.Name), Description: desc,
		Type: types.StringValue(a.Type), RedirectURIs: strSet(a.RedirectURIs), PostLogoutRedirectURIs: strSet(a.PostLogoutRedirectURIs),
		GrantTypes: strSet(a.GrantTypes), Scopes: strSet(a.Scopes), SkipConsent: types.BoolValue(a.SkipConsent),
		AccessTokenTTL: nullableInt64(a.AccessTokenTTL), RefreshTokenTTL: nullableInt64(a.RefreshTokenTTL),
		Disabled: types.BoolValue(a.Disabled), ClientSecret: secret,
		CreatedAt: types.StringValue(a.CreatedAt), UpdatedAt: types.StringValue(a.UpdatedAt),
	}
}

func (m oauthClientModel) body() map[string]any {
	b := map[string]any{
		"name": m.Name.ValueString(), "redirect_uris": setStrings(m.RedirectURIs), "post_logout_redirect_uris": setStrings(m.PostLogoutRedirectURIs),
		"grant_types": setStrings(m.GrantTypes), "scopes": setStrings(m.Scopes), "skip_consent": m.SkipConsent.ValueBool(),
	}
	if b["redirect_uris"] == nil {
		b["redirect_uris"] = []string{}
	}
	if b["post_logout_redirect_uris"] == nil {
		b["post_logout_redirect_uris"] = []string{}
	}
	if p := int64Ptr(m.AccessTokenTTL); p != nil {
		b["access_token_ttl"] = *p
	}
	if p := int64Ptr(m.RefreshTokenTTL); p != nil {
		b["refresh_token_ttl"] = *p
	}
	return b
}

func (r *oauthClientResource) Create(ctx context.Context, req resource.CreateRequest, resp *resource.CreateResponse) {
	var plan oauthClientModel
	resp.Diagnostics.Append(req.Plan.Get(ctx, &plan)...)
	if resp.Diagnostics.HasError() {
		return
	}
	body := plan.body()
	body["type"] = plan.Type.ValueString()
	if p := strPtr(plan.Description); p != nil {
		body["description"] = *p
	}
	if !plan.Realm.IsNull() && !plan.Realm.IsUnknown() {
		body["realm"] = plan.Realm.ValueString()
	}
	var out struct {
		Client       apiOAuthClient `json:"client"`
		ClientSecret *string        `json:"client_secret"`
	}
	if err := r.c.Post(ctx, "/api/auth/clients", body, &out); err != nil {
		addErr(&resp.Diagnostics, "Error creating OAuth client", err)
		return
	}
	secret := types.StringNull()
	if out.ClientSecret != nil {
		secret = types.StringValue(*out.ClientSecret)
	}
	st := out.Client.model(secret)
	// `disabled` is a patch field, not a create field.
	if plan.Disabled.ValueBool() && !st.Disabled.ValueBool() {
		var patched apiOAuthClient
		if err := r.c.Patch(ctx, "/api/auth/clients/"+seg(st.ID.ValueString()), map[string]any{"disabled": true}, &patched); err != nil {
			resp.Diagnostics.Append(resp.State.Set(ctx, st)...)
			addErr(&resp.Diagnostics, "Error disabling OAuth client", err)
			return
		}
		st = patched.model(secret)
	}
	resp.Diagnostics.Append(resp.State.Set(ctx, st)...)
}

func (r *oauthClientResource) Read(ctx context.Context, req resource.ReadRequest, resp *resource.ReadResponse) {
	var state oauthClientModel
	resp.Diagnostics.Append(req.State.Get(ctx, &state)...)
	if resp.Diagnostics.HasError() {
		return
	}
	var out apiOAuthClient
	err := r.c.Get(ctx, "/api/auth/clients/"+seg(state.ID.ValueString()), nil, &out)
	if removeIfGone(ctx, &resp.State, &resp.Diagnostics, "OAuth client", err) {
		return
	}
	resp.Diagnostics.Append(resp.State.Set(ctx, out.model(state.ClientSecret))...)
}

func (r *oauthClientResource) Update(ctx context.Context, req resource.UpdateRequest, resp *resource.UpdateResponse) {
	var plan, state oauthClientModel
	resp.Diagnostics.Append(req.Plan.Get(ctx, &plan)...)
	resp.Diagnostics.Append(req.State.Get(ctx, &state)...)
	if resp.Diagnostics.HasError() {
		return
	}
	body := plan.body()
	body["disabled"] = plan.Disabled.ValueBool()
	// An empty description clears it (the server keeps the old value for an absent one).
	body["description"] = plan.Description.ValueString()
	var out apiOAuthClient
	if err := r.c.Patch(ctx, "/api/auth/clients/"+seg(state.ID.ValueString()), body, &out); err != nil {
		addErr(&resp.Diagnostics, "Error updating OAuth client", err)
		return
	}
	resp.Diagnostics.Append(resp.State.Set(ctx, out.model(state.ClientSecret))...)
}

func (r *oauthClientResource) Delete(ctx context.Context, req resource.DeleteRequest, resp *resource.DeleteResponse) {
	var state oauthClientModel
	resp.Diagnostics.Append(req.State.Get(ctx, &state)...)
	if resp.Diagnostics.HasError() {
		return
	}
	if err := r.c.Delete(ctx, "/api/auth/clients/"+seg(state.ID.ValueString()), nil); err != nil && !isGone(err) {
		addErr(&resp.Diagnostics, "Error deleting OAuth client", err)
	}
}

// =============================================================================
// shared
// =============================================================================

// splitID splits `a/b` at the first slash.
func splitID(id string) (string, string, bool) {
	for i := 0; i < len(id); i++ {
		if id[i] == '/' {
			if i == 0 || i == len(id)-1 {
				return "", "", false
			}
			return id[:i], id[i+1:], true
		}
	}
	return "", "", false
}

// decodeList decodes either a bare JSON array or an `{"items": [...]}` envelope.
func decodeList[T any](raw json.RawMessage) ([]T, error) {
	var arr []T
	if err := json.Unmarshal(raw, &arr); err == nil {
		return arr, nil
	}
	var env items[T]
	if err := json.Unmarshal(raw, &env); err != nil {
		return nil, err
	}
	return env.Items, nil
}
