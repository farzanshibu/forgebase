# null locks a rule, "" opens it, any other value is a filter expression.
resource "forgebase_collection_rules" "posts" {
  collection = forgebase_collection.posts.name

  list_rule   = ""                           # anyone may list
  view_rule   = ""                           # anyone may read one
  create_rule = "@request.auth.id != \"\""   # signed-in users may create
  update_rule = "@request.auth.id = author"  # authors may update
  # delete_rule omitted: locked, only administrators may delete

  field_rules = {
    internal_notes = {
      read  = "@request.auth.role = \"staff\""
      write = "@request.auth.role = \"staff\""
    }
  }
}
