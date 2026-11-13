# A small backend: a collection with rules and an index, a bucket for uploads,
# a queue with a worker subscription, a function, and a service account whose
# API key the deploy pipeline uses afterwards.
#
#   export FORGEBASE_URL=http://127.0.0.1:8090
#   export FORGEBASE_TOKEN=<administrator API key>
#   terraform init && terraform apply

terraform {
  required_providers {
    forgebase = { source = "forgebase/forgebase" }
  }
}

provider "forgebase" {}

resource "forgebase_collection" "posts" {
  name = "posts"
  fields = [
    { name = "title", type = "text", required = true, options = jsonencode({ max_length = 200 }) },
    { name = "status", type = "select", options = jsonencode({ values = ["draft", "live"] }), default = jsonencode("draft") },
    { name = "cover", type = "text" },
  ]
}

resource "forgebase_index" "posts_status" {
  collection = forgebase_collection.posts.name
  keys       = [{ path = "status" }]
}

resource "forgebase_collection_rules" "posts" {
  collection  = forgebase_collection.posts.name
  list_rule   = ""
  view_rule   = ""
  create_rule = "@request.auth.id != \"\""
}

resource "forgebase_storage_bucket" "uploads" {
  name                  = "uploads"
  visibility            = "private"
  allowed_content_types = ["image/png", "image/jpeg"]
}

resource "forgebase_storage_bucket_acl" "uploads" {
  bucket  = forgebase_storage_bucket.uploads.name
  entries = [{ grantee = "authenticated", permission = "write", prefix = "incoming/" }]
}

resource "forgebase_queue_topic" "post_events" {
  name = "post-events"
  source = {
    collection = forgebase_collection.posts.name
  }
}

resource "forgebase_queue_subscription" "indexer" {
  name                  = "post-indexer"
  topic                 = forgebase_queue_topic.post_events.name
  max_delivery_attempts = 5
}

resource "forgebase_function" "hello" {
  name         = "hello"
  language     = "javascript"
  capabilities = ["log"]
  source       = <<-JS
    import { serve } from "forgebase";
    serve(() => new Response("hello"));
  JS
}

resource "forgebase_service_account" "pipeline" {
  name = "deploy-pipeline"
}

resource "forgebase_api_key" "pipeline" {
  service_account_id = forgebase_service_account.pipeline.id
  name               = "pipeline"
  scopes             = ["collections:write", "storage:write", "functions:write"]
  expires_in_days    = 90
}

output "pipeline_key" {
  value     = forgebase_api_key.pipeline.key
  sensitive = true
}
