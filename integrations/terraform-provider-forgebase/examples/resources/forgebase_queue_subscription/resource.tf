resource "forgebase_queue_topic" "orders_dead" {
  name = "orders-dead"
}

# Pull subscription: consumers call the pull API.
resource "forgebase_queue_subscription" "worker" {
  name                  = "order-worker"
  topic                 = forgebase_queue_topic.orders.name
  ack_deadline_ms       = 30000
  max_delivery_attempts = 5
  dead_letter_topic     = forgebase_queue_topic.orders_dead.name
}

# Push subscription: the server calls a webhook. The signing secret is
# returned once by the server and kept (sensitive) in push_secret.
resource "forgebase_queue_subscription" "hook" {
  name  = "order-hook"
  topic = forgebase_queue_topic.orders.name

  push = {
    kind     = "webhook"
    endpoint = "https://example.com/hooks/orders"
  }
}

output "order_hook_secret" {
  value     = forgebase_queue_subscription.hook.push_secret
  sensitive = true
}
