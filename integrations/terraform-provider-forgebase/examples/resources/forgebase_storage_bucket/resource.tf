resource "forgebase_storage_bucket" "avatars" {
  name                  = "avatars"
  visibility            = "public"
  versioning            = true
  max_object_bytes      = 5 * 1024 * 1024
  allowed_content_types = ["image/png", "image/jpeg", "image/webp"]
}
