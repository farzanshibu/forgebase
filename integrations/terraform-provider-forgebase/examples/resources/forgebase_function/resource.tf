resource "forgebase_function" "hello" {
  name         = "hello"
  language     = "typescript"
  source_file  = "${path.module}/functions/hello.ts" # content changes are detected by hash
  description  = "Says hello"
  access       = "authenticated"
  capabilities = ["log", "env", "secrets"]

  env = {
    GREETING = "hello"
  }

  # Sensitive. The server never returns values, so only the presence of each
  # name is checked for drift; change a value to rotate it.
  secrets = {
    API_TOKEN = var.api_token
  }

  limits = {
    timeout_ms = 2000
  }
}

# A WebAssembly module, hooked to record changes.
resource "forgebase_function" "audit" {
  name         = "audit"
  wasm_file    = "${path.module}/functions/audit.wasm"
  capabilities = ["db.write", "log"]

  hooks = [
    { collection = "posts", events = ["after_create", "after_update"] },
  ]
}

variable "api_token" {
  type      = string
  sensitive = true
}
