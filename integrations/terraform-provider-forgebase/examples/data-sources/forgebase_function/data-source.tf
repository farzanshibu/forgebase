data "forgebase_function" "hello" {
  name = "hello"
}

output "hello_version" {
  value = data.forgebase_function.hello.active_version
}
