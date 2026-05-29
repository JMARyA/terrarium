//! Unified API authentication.
//!
//! Accepts either an HTTP Basic credential (used by the Terraform HTTP backend
//! and `terra remote`) or a Bearer API token (minted self-service in the web
//! UI). Bearer is tried first; Basic is the fallback, so the Terraform backend
//! contract is unchanged.

use authur::extractor::{APIUser, BasicAuthUser};
use authur::{User, UserDB, vfs};
use axum::extract::{FromRef, FromRequestParts};
use axum::http::StatusCode;
use axum::http::request::Parts;

pub struct AuthUser(pub User);

impl<S> FromRequestParts<S> for AuthUser
where
    UserDB<vfs::PhysicalFS>: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = (StatusCode, &'static str);

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        if let Ok(APIUser(user)) = APIUser::from_request_parts(parts, state).await {
            return Ok(AuthUser(user));
        }
        let BasicAuthUser(user) = BasicAuthUser::from_request_parts(parts, state).await?;
        Ok(AuthUser(user))
    }
}
