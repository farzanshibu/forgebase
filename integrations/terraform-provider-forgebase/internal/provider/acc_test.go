package provider_test

import (
	"fmt"
	"os"
	"testing"

	"github.com/hashicorp/terraform-plugin-framework/providerserver"
	"github.com/hashicorp/terraform-plugin-go/tfprotov6"
	"github.com/hashicorp/terraform-plugin-testing/helper/acctest"
	"github.com/hashicorp/terraform-plugin-testing/helper/resource"

	"github.com/farzanshibu/forgebase/integrations/terraform-provider-forgebase/internal/provider"
)

// Acceptance tests run against a real ForgeBase server and need the Terraform
// CLI. They are skipped unless TF_ACC=1 and the server is configured:
//
//	TF_ACC=1 FORGEBASE_URL=http://127.0.0.1:8090 FORGEBASE_TOKEN=<admin key> go test ./internal/provider -run TestAcc -v
//
// The credential needs a platform administrator (collection rules, plans,
// features and roles are administrator-only).

var accFactories = map[string]func() (tfprotov6.ProviderServer, error){
	"forgebase": providerserver.NewProtocol6WithError(provider.New("acc")()),
}

func accPreCheck(t *testing.T) {
	t.Helper()
	for _, v := range []string{"FORGEBASE_URL", "FORGEBASE_TOKEN"} {
		if os.Getenv(v) == "" {
			t.Fatalf("%s must be set for acceptance tests", v)
		}
	}
}

func accName(prefix string) string {
	return prefix + acctest.RandStringFromCharSet(8, "abcdefghijklmnopqrstuvwxyz")
}

const accProvider = `provider "forgebase" {}` + "\n"

func TestAccCollection_basic(t *testing.T) {
	name := accName("tfacc_posts_")
	cfg := func(extra string) string {
		return accProvider + fmt.Sprintf(`
resource "forgebase_collection" "posts" {
  name = %q
  fields = [
    { name = "title", type = "text", required = true, options = jsonencode({ max_length = 200 }) },
    { name = "status", type = "select", options = jsonencode({ values = ["draft", "live"] }), default = jsonencode("draft") },
    %s
  ]
}
`, name, extra)
	}
	resource.Test(t, resource.TestCase{
		PreCheck: func() { accPreCheck(t) }, ProtoV6ProviderFactories: accFactories,
		Steps: []resource.TestStep{
			{
				Config: cfg(""),
				Check: resource.ComposeAggregateTestCheckFunc(
					resource.TestCheckResourceAttr("forgebase_collection.posts", "name", name),
					resource.TestCheckResourceAttr("forgebase_collection.posts", "fields.#", "2"),
					resource.TestCheckResourceAttrSet("forgebase_collection.posts", "fields.0.id"),
				),
			},
			{ // add a field in place
				Config: cfg(`{ name = "views", type = "number", options = jsonencode({ integer = true }) }`),
				Check:  resource.TestCheckResourceAttr("forgebase_collection.posts", "fields.#", "3"),
			},
			{ResourceName: "forgebase_collection.posts", ImportState: true, ImportStateVerify: true, ImportStateVerifyIdentifierAttribute: "name",
				ImportStateVerifyIgnore: []string{"fields.0.options", "fields.1.options", "fields.2.options", "fields.0.renamed_from", "fields.1.renamed_from", "fields.2.renamed_from"}},
		},
	})
}

func TestAccStorageBucket_withACL(t *testing.T) {
	name := accName("tfacc-bucket-")
	cfg := func(visibility string) string {
		return accProvider + fmt.Sprintf(`
resource "forgebase_storage_bucket" "b" {
  name       = %q
  visibility = %q
  allowed_content_types = ["image/png"]
  force_destroy = true
}

resource "forgebase_storage_bucket_acl" "b" {
  bucket = forgebase_storage_bucket.b.name
  entries = [{ grantee = "authenticated", permission = "read", prefix = "shared/" }]
}
`, name, visibility)
	}
	resource.Test(t, resource.TestCase{
		PreCheck: func() { accPreCheck(t) }, ProtoV6ProviderFactories: accFactories,
		Steps: []resource.TestStep{
			{Config: cfg("private"), Check: resource.TestCheckResourceAttr("forgebase_storage_bucket.b", "visibility", "private")},
			{Config: cfg("public"), Check: resource.TestCheckResourceAttr("forgebase_storage_bucket.b", "visibility", "public")},
			{ResourceName: "forgebase_storage_bucket.b", ImportState: true, ImportStateVerify: true, ImportStateVerifyIgnore: []string{"force_destroy"}},
			{ResourceName: "forgebase_storage_bucket_acl.b", ImportState: true, ImportStateVerify: true},
		},
	})
}

func TestAccQueue_topicAndSubscription(t *testing.T) {
	name := accName("tfacc-q-")
	cfg := func(attempts int) string {
		return accProvider + fmt.Sprintf(`
resource "forgebase_queue_topic" "t" {
  name          = %q
  force_destroy = true
}

resource "forgebase_queue_subscription" "s" {
  name                  = "%s-sub"
  topic                 = forgebase_queue_topic.t.name
  max_delivery_attempts = %d
}
`, name, name, attempts)
	}
	resource.Test(t, resource.TestCase{
		PreCheck: func() { accPreCheck(t) }, ProtoV6ProviderFactories: accFactories,
		Steps: []resource.TestStep{
			{Config: cfg(3), Check: resource.TestCheckResourceAttr("forgebase_queue_subscription.s", "max_delivery_attempts", "3")},
			{Config: cfg(5), Check: resource.TestCheckResourceAttr("forgebase_queue_subscription.s", "max_delivery_attempts", "5")},
			{ResourceName: "forgebase_queue_topic.t", ImportState: true, ImportStateVerify: true, ImportStateVerifyIgnore: []string{"force_destroy"}},
			{ResourceName: "forgebase_queue_subscription.s", ImportState: true, ImportStateVerify: true, ImportStateVerifyIgnore: []string{"start"}},
		},
	})
}

func TestAccFunction_inlineSource(t *testing.T) {
	name := accName("tfacc-fn-")
	cfg := func(body string) string {
		return accProvider + fmt.Sprintf(`
resource "forgebase_function" "f" {
  name         = %q
  language     = "javascript"
  capabilities = ["log"]
  source       = <<-JS
    import { serve } from "forgebase";
    serve(() => new Response(%q));
  JS
  secrets = { GREETING = "hello" }
}
`, name, body)
	}
	resource.Test(t, resource.TestCase{
		PreCheck: func() { accPreCheck(t) }, ProtoV6ProviderFactories: accFactories,
		Steps: []resource.TestStep{
			{Config: cfg("v1"), Check: resource.TestCheckResourceAttr("forgebase_function.f", "active_version", "1")},
			{Config: cfg("v2"), Check: resource.TestCheckResourceAttr("forgebase_function.f", "active_version", "2")},
			{ResourceName: "forgebase_function.f", ImportState: true, ImportStateVerify: true, ImportStateVerifyIgnore: []string{"secrets"}},
		},
	})
}

func TestAccServiceAccount_withKey(t *testing.T) {
	name := accName("tfacc-sa-")
	resource.Test(t, resource.TestCase{
		PreCheck: func() { accPreCheck(t) }, ProtoV6ProviderFactories: accFactories,
		Steps: []resource.TestStep{{
			Config: accProvider + fmt.Sprintf(`
resource "forgebase_service_account" "sa" {
  name = %q
}

resource "forgebase_api_key" "k" {
  service_account_id = forgebase_service_account.sa.id
  name               = "ci"
  scopes             = ["collections:read"]
}
`, name),
			Check: resource.ComposeAggregateTestCheckFunc(
				resource.TestCheckResourceAttrSet("forgebase_api_key.k", "key"),
				resource.TestCheckResourceAttr("forgebase_service_account.sa", "role", "service"),
			),
		}},
	})
}

func TestAccOAuthClient_basic(t *testing.T) {
	name := accName("tfacc-app-")
	cfg := func(uri string) string {
		return accProvider + fmt.Sprintf(`
resource "forgebase_oauth_client" "c" {
  name          = %q
  redirect_uris = [%q]
}
`, name, uri)
	}
	resource.Test(t, resource.TestCase{
		PreCheck: func() { accPreCheck(t) }, ProtoV6ProviderFactories: accFactories,
		Steps: []resource.TestStep{
			{Config: cfg("https://app.example.com/cb"), Check: resource.TestCheckResourceAttrSet("forgebase_oauth_client.c", "client_secret")},
			{Config: cfg("https://app.example.com/callback"), Check: resource.TestCheckResourceAttr("forgebase_oauth_client.c", "redirect_uris.#", "1")},
			{ResourceName: "forgebase_oauth_client.c", ImportState: true, ImportStateVerify: true, ImportStateVerifyIgnore: []string{"client_secret"}},
		},
	})
}

func TestAccRole_realm(t *testing.T) {
	key := accName("tfacc-role-")
	cfg := func(allow string) string {
		return accProvider + fmt.Sprintf(`
resource "forgebase_role" "r" {
  key   = %q
  allow = [%q]
}
`, key, allow)
	}
	resource.Test(t, resource.TestCase{
		PreCheck: func() { accPreCheck(t) }, ProtoV6ProviderFactories: accFactories,
		Steps: []resource.TestStep{
			{Config: cfg("collections:read")},
			{Config: cfg("collections:*"), Check: resource.TestCheckResourceAttr("forgebase_role.r", "allow.#", "1")},
			{ResourceName: "forgebase_role.r", ImportState: true, ImportStateVerify: true},
		},
	})
}

func TestAccPlanAndFeature(t *testing.T) {
	suffix := accName("")
	resource.Test(t, resource.TestCase{
		PreCheck: func() { accPreCheck(t) }, ProtoV6ProviderFactories: accFactories,
		Steps: []resource.TestStep{{
			Config: accProvider + fmt.Sprintf(`
resource "forgebase_feature" "f" {
  key  = "tfacc_feature_%[1]s"
  kind = "metered"
  unit = "requests"
}

resource "forgebase_plan" "p" {
  key        = "tfacc_plan_%[1]s"
  trial_days = 7
  features = [{ feature = forgebase_feature.f.key, limit = 1000 }]
}
`, suffix),
			Check: resource.TestCheckResourceAttr("forgebase_plan.p", "features.0.limit", "1000"),
		}},
	})
}
