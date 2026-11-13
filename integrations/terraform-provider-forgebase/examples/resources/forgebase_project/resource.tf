resource "forgebase_project" "web" {
  org  = forgebase_org.acme.id
  slug = "web"
  name = "Web"
}

# Each environment is a tenant: use its id in the provider's tenant argument.
output "production_tenant" {
  value = one([for e in forgebase_project.web.environments : e.id if e.name == "production"])
}
