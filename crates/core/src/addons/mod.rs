//! Copper addons: small web bundles (HTML/JS/CSS + `manifest.json`) written for one *slot* of
//! the Copper window, uploaded by an admin and handed to the people the admin picks
//! (contract: `docs/addons.md`).
//!
//! * [`Slot`] — the closed list of replaceable regions (v1: `agent-sidebar`).
//! * [`manifest`] — `manifest.json` (`copper-addon/1`) parsing and validation, settings types.
//! * [`bundle`] — `.copperaddon` zip ingest (path rules, junk files, zip-bomb guards) and the
//!   canonical served document (`copper-addon-bundle/1`) whose SHA-256 identifies a version.
//! * [`store`] — Postgres: upload, update, delete, admin views, per-person resolution. Shared
//!   by the admin API (`/admin/api/addons`) and `copper-cloud addons …`; every write takes an
//!   [`Actor`] and writes an `admin_audit` row.
//! * `GET /v1/addons` and `GET /v1/addons/{id}/bundle/{sha256}` (device session auth) — see
//!   `routes.rs`.
//!
//! Admin-API writes publish [`Event::Addons`] to every connected user ([`notify`]); CLI writes
//! reach Coppers at their next check (launch, update check).

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::error::{ApiError, ErrorLog};
use crate::events::{Event, Events};

pub mod bundle;
pub mod manifest;
mod routes;
pub mod store;

pub(crate) use routes::routes;

pub use bundle::Built;
pub use manifest::Manifest;

/// A replaceable region of the Copper window. The list is closed and compiled into every
/// client and into the cloud; uploads for any other slot are refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Slot {
    /// The pane on the right of the page stage, toggled by ⌘E (Mac) / Ctrl+E (Linux, Windows).
    AgentSidebar,
}

impl Slot {
    /// Every slot this server knows, in display order.
    pub const ALL: &'static [Self] = &[Self::AgentSidebar];

    /// Wire id (`agent-sidebar`).
    pub fn id(self) -> &'static str {
        match self {
            Self::AgentSidebar => "agent-sidebar",
        }
    }

    /// Human name for the admin portal.
    pub fn name(self) -> &'static str {
        match self {
            Self::AgentSidebar => "Agent sidebar",
        }
    }

    /// One sentence on where the slot is and what it replaces.
    pub fn description(self) -> &'static str {
        match self {
            Self::AgentSidebar => {
                "The pane on the right of the page, toggled by \u{2318}E (Mac) or Ctrl+E \
                 (Linux, Windows). Replaces the built-in agent pane."
            }
        }
    }

    /// The slot with wire id `raw`, if this server knows it.
    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|s| s.id() == raw)
    }

    /// `{id, name, description}` (`GET /admin/api/addons/slots`).
    pub fn view(self) -> Value {
        json!({ "id": self.id(), "name": self.name(), "description": self.description() })
    }

    /// Comma-separated wire ids, for error messages.
    pub fn known_list() -> String {
        Self::ALL
            .iter()
            .map(|s| s.id())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

impl std::fmt::Display for Slot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.id())
    }
}

/// Every slot as `[{id, name, description}]`.
pub fn slots_view() -> Value {
    Value::Array(Slot::ALL.iter().map(|s| s.view()).collect())
}

/// Who made a change (audit `admin_id` / `detail.via`, `addons.updated_by`).
#[derive(Clone, Debug)]
pub enum Actor {
    Admin { id: Uuid, email: String },
    Cli,
}

impl Actor {
    /// `updated_by` value: the admin's email, or `cli`.
    pub fn label(&self) -> &str {
        match self {
            Self::Admin { email, .. } => email,
            Self::Cli => "cli",
        }
    }

    /// `admin_audit.admin_id` / `addon_versions.uploaded_by` (NULL for the CLI).
    pub fn admin_id(&self) -> Option<Uuid> {
        match self {
            Self::Admin { id, .. } => Some(*id),
            Self::Cli => None,
        }
    }

    /// `detail.via` in audit rows.
    pub fn via(&self) -> &'static str {
        match self {
            Self::Admin { .. } => "admin_api",
            Self::Cli => "cli",
        }
    }
}

/// Tell every connected Copper that addons changed (SSE `addons` on `/v1/sync/events`); they
/// refetch `GET /v1/addons` with their last revision.
pub fn notify(events: &Events) {
    events.publish_all(&Event::Addons);
}

/// Errors of addon writes: every [`ApiError`], plus the upload conflict
/// `409 {"error":"version_exists"}` (same version number, different bytes).
#[derive(Debug, thiserror::Error)]
pub enum AddonError {
    #[error(transparent)]
    Api(#[from] ApiError),
    #[error(
        "{id} {version} already exists with different contents (bump \"version\" in manifest.json)"
    )]
    VersionExists {
        id: String,
        version: String,
        /// SHA-256 of the stored bundle for that version.
        sha256: String,
    },
    /// An upload whose `Content-Type` is not a zip (415).
    #[error("send the .copperaddon file as the raw body with Content-Type: application/zip")]
    UnsupportedMediaType,
}

impl From<sqlx::Error> for AddonError {
    fn from(err: sqlx::Error) -> Self {
        Self::Api(err.into())
    }
}

impl From<tokio::task::JoinError> for AddonError {
    fn from(err: tokio::task::JoinError) -> Self {
        Self::Api(err.into())
    }
}

impl AddonError {
    /// HTTP status of the response.
    pub fn status(&self) -> StatusCode {
        match self {
            Self::Api(e) => e.status(),
            Self::VersionExists { .. } => StatusCode::CONFLICT,
            Self::UnsupportedMediaType => StatusCode::UNSUPPORTED_MEDIA_TYPE,
        }
    }
}

impl IntoResponse for AddonError {
    fn into_response(self) -> Response {
        match self {
            Self::Api(e) => e.into_response(),
            Self::VersionExists {
                ref id,
                ref version,
                ref sha256,
            } => {
                let body = json!({
                    "error": "version_exists",
                    "message": self.to_string(),
                    "id": id,
                    "version": version,
                    "sha256": sha256,
                });
                let mut resp = (StatusCode::CONFLICT, axum::Json(body)).into_response();
                resp.extensions_mut().insert(ErrorLog {
                    code: "version_exists",
                    message: None,
                });
                resp
            }
            Self::UnsupportedMediaType => {
                let body = json!({
                    "error": "unsupported_media_type",
                    "message": self.to_string(),
                });
                let mut resp =
                    (StatusCode::UNSUPPORTED_MEDIA_TYPE, axum::Json(body)).into_response();
                resp.extensions_mut().insert(ErrorLog {
                    code: "unsupported_media_type",
                    message: None,
                });
                resp
            }
        }
    }
}

pub(crate) fn rfc3339(t: time::OffsetDateTime) -> String {
    t.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_are_a_closed_list() {
        assert_eq!(Slot::parse("agent-sidebar"), Some(Slot::AgentSidebar));
        assert_eq!(Slot::parse("Agent-Sidebar"), None);
        assert_eq!(Slot::parse("toolbar"), None);
        assert_eq!(
            slots_view(),
            json!([{ "id": "agent-sidebar", "name": "Agent sidebar", "description": Slot::AgentSidebar.description() }])
        );
        // Slot id grammar: ^[a-z][a-z0-9-]{1,40}$
        for s in Slot::ALL {
            let id = s.id();
            assert!(id.len() >= 2 && id.len() <= 41, "{id}");
            assert!(id.as_bytes()[0].is_ascii_lowercase(), "{id}");
            assert!(
                id.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
                "{id}"
            );
        }
    }

    #[test]
    fn version_exists_renders_its_own_code() {
        let resp = AddonError::VersionExists {
            id: "com.example.hello".into(),
            version: "1.0.0".into(),
            sha256: "ab".repeat(32),
        }
        .into_response();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        assert_eq!(
            resp.extensions().get::<ErrorLog>().unwrap().code,
            "version_exists"
        );
        let resp = AddonError::from(ApiError::NotFound).into_response();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
