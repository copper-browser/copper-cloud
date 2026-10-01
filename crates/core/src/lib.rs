//! Copper Cloud core: config, db, crypto, auth, sync docs/history/SSE, TLS, observability.
//!
//! The binary composes [`app`] from [`router`] (auth + devices + sync) plus the canvas
//! crate's router, behind the instance-key gate ([`instance_gate`]).

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
pub mod shutdown;
pub mod state;
pub mod sync;
pub mod tls;

pub use app::{app, instance_gate, router};
