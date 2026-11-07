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

use axum::extract::{FromRequestParts, Request};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use forgebase_data::{AccessContext, Principal};

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

/// Stops callers who are not administrators from probing which collections
/// exist: the data core looks a collection up *before* it asks the authorizer,
/// so `collection_not_found` would answer `404` for a typo but `401`/`403` for a
/// real collection. For such callers it becomes the same `401` (anonymous) or
/// `403` (authenticated) as an existing, forbidden collection. Administrators
/// and the system keep the honest `404`.
pub async fn hide_collection_existence(req: Request, next: Next) -> Response {
    let ctx = req.extensions().get::<AccessContext>().cloned();
    let res = next.run(req).await;
    let privileged = ctx.as_ref().is_some_and(AccessContext::is_privileged);
    if !privileged
        && res
            .extensions()
            .get::<ApiError>()
            .is_some_and(|e| e.code() == "collection_not_found")
    {
        let anonymous = ctx.is_none_or(|c| c.principal == Principal::Anonymous);
        return if anonymous {
            ApiError::new(StatusCode::UNAUTHORIZED, "unauthenticated", "authentication required")
        } else {
            ApiError::new(StatusCode::FORBIDDEN, "forbidden", "not allowed")
        }
        .into_response();
    }
    res
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
