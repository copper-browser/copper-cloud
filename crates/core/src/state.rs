//! Shared application state handed to every handler.

use std::sync::Arc;
use std::time::Instant;

use crate::access::AccessModeCache;
use crate::config::Config;
use crate::crypto::Crypto;
use crate::events::Events;

pub struct AppState {
    pub db: sqlx::PgPool,
    pub cfg: Arc<Config>,
    pub crypto: Crypto,
    pub events: Events,
    pub started_at: Instant,
    /// `server_settings.access_mode`, cached for 5 s (see [`crate::access::access_mode`]).
    pub access_mode: AccessModeCache,
    /// SSO HTTP client + OIDC discovery cache.
    pub sso: crate::sso::SsoCache,
    /// Device-agent relay: open device streams and agent calls in flight.
    pub device_agent: crate::device_agent::Hub,
}

pub type SharedState = Arc<AppState>;

impl AppState {
    /// Build the shared state: derives the key-encryption key from `cfg.master_key`.
    pub fn new(db: sqlx::PgPool, cfg: Config) -> SharedState {
        let crypto = Crypto::from_master_key(&cfg.master_key);
        let device_agent = crate::device_agent::Hub::new(
            cfg.device_agent.agent_per_minute,
            cfg.device_agent.online_window,
            cfg.device_agent.max_pending_per_link,
        );
        Arc::new(Self {
            db,
            cfg: Arc::new(cfg),
            crypto,
            events: Events::new(),
            started_at: Instant::now(),
            access_mode: AccessModeCache::default(),
            sso: crate::sso::SsoCache::default(),
            device_agent,
        })
    }
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("cfg", &self.cfg)
            .field("events", &self.events)
            .field("access_mode", &self.access_mode)
            .field("device_agent", &self.device_agent)
            .finish_non_exhaustive()
    }
}
