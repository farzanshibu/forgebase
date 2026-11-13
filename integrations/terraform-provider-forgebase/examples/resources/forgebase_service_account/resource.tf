resource "forgebase_service_account" "ci" {
  name        = "ci-deploys"
  description = "Used by the deploy pipeline"
}
