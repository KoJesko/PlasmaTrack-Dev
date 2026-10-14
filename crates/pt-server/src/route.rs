//! Route GeoJSON (PUBLIC_API §4, ADMIN_API §5): stage 5, the map projection.
//! Until it ships, both endpoints answer `501 not_implemented`.

use serde_json::json;

use crate::env::Host;
use crate::http::{ApiError, ApiResult, Request, Response};
use crate::public::job_row;
use crate::auth;

fn not_yet() -> ApiError {
    ApiError::new(501, "not_implemented", "Route maps arrive in the next server update.").details(json!({"feature": "route"}))
}

pub async fn public<H: Host>(h: &H, req: &Request, id: &str) -> ApiResult<Response> {
    auth::service(h, req)?;
    job_row(h, id).await?;
    Err(not_yet())
}

pub async fn admin<H: Host>(h: &H, req: &Request, id: &str) -> ApiResult<Response> {
    auth::admin(h, req).await?;
    job_row(h, id).await?;
    Err(not_yet())
}
