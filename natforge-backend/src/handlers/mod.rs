pub mod admin;
pub mod auth;
pub mod devices;
pub mod internal;
pub mod tunnels;
pub mod user;

use axum::http::StatusCode;
use std::fmt::Display;

/// Map any error to a 500 response carrying its text. Used by handlers for
/// infrastructure failures that have no more specific status.
pub(crate) fn err<E: Display>(e: E) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

/// Like [`err`], but prefixes "database error: " (for DB/query failures).
pub(crate) fn db_err<E: Display>(e: E) -> (StatusCode, String) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("database error: {e}"),
    )
}
