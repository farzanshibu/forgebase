resource "forgebase_index" "posts_status" {
  name       = "idx_posts_status_published"
  collection = forgebase_collection.posts.name

  keys = [
    { path = "status" },
    { path = "published_at", desc = true },
  ]
}

resource "forgebase_index" "posts_search" {
  collection = forgebase_collection.posts.name
  kind       = "fulltext"
  keys       = [{ path = "title" }]
}
