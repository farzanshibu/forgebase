resource "forgebase_api_key" "deploy" {
  service_account_id = forgebase_service_account.ci.id
  name               = "deploy"
  scopes             = ["collections:write", "storage:write", "functions:write"]
  expires_in_days    = 90
}

# The secret is only known when Terraform created the key.
output "deploy_key" {
  value     = forgebase_api_key.deploy.key
  sensitive = true
}
