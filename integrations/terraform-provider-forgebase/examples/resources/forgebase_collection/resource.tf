resource "forgebase_collection" "posts" {
  name = "posts"

  fields = [
    {
      name     = "title"
      type     = "text"
      required = true
      options  = jsonencode({ max_length = 200 })
    },
    {
      name    = "status"
      type    = "select"
      options = jsonencode({ values = ["draft", "live"] })
      default = jsonencode("draft")
    },
    {
      name = "published_at"
      type = "date"
    },
  ]
}

# A read-only view over a SQL query.
resource "forgebase_collection" "live_posts" {
  name       = "live_posts"
  type       = "view"
  view_query = "SELECT id, title FROM posts WHERE status = 'live'"

  depends_on = [forgebase_collection.posts]
}
