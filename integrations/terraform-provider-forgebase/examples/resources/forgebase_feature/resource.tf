resource "forgebase_feature" "ai_generation" {
  key         = "ai_generation"
  name        = "AI generation"
  kind        = "metered"
  unit        = "requests"
  permissions = ["ai:*"]
}
