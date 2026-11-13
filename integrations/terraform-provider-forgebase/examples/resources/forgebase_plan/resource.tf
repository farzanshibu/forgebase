resource "forgebase_plan" "pro" {
  key        = "pro"
  name       = "Pro"
  trial_days = 14

  features = [
    {
      feature = forgebase_feature.ai_generation.key
      limit   = 10000
      period  = "month"
    },
  ]
}
