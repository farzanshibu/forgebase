resource "forgebase_oauth_client" "web" {
  name          = "Web app"
  description   = "The customer web app"
  redirect_uris = ["https://app.example.com/auth/callback"]
  scopes        = ["openid", "profile", "email", "offline_access"]

  access_token_ttl = 900
}

resource "forgebase_oauth_client" "spa" {
  name          = "Single-page app"
  type          = "public" # no secret; PKCE required
  redirect_uris = ["https://spa.example.com/"]
}

output "web_client_secret" {
  value     = forgebase_oauth_client.web.client_secret
  sensitive = true
}
