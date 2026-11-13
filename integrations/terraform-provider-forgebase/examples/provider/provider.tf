terraform {
  required_providers {
    forgebase = {
      source = "forgebase/forgebase"
    }
  }
}

# url and token can also come from FORGEBASE_URL and FORGEBASE_TOKEN.
provider "forgebase" {
  url   = "https://forgebase.example.com"
  token = var.forgebase_token # a service-account API key (fbk_…) or a personal access token
}

variable "forgebase_token" {
  type      = string
  sensitive = true
}
