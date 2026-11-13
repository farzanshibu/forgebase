// Package provider implements the ForgeBase Terraform provider.
package provider

import (
	"context"
	"os"
	"strconv"
	"strings"

	"github.com/hashicorp/terraform-plugin-framework/datasource"
	"github.com/hashicorp/terraform-plugin-framework/path"
	"github.com/hashicorp/terraform-plugin-framework/provider"
	"github.com/hashicorp/terraform-plugin-framework/provider/schema"
	"github.com/hashicorp/terraform-plugin-framework/resource"
	"github.com/hashicorp/terraform-plugin-framework/types"

	"github.com/farzanshibu/forgebase/integrations/terraform-provider-forgebase/internal/client"
)

var _ provider.Provider = (*forgebaseProvider)(nil)

// New returns the provider factory.
func New(version string) func() provider.Provider {
	return func() provider.Provider { return &forgebaseProvider{version: version} }
}

type forgebaseProvider struct{ version string }

type providerModel struct {
	URL      types.String `tfsdk:"url"`
	Token    types.String `tfsdk:"token"`
	Realm    types.String `tfsdk:"realm"`
	Org      types.String `tfsdk:"org"`
	Tenant   types.String `tfsdk:"tenant"`
	Insecure types.Bool   `tfsdk:"insecure_skip_verify"`
}

func (p *forgebaseProvider) Metadata(_ context.Context, _ provider.MetadataRequest, resp *provider.MetadataResponse) {
	resp.TypeName = "forgebase"
	resp.Version = p.version
}

func (p *forgebaseProvider) Schema(_ context.Context, _ provider.SchemaRequest, resp *provider.SchemaResponse) {
	resp.Schema = schema.Schema{
		Description: "Manage ForgeBase configuration (collections, storage, queues, functions, identity and more) through its REST API.",
		Attributes: map[string]schema.Attribute{
			"url": schema.StringAttribute{
				Optional:    true,
				Description: "Base URL of the ForgeBase server, e.g. `https://forgebase.example.com`. Defaults to the `FORGEBASE_URL` environment variable.",
			},
			"token": schema.StringAttribute{
				Optional:  true,
				Sensitive: true,
				Description: "Credential sent as `Authorization: Bearer <token>`: a service-account API key (`fbk_…`), a personal access token (`fbp_…`) " +
					"or an access token. Defaults to the `FORGEBASE_TOKEN` environment variable.",
			},
			"realm": schema.StringAttribute{
				Optional:    true,
				Description: "Realm sent as the `X-Forgebase-Realm` header. Defaults to the `FORGEBASE_REALM` environment variable.",
			},
			"org": schema.StringAttribute{
				Optional:    true,
				Description: "Organization sent as the `X-Forgebase-Org` header. Defaults to the `FORGEBASE_ORG` environment variable.",
			},
			"tenant": schema.StringAttribute{
				Optional:    true,
				Description: "Tenant (environment id) sent as the `X-Forgebase-Tenant` header, so resources are created inside that environment. Defaults to the `FORGEBASE_TENANT` environment variable.",
			},
			"insecure_skip_verify": schema.BoolAttribute{
				Optional:    true,
				Description: "Skip TLS certificate verification. Only for development. Defaults to the `FORGEBASE_INSECURE` environment variable.",
			},
		},
	}
}

func (p *forgebaseProvider) Configure(ctx context.Context, req provider.ConfigureRequest, resp *provider.ConfigureResponse) {
	var cfg providerModel
	resp.Diagnostics.Append(req.Config.Get(ctx, &cfg)...)
	if resp.Diagnostics.HasError() {
		return
	}
	for name, v := range map[string]types.String{"url": cfg.URL, "token": cfg.Token, "realm": cfg.Realm, "org": cfg.Org, "tenant": cfg.Tenant} {
		if v.IsUnknown() {
			resp.Diagnostics.AddAttributeError(path.Root(name), "Unknown provider value",
				"The "+name+" is not known until apply. Set it to a value known at plan time, or use the environment variable.")
		}
	}
	if resp.Diagnostics.HasError() {
		return
	}
	pick := func(v types.String, env string) string {
		if !v.IsNull() && v.ValueString() != "" {
			return v.ValueString()
		}
		return os.Getenv(env)
	}
	url := pick(cfg.URL, "FORGEBASE_URL")
	token := pick(cfg.Token, "FORGEBASE_TOKEN")
	if url == "" {
		resp.Diagnostics.AddAttributeError(path.Root("url"), "Missing ForgeBase URL",
			"Set the `url` provider argument or the FORGEBASE_URL environment variable.")
	}
	if token == "" {
		resp.Diagnostics.AddAttributeWarning(path.Root("token"), "No ForgeBase credential",
			"No `token` or FORGEBASE_TOKEN is set. Requests are anonymous and most management APIs will answer 401 or 403.")
	}
	insecure := false
	if !cfg.Insecure.IsNull() {
		insecure = cfg.Insecure.ValueBool()
	} else if v, err := strconv.ParseBool(strings.TrimSpace(os.Getenv("FORGEBASE_INSECURE"))); err == nil {
		insecure = v
	}
	if resp.Diagnostics.HasError() {
		return
	}
	c, err := client.New(client.Config{
		URL: url, Token: token,
		Realm: pick(cfg.Realm, "FORGEBASE_REALM"), Org: pick(cfg.Org, "FORGEBASE_ORG"), Tenant: pick(cfg.Tenant, "FORGEBASE_TENANT"),
		Insecure: insecure, UserAgent: "terraform-provider-forgebase/" + p.version,
	})
	if err != nil {
		resp.Diagnostics.AddAttributeError(path.Root("url"), "Invalid provider configuration", err.Error())
		return
	}
	resp.ResourceData = c
	resp.DataSourceData = c
}

func (p *forgebaseProvider) Resources(context.Context) []func() resource.Resource {
	return []func() resource.Resource{
		NewCollectionResource,
		NewIndexResource,
		NewCollectionRulesResource,
		NewStorageBucketResource,
		NewStorageBucketACLResource,
		NewQueueTopicResource,
		NewQueueSubscriptionResource,
		NewFunctionResource,
		NewServiceAccountResource,
		NewAPIKeyResource,
		NewOAuthClientResource,
		NewRoleResource,
		NewOrgResource,
		NewProjectResource,
		NewFeatureResource,
		NewPlanResource,
	}
}

func (p *forgebaseProvider) DataSources(context.Context) []func() datasource.DataSource {
	return []func() datasource.DataSource{
		NewCollectionDataSource,
		NewStorageBucketDataSource,
		NewQueueTopicDataSource,
		NewFunctionDataSource,
		NewServiceAccountDataSource,
		NewOrgDataSource,
		NewProjectDataSource,
		NewPlanDataSource,
	}
}
