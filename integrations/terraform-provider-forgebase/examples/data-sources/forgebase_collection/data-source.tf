data "forgebase_collection" "users" {
  name = "users"
}

output "user_fields" {
  value = [for f in data.forgebase_collection.users.fields : f.name]
}
