resource "forgebase_queue_topic" "orders" {
  name         = "orders"
  retention_ms = 7 * 24 * 60 * 60 * 1000

  labels = {
    team = "billing"
  }

  schema = jsonencode({
    type     = "object"
    required = ["order_id"]
  })
}

# A topic fed by record changes of a collection.
resource "forgebase_queue_topic" "post_changes" {
  name = "post-changes"

  source = {
    collection = forgebase_collection.posts.name
    ops        = ["create", "update"]
  }
}
