//! ForgeBase product customization (FB-74, ADR-0020).
//!
//! | Piece | Where |
//! |---|---|
//! | Custom domains: claim, DNS TXT verification, pluggable resolver | [`domains`], [`dns`] |
//! | Host-header routing to the realm (outermost middleware) | [`host`] |
//! | White-label branding: name, colours, sign-in text, logo, favicon | [`branding`] |
//! | Email templates per realm, rendering with escaping, preview | [`render`], [`email`] |
//! | SMTP and log mail providers | [`mail`] |
//! | Resend, Postmark, SendGrid, Mailgun and Amazon SES transports; sealed per-realm provider config | [`transports`], [`mailconfig`] |
//! | Sending through the job queue with retries, delivery log | [`email`], [`jobs`] |
//! | Versioned legal agreements, acceptance records, re-accept gate | [`agreements`], [`gate`] |
//! | REST under `/api/v1/customize`, public views, branded API docs page | [`http`] |
//!
//! Everything is scoped by realm (the top isolation level of ADR-0008).
//! Administration needs a privileged caller or the realm-level permissions
//! `customization:write` / `customization:read`. TLS for custom domains is
//! left to the reverse proxy in front of ForgeBase (see ADR-0020).

pub mod agreements;
pub mod branding;
pub mod dns;
pub mod domains;
pub mod email;
mod error;
pub mod gate;
pub mod host;
pub mod http;
pub mod jobs;
pub mod mail;
pub mod mailconfig;
pub mod model;
pub mod render;
mod service;
pub mod transports;

pub use agreements::{AcceptSource, AcceptanceQuery};
pub use dns::{DnsError, DnsResolver, DohResolver, StaticResolver};
pub use email::{BUILTIN_KEYS, DeliveryQuery};
pub use error::{CustomizeError, Result};
pub use gate::AgreementGate;
pub use host::{ResolvedHost, route as host_router};
pub use http::{extension, openapi, router};
pub use jobs::register_jobs;
pub use mail::{LogProvider, MailProvider, OutMail, SendError, SendReceipt, SmtpConfig, SmtpProvider, SmtpTls};
pub use mailconfig::{MailConfig, TestMailResult};
pub use model::*;
pub use service::{CustomizeConfig, CustomizeService, EMAIL_JOB, READ_PERMISSION, WRITE_PERMISSION};
