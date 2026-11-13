# A realm role (needs a super-admin credential).
resource "forgebase_role" "editor" {
  key         = "editor"
  name        = "Editor"
  description = "Edits content, cannot delete collections"

  allow = ["collections:read", "records:*"]
  deny  = ["collections:delete"]
}

# An organization role.
resource "forgebase_role" "billing" {
  key   = "billing"
  org   = forgebase_org.acme.id
  allow = ["billing:read"]
}
