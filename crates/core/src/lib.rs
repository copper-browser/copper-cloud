//! Copper Cloud core: config, db, crypto, auth, sync docs/history/SSE, TLS, observability.
//!
//! The binary composes [`app`] from [`router`] (auth + devices + sync) plus the canvas
//! crate's router, behind the instance gate ([`instance_gate`]: instance key in `open` mode or
//! a per-person access key, see [`access`]).

pub mod access;
pub mod app;
pub mod auth;
pub mod config;
pub mod crypto;
pub mod db;
pub mod error;
pub mod events;
pub mod extract;
pub mod ids;
pub mod link;
pub mod observe;
pub mod pairing;
pub mod shutdown;
pub mod state;
pub mod sync;
pub mod tls;

pub use app::{app, app_with, instance_gate, router};
