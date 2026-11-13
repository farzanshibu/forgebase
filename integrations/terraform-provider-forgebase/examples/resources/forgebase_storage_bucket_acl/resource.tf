resource "forgebase_storage_bucket_acl" "avatars" {
  bucket = forgebase_storage_bucket.avatars.name

  entries = [
    { grantee = "authenticated", permission = "write", prefix = "uploads/" },
    { grantee = "role:moderators", permission = "admin" },
  ]
}
