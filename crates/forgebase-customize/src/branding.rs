//! White-label branding: name, colours, sign-in text, logo and favicon per
//! realm, and the public view dashboards and hosted pages read.
//!
//! Everything a realm admin enters is data, never markup: text fields are
//! plain text (the pages that show them escape them), colours must be hex
//! (`#rgb` / `#rrggbb`) so they are safe inside CSS, and uploaded images are
//! sniffed (PNG, JPEG, GIF, WebP, ICO, SVG) and served with `nosniff` and a
//! sandboxing Content-Security-Policy, so an SVG cannot run script.

use forgebase_data::AccessContext;
use forgebase_db::Value;
use sha2::{Digest, Sha256};

use crate::error::{CustomizeError, Result};
use crate::mail::valid_address;
use crate::model::{Asset, AssetKind, Branding, BrandingInput, PublicBranding};
use crate::service::{CustomizeService, opt_int, opt_text, text};

/// Largest uploaded image.
pub const MAX_ASSET_BYTES: usize = 256 * 1024;

fn plain(name: &str, v: Option<String>, max: usize) -> Result<Option<String>> {
    let Some(v) = v.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    if v.chars().count() > max {
        return Err(CustomizeError::invalid(
            "invalid_branding",
            format!("{name} is longer than {max} characters"),
        ));
    }
    if v.chars().any(|c| c.is_control() && c != '\n') {
        return Err(CustomizeError::invalid(
            "invalid_branding",
            format!("{name} contains control characters"),
        ));
    }
    Ok(Some(v))
}

fn color(name: &str, v: Option<String>) -> Result<Option<String>> {
    let Some(v) = v.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let ok = v.starts_with('#') && matches!(v.len(), 4 | 7) && v[1..].chars().all(|c| c.is_ascii_hexdigit());
    if !ok {
        return Err(CustomizeError::invalid(
            "invalid_branding",
            format!("{name} must be a hex colour such as #1a2b3c or #abc"),
        ));
    }
    Ok(Some(v.to_ascii_lowercase()))
}

/// The image type of `bytes` when it matches `content_type`.
fn sniff(content_type: &str, bytes: &[u8]) -> Option<&'static str> {
    let ct = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let ok = match ct.as_str() {
        "image/png" => bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]),
        "image/jpeg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
        "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "image/webp" => bytes.len() > 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP",
        "image/x-icon" | "image/vnd.microsoft.icon" => bytes.starts_with(&[0, 0, 1, 0]),
        "image/svg+xml" => {
            let head = String::from_utf8_lossy(&bytes[..bytes.len().min(2048)]).to_ascii_lowercase();
            std::str::from_utf8(bytes).is_ok() && head.trim_start().starts_with(['<']) && head.contains("<svg")
        }
        _ => false,
    };
    ok.then_some(match ct.as_str() {
        "image/png" => "image/png",
        "image/jpeg" => "image/jpeg",
        "image/gif" => "image/gif",
        "image/webp" => "image/webp",
        "image/svg+xml" => "image/svg+xml",
        _ => "image/x-icon",
    })
}

impl CustomizeService {
    /// The branding of a realm (empty fields when none was saved).
    pub async fn branding(&self, ctx: &AccessContext, realm: Option<&str>) -> Result<Branding> {
        let realm = self.realm_for(ctx, realm)?;
        self.can_read(ctx, &realm)?;
        self.branding_of(&realm).await
    }

    pub(crate) async fn branding_of(&self, realm: &str) -> Result<Branding> {
        let rows = self
            .query(
                "SELECT display_name, primary_color, background_color, text_color, accent_color, signin_title,
                        signin_subtitle, signin_footer, support_email, updated_at
                 FROM _forgebase_branding WHERE realm = ?1",
                vec![realm.into()],
            )
            .await?;
        let assets = self
            .query(
                "SELECT kind FROM _forgebase_branding_assets WHERE realm = ?1",
                vec![realm.into()],
            )
            .await?;
        let has = |k: &str| assets.rows.iter().any(|r| text(r, 0) == k);
        let mut b = Branding {
            realm: realm.to_owned(),
            has_logo: has("logo"),
            has_favicon: has("favicon"),
            ..Branding::default()
        };
        if let Some(r) = rows.rows.first() {
            b.display_name = opt_text(r, 0);
            b.primary_color = opt_text(r, 1);
            b.background_color = opt_text(r, 2);
            b.text_color = opt_text(r, 3);
            b.accent_color = opt_text(r, 4);
            b.signin_title = opt_text(r, 5);
            b.signin_subtitle = opt_text(r, 6);
            b.signin_footer = opt_text(r, 7);
            b.support_email = opt_text(r, 8);
            b.updated_at = opt_int(r, 9);
        }
        Ok(b)
    }

    /// Replaces the branding of a realm; fields left out are cleared.
    pub async fn put_branding(
        &self,
        ctx: &AccessContext,
        realm: Option<&str>,
        input: BrandingInput,
    ) -> Result<Branding> {
        let realm = self.realm_for(ctx, realm)?;
        self.can_write(ctx, &realm)?;
        self.require_realm(&realm).await?;
        let support = plain("support_email", input.support_email, 254)?;
        if let Some(s) = &support
            && !valid_address(s)
        {
            return Err(CustomizeError::invalid(
                "invalid_branding",
                "support_email is not a valid address",
            ));
        }
        let params: Vec<Value> = vec![
            realm.clone().into(),
            plain("display_name", input.display_name, 80)?.into(),
            color("primary_color", input.primary_color)?.into(),
            color("background_color", input.background_color)?.into(),
            color("text_color", input.text_color)?.into(),
            color("accent_color", input.accent_color)?.into(),
            plain("signin_title", input.signin_title, 120)?.into(),
            plain("signin_subtitle", input.signin_subtitle, 300)?.into(),
            plain("signin_footer", input.signin_footer, 300)?.into(),
            support.into(),
            self.now().into(),
            ctx.principal_key().into(),
        ];
        let mut tx = self.begin().await?;
        tx.exec(
            "INSERT INTO _forgebase_branding (realm, display_name, primary_color, background_color, text_color,
                 accent_color, signin_title, signin_subtitle, signin_footer, support_email, updated_at, updated_by)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT (realm) DO UPDATE SET display_name = excluded.display_name,
                 primary_color = excluded.primary_color, background_color = excluded.background_color,
                 text_color = excluded.text_color, accent_color = excluded.accent_color,
                 signin_title = excluded.signin_title, signin_subtitle = excluded.signin_subtitle,
                 signin_footer = excluded.signin_footer, support_email = excluded.support_email,
                 updated_at = excluded.updated_at, updated_by = excluded.updated_by",
            params,
        )
        .await?;
        tx.commit().await?;
        self.branding_of(&realm).await
    }

    /// Stores the logo or favicon of a realm.
    pub async fn put_asset(
        &self,
        ctx: &AccessContext,
        realm: Option<&str>,
        kind: AssetKind,
        content_type: &str,
        bytes: &[u8],
    ) -> Result<Branding> {
        let realm = self.realm_for(ctx, realm)?;
        self.can_write(ctx, &realm)?;
        self.require_realm(&realm).await?;
        if bytes.is_empty() || bytes.len() > MAX_ASSET_BYTES {
            return Err(CustomizeError::invalid(
                "invalid_asset",
                format!("an image is 1 byte to {} KiB", MAX_ASSET_BYTES / 1024),
            ));
        }
        let Some(ct) = sniff(content_type, bytes) else {
            return Err(CustomizeError::invalid(
                "invalid_asset",
                "the content does not match its Content-Type (png, jpeg, gif, webp, x-icon, svg+xml)",
            ));
        };
        let etag = Sha256::digest(bytes)
            .iter()
            .take(12)
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let mut tx = self.begin().await?;
        tx.exec(
            "INSERT INTO _forgebase_branding_assets (realm, kind, content_type, bytes, etag, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (realm, kind) DO UPDATE SET content_type = excluded.content_type, bytes = excluded.bytes,
                 etag = excluded.etag, updated_at = excluded.updated_at",
            vec![
                realm.clone().into(),
                kind.as_str().into(),
                ct.into(),
                Value::Blob(bytes.to_vec()),
                etag.into(),
                self.now().into(),
            ],
        )
        .await?;
        tx.commit().await?;
        self.branding_of(&realm).await
    }

    /// Removes the logo or favicon.
    pub async fn delete_asset(&self, ctx: &AccessContext, realm: Option<&str>, kind: AssetKind) -> Result<Branding> {
        let realm = self.realm_for(ctx, realm)?;
        self.can_write(ctx, &realm)?;
        let mut tx = self.begin().await?;
        tx.exec(
            "DELETE FROM _forgebase_branding_assets WHERE realm = ?1 AND kind = ?2",
            vec![realm.clone().into(), kind.as_str().into()],
        )
        .await?;
        tx.commit().await?;
        self.branding_of(&realm).await
    }

    /// An uploaded image (public).
    pub async fn asset(&self, realm: &str, kind: AssetKind) -> Result<Asset> {
        let rows = self
            .query(
                "SELECT content_type, bytes, etag FROM _forgebase_branding_assets WHERE realm = ?1 AND kind = ?2",
                vec![realm.into(), kind.as_str().into()],
            )
            .await?;
        let r = rows
            .rows
            .first()
            .ok_or_else(|| CustomizeError::NotFound(kind.as_str().into()))?;
        let bytes = match r.get(1) {
            Some(Value::Blob(b)) => b.clone(),
            _ => Vec::new(),
        };
        Ok(Asset {
            content_type: text(r, 0),
            bytes,
            etag: text(r, 2),
        })
    }

    /// What dashboards and hosted pages show for a realm (public).
    pub async fn public_branding(&self, realm: &str) -> Result<PublicBranding> {
        self.require_realm(realm).await?;
        let b = self.branding_of(realm).await?;
        let base = "/api/v1/customize/public/branding";
        let domains = self
            .query(
                "SELECT host FROM _forgebase_domains WHERE realm = ?1 AND status = 'verified' ORDER BY host",
                vec![realm.into()],
            )
            .await?;
        Ok(PublicBranding {
            realm: realm.to_owned(),
            display_name: b.display_name.clone().unwrap_or_else(|| realm.to_owned()),
            primary_color: b.primary_color,
            background_color: b.background_color,
            text_color: b.text_color,
            accent_color: b.accent_color,
            signin_title: b.signin_title,
            signin_subtitle: b.signin_subtitle,
            signin_footer: b.signin_footer,
            support_email: b.support_email,
            logo_url: b.has_logo.then(|| format!("{base}/logo")),
            favicon_url: b.has_favicon.then(|| format!("{base}/favicon")),
            domains: domains.rows.iter().map(|r| text(r, 0)).collect(),
        })
    }
}

/// CSS custom properties for a realm's colours (values are validated hex).
pub fn theme_css(b: &PublicBranding) -> String {
    let mut css = String::from(":root{");
    for (name, v) in [
        ("--fb-primary", &b.primary_color),
        ("--fb-background", &b.background_color),
        ("--fb-text", &b.text_color),
        ("--fb-accent", &b.accent_color),
    ] {
        if let Some(v) = v
            .as_deref()
            .filter(|v| v.starts_with('#') && v[1..].chars().all(|c| c.is_ascii_hexdigit()))
        {
            css.push_str(&format!("{name}:{v};"));
        }
    }
    css.push('}');
    css
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colour_validation() {
        assert_eq!(color("c", Some("#ABC".into())).unwrap_or_default(), Some("#abc".into()));
        for bad in ["red", "#12", "#12345", "#gggggg", "#fff;}body{display:none", "url(x)"] {
            assert!(color("c", Some(bad.into())).is_err(), "{bad}");
        }
    }

    #[test]
    fn sniffing_rejects_mismatched_content() {
        let png = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0];
        assert!(sniff("image/png", &png).is_some());
        assert!(sniff("image/jpeg", &png).is_none());
        assert!(sniff("text/html", b"<html>").is_none());
        assert!(sniff("image/svg+xml", b"<svg xmlns='http://www.w3.org/2000/svg'/>").is_some());
        assert!(sniff("image/svg+xml", b"hello").is_none());
    }
}
