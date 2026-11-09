//! The request principal.
//!
//! Handlers never authenticate anyone. They read the
//! [`AccessContext`] that an earlier layer stored in the request extensions:
//!
//! * the auth layer (FB-20, mounted by the server) fills it from a session,
//!   token or API key;
//! * until it is mounted, [`install_anonymous`] stores
//!   [`AccessContext::anonymous`], so the [`DefaultAuthorizer`](forgebase_data::DefaultAuthorizer)
//!   denies everything (`401`);
//! * tests install an administrator or a user with their own layer.
//!
//! Request extensions cannot be set by a client, only by code in this process,
//! so the context cannot be forged over HTTP. A request with no context at
//! all is treated as anonymous (fail closed).
//!
//! Collection existence is not leaked: the data core answers a caller who is
//! not privileged the same denial for a missing collection as for a
//! forbidden one (FB-21; this replaced FB-11's `hide_collection_existence`
//! response rewrite).

use axum::extract::{FromRequestParts, Request};
use axum::http::request::Parts;
use axum::middleware::Next;
use axum::response::Response;
use forgebase_data::AccessContext;

use crate::context::RequestId;
use crate::error::ApiError;

/// Stores an anonymous [`AccessContext`] unless an earlier layer already
/// stored one. Layers that authenticate must run *before* (outside) this one.
pub async fn install_anonymous(mut req: Request, next: Next) -> Response {
    if req.extensions().get::<AccessContext>().is_none() {
        req.extensions_mut().insert(AccessContext::anonymous());
    }
    next.run(req).await
}

/// Extractor for the request's [`AccessContext`], with the request id filled in.
#[derive(Debug, Clone)]
pub struct Access(pub AccessContext);

impl<S: Send + Sync> FromRequestParts<S> for Access {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let mut ctx = parts
            .extensions
            .get::<AccessContext>()
            .cloned()
            .unwrap_or_else(AccessContext::anonymous);
        if ctx.request_id.is_none()
            && let Some(id) = parts.extensions.get::<RequestId>()
        {
            ctx.request_id = Some(id.as_str().to_owned());
        }
        Ok(Access(ctx))
    }
}
