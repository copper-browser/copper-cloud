//! Configuration: a TOML file (default `/etc/copper-cloud/copper-cloud.toml`) overlaid with
//! environment variables `COPPER_CLOUD_<KEY>`; nested keys use `__`
//! (`COPPER_CLOUD_TLS__MODE=acme`, `COPPER_CLOUD_LIMITS__MAX_BLOB_BYTES=4000000`).

use std::fmt::{self, Write as _};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context as _};
use serde::{Deserialize, Serialize};

pub const DEFAULT_CONFIG_PATH: &str = "/etc/copper-cloud/copper-cloud.toml";
pub const ENV_PREFIX: &str = "COPPER_CLOUD_";
pub const DEFAULT_METRICS_BIND: &str = "127.0.0.1:9464";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TlsMode {
    /// rcgen self-signed certificate; clients pin its SHA-256 fingerprint (link code `fp=`).
    #[default]
    SelfSigned,
    /// Let's Encrypt via TLS-ALPN-01 (rustls-acme) for `tls.domain`. Needs :443 reachable.
    Acme,
    /// Plain HTTP. Only behind a TLS-terminating reverse proxy.
    Off,
}

impl fmt::Display for TlsMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::SelfSigned => "self-signed",
            Self::Acme => "acme",
            Self::Off => "off",
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    #[default]
    Json,
    Pretty,
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct TlsConfig {
    pub mode: TlsMode,
    pub cert_path: PathBuf,
    pub key_path: PathBuf,
    /// Required for `acme`; also used as the certificate SAN / public host when set.
    pub domain: Option<String>,
    pub acme_email: Option<String>,
    pub acme_cache_dir: PathBuf,
    /// Use the Let's Encrypt staging directory (testing).
    pub acme_staging: bool,
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Limits {
    /// Max decoded size of one sync doc payload.
    pub max_blob_bytes: usize,
    /// Max history entries per `POST /v1/sync/history` (and max `limit` on GET).
    pub max_history_batch: usize,
    /// Max serialized size of one history entry.
    pub max_history_entry_bytes: usize,
    /// Requests per minute per client IP on `/v1/auth/*`.
    pub auth_per_minute: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_blob_bytes: 8_000_000,
            max_history_batch: 2000,
            max_history_entry_bytes: 16 * 1024,
            auth_per_minute: 10,
        }
    }
}

/// Fully validated runtime configuration.
#[derive(Clone)]
#[non_exhaustive]
pub struct Config {
    pub listen: SocketAddr,
    /// Normalized `host[:port]` (no scheme, no path) that clients use; shown in the link code.
    pub public_url: String,
    pub database_url: String,
    pub db_max_connections: u32,
    /// Shared instance secret every `/v1` request must present in `X-Copper-Instance`.
    pub instance_key: String,
    pub master_key: [u8; 32],
    /// Allow self-service signup after the first user (the first user can always sign up).
    /// The admin CLI can override this at runtime (`admin disable-signup|enable-signup`).
    pub allow_signup: bool,
    /// Honor `X-Forwarded-For` for client IPs (rate limiting, logs). Only behind a proxy.
    pub trust_proxy: bool,
    pub tls: TlsConfig,
    pub limits: Limits,
    /// Prometheus `/metrics` listener; `None` disables it.
    pub metrics_bind: Option<SocketAddr>,
    pub log_format: LogFormat,
    /// `tracing` env-filter directive (overridden by `RUST_LOG` when set).
    pub log_level: String,
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("listen", &self.listen)
            .field("public_url", &self.public_url)
            .field("database_url", &redact_database_url(&self.database_url))
            .field("db_max_connections", &self.db_max_connections)
            .field("instance_key", &"<redacted>")
            .field("master_key", &"<redacted>")
            .field("allow_signup", &self.allow_signup)
            .field("trust_proxy", &self.trust_proxy)
            .field("tls", &self.tls)
            .field("limits", &self.limits)
            .field("metrics_bind", &self.metrics_bind)
            .field("log_format", &self.log_format)
            .field("log_level", &self.log_level)
            .finish()
    }
}

/// `postgres://user:secret@host:5432/db?sslmode=require` → `postgres://user:***@host:5432/db`.
pub fn redact_database_url(url: &str) -> String {
    let no_query = url.split('?').next().unwrap_or_default();
    let Some((scheme, rest)) = no_query.split_once("://") else {
        return "<redacted>".into();
    };
    match rest.rsplit_once('@') {
        Some((userinfo, host)) => {
            let user = userinfo.split(':').next().unwrap_or_default();
            if userinfo.contains(':') {
                format!("{scheme}://{user}:***@{host}")
            } else {
                format!("{scheme}://{user}@{host}")
            }
        }
        None => format!("{scheme}://{rest}"),
    }
}

// ---------------------------------------------------------------------------------------------
// Raw (file + env) representation

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    listen: Option<String>,
    public_url: Option<String>,
    database_url: Option<String>,
    db_max_connections: Option<u32>,
    instance_key: Option<String>,
    master_key: Option<String>,
    allow_signup: Option<bool>,
    trust_proxy: Option<bool>,
    metrics_bind: Option<String>,
    log_format: Option<LogFormat>,
    log_level: Option<String>,
    #[serde(default)]
    tls: RawTls,
    #[serde(default)]
    limits: RawLimits,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTls {
    mode: Option<TlsMode>,
    cert_path: Option<PathBuf>,
    key_path: Option<PathBuf>,
    domain: Option<String>,
    acme_email: Option<String>,
    acme_cache_dir: Option<PathBuf>,
    acme_staging: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLimits {
    max_blob_bytes: Option<usize>,
    max_history_batch: Option<usize>,
    max_history_entry_bytes: Option<usize>,
    auth_per_minute: Option<u32>,
}

#[derive(Clone, Copy)]
enum Kind {
    Str,
    Bool,
    Int,
}

/// Every key that may be overridden from the environment (dotted path, value kind).
const ENV_KEYS: &[(&str, Kind)] = &[
    ("listen", Kind::Str),
    ("public_url", Kind::Str),
    ("database_url", Kind::Str),
    ("db_max_connections", Kind::Int),
    ("instance_key", Kind::Str),
    ("master_key", Kind::Str),
    ("allow_signup", Kind::Bool),
    ("trust_proxy", Kind::Bool),
    ("metrics_bind", Kind::Str),
    ("log_format", Kind::Str),
    ("log_level", Kind::Str),
    ("tls.mode", Kind::Str),
    ("tls.cert_path", Kind::Str),
    ("tls.key_path", Kind::Str),
    ("tls.domain", Kind::Str),
    ("tls.acme_email", Kind::Str),
    ("tls.acme_cache_dir", Kind::Str),
    ("tls.acme_staging", Kind::Bool),
    ("limits.max_blob_bytes", Kind::Int),
    ("limits.max_history_batch", Kind::Int),
    ("limits.max_history_entry_bytes", Kind::Int),
    ("limits.auth_per_minute", Kind::Int),
];

/// Environment variable name for a dotted config key: `tls.mode` → `COPPER_CLOUD_TLS__MODE`.
pub fn env_var_name(key: &str) -> String {
    let mut name = String::with_capacity(ENV_PREFIX.len() + key.len() + 2);
    name.push_str(ENV_PREFIX);
    for c in key.chars() {
        if c == '.' {
            name.push_str("__");
        } else {
            name.push(c.to_ascii_uppercase());
        }
    }
    name
}

fn parse_env_value(key: &str, kind: Kind, raw: &str) -> anyhow::Result<toml::Value> {
    let v = raw.trim();
    Ok(match kind {
        Kind::Str => toml::Value::String(v.to_owned()),
        Kind::Bool => match v.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => toml::Value::Boolean(true),
            "0" | "false" | "no" | "off" => toml::Value::Boolean(false),
            _ => bail!("{}: expected a boolean, got {v:?}", env_var_name(key)),
        },
        Kind::Int => toml::Value::Integer(
            v.replace('_', "")
                .parse::<i64>()
                .with_context(|| format!("{}: expected an integer", env_var_name(key)))?,
        ),
    })
}

fn overlay_env(
    table: &mut toml::Table,
    env: &dyn Fn(&str) -> Option<String>,
) -> anyhow::Result<()> {
    for &(key, kind) in ENV_KEYS {
        let Some(raw) = env(&env_var_name(key)) else {
            continue;
        };
        let value = parse_env_value(key, kind, &raw)?;
        let mut parts = key.split('.').peekable();
        let mut cur = &mut *table;
        while let Some(part) = parts.next() {
            if parts.peek().is_none() {
                cur.insert(part.to_owned(), value);
                break;
            }
            let entry = cur
                .entry(part.to_owned())
                .or_insert_with(|| toml::Value::Table(toml::Table::new()));
            cur = entry
                .as_table_mut()
                .ok_or_else(|| anyhow!("config key `{part}` must be a table"))?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Loading

/// Where the config file comes from and whether its absence is an error.
#[derive(Clone, Debug)]
pub struct ConfigSource {
    pub path: PathBuf,
    /// `true` when given via `--config`/`COPPER_CLOUD_CONFIG`: a missing file is an error.
    /// The default path may be missing (pure-env configuration).
    pub explicit: bool,
}

impl ConfigSource {
    pub fn resolve(cli: Option<PathBuf>) -> Self {
        if let Some(path) = cli {
            return Self {
                path,
                explicit: true,
            };
        }
        match std::env::var_os("COPPER_CLOUD_CONFIG") {
            Some(p) if !p.is_empty() => Self {
                path: PathBuf::from(p),
                explicit: true,
            },
            _ => Self {
                path: PathBuf::from(DEFAULT_CONFIG_PATH),
                explicit: false,
            },
        }
    }
}

impl Config {
    /// Load from `source` + the process environment.
    pub fn load(source: &ConfigSource) -> anyhow::Result<Self> {
        let text = match std::fs::read_to_string(&source.path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !source.explicit => String::new(),
            Err(e) => {
                return Err(e)
                    .with_context(|| format!("reading config file {}", source.path.display()))
            }
        };
        Self::from_toml_str(&text, &|k| std::env::var(k).ok())
            .with_context(|| format!("invalid configuration ({})", source.path.display()))
    }

    /// Parse TOML text overlaid with env values from `env` (injectable for tests).
    pub fn from_toml_str(text: &str, env: &dyn Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        let mut table: toml::Table = toml::from_str(text).context("parsing TOML")?;
        overlay_env(&mut table, env)?;
        let raw: RawConfig = toml::Value::Table(table)
            .try_into()
            .context("decoding configuration")?;
        Self::from_raw(raw)
    }

    #[allow(clippy::too_many_lines)]
    fn from_raw(raw: RawConfig) -> anyhow::Result<Self> {
        let listen: SocketAddr = raw
            .listen
            .as_deref()
            .unwrap_or("0.0.0.0:443")
            .parse()
            .context("`listen` must be an ip:port socket address")?;

        let database_url = raw
            .database_url
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| anyhow!("`database_url` is required (or COPPER_CLOUD_DATABASE_URL)"))?;
        if !(database_url.starts_with("postgres://") || database_url.starts_with("postgresql://")) {
            bail!("`database_url` must start with postgres:// or postgresql://");
        }

        let instance_key = raw
            .instance_key
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                anyhow!("`instance_key` is required (generate with `copper-cloud init-config`)")
            })?;
        validate_instance_key(&instance_key)?;

        let master_key = parse_master_key(raw.master_key.as_deref().ok_or_else(|| {
            anyhow!("`master_key` is required (generate with `copper-cloud init-config`)")
        })?)?;

        let tls_mode = raw.tls.mode.unwrap_or_default();
        let domain = raw
            .tls
            .domain
            .map(|d| d.trim().trim_end_matches('.').to_ascii_lowercase())
            .filter(|d| !d.is_empty());
        if tls_mode == TlsMode::Acme && domain.is_none() {
            bail!("`tls.mode = \"acme\"` requires `tls.domain`");
        }
        let tls = TlsConfig {
            mode: tls_mode,
            cert_path: raw
                .tls
                .cert_path
                .unwrap_or_else(|| PathBuf::from("/etc/copper-cloud/tls/cert.pem")),
            key_path: raw
                .tls
                .key_path
                .unwrap_or_else(|| PathBuf::from("/etc/copper-cloud/tls/key.pem")),
            domain,
            acme_email: raw.tls.acme_email.filter(|s| !s.trim().is_empty()),
            acme_cache_dir: raw
                .tls
                .acme_cache_dir
                .unwrap_or_else(|| PathBuf::from("/var/lib/copper-cloud/acme")),
            acme_staging: raw.tls.acme_staging.unwrap_or(false),
        };

        let public_url = match raw.public_url.filter(|s| !s.trim().is_empty()) {
            Some(u) => normalize_public_url(&u)?,
            None => match &tls.domain {
                Some(d) => d.clone(),
                None => bail!("`public_url` is required (the host[:port] clients connect to)"),
            },
        };

        let defaults = Limits::default();
        let limits = Limits {
            max_blob_bytes: raw.limits.max_blob_bytes.unwrap_or(defaults.max_blob_bytes),
            max_history_batch: raw
                .limits
                .max_history_batch
                .unwrap_or(defaults.max_history_batch),
            max_history_entry_bytes: raw
                .limits
                .max_history_entry_bytes
                .unwrap_or(defaults.max_history_entry_bytes),
            auth_per_minute: raw
                .limits
                .auth_per_minute
                .unwrap_or(defaults.auth_per_minute),
        };
        if limits.max_blob_bytes < 1024 || limits.max_blob_bytes > 256 * 1024 * 1024 {
            bail!("`limits.max_blob_bytes` must be between 1 KiB and 256 MiB");
        }
        if limits.max_history_batch == 0 || limits.max_history_batch > 100_000 {
            bail!("`limits.max_history_batch` must be between 1 and 100000");
        }
        if limits.max_history_entry_bytes < 256 || limits.max_history_entry_bytes > 1024 * 1024 {
            bail!("`limits.max_history_entry_bytes` must be between 256 B and 1 MiB");
        }
        if limits.auth_per_minute == 0 {
            bail!("`limits.auth_per_minute` must be at least 1");
        }

        let metrics_bind = match raw.metrics_bind.as_deref().map(str::trim) {
            None => Some(DEFAULT_METRICS_BIND.parse().expect("valid default")),
            Some("" | "off" | "none" | "false") => None,
            Some(s) => Some(
                s.parse()
                    .context("`metrics_bind` must be an ip:port socket address or \"off\"")?,
            ),
        };

        let db_max_connections = raw.db_max_connections.unwrap_or(20);
        if db_max_connections == 0 {
            bail!("`db_max_connections` must be at least 1");
        }

        Ok(Self {
            listen,
            public_url,
            database_url,
            db_max_connections,
            instance_key,
            master_key,
            allow_signup: raw.allow_signup.unwrap_or(true),
            trust_proxy: raw.trust_proxy.unwrap_or(false),
            tls,
            limits,
            metrics_bind,
            log_format: raw.log_format.unwrap_or_default(),
            log_level: raw
                .log_level
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "info".into()),
        })
    }

    /// A self-contained config for tests and tooling: random keys, TLS off, ephemeral port,
    /// no metrics listener. Mutate fields as needed.
    pub fn for_tests(database_url: &str) -> Self {
        Self {
            listen: "127.0.0.1:0".parse().expect("valid"),
            public_url: "localhost:8443".into(),
            database_url: database_url.to_owned(),
            db_max_connections: 10,
            instance_key: crate::ids::random_token(),
            master_key: crate::ids::random_bytes::<32>(),
            allow_signup: true,
            trust_proxy: false,
            tls: TlsConfig {
                mode: TlsMode::Off,
                cert_path: PathBuf::from("cert.pem"),
                key_path: PathBuf::from("key.pem"),
                domain: None,
                acme_email: None,
                acme_cache_dir: PathBuf::from("acme"),
                acme_staging: true,
            },
            limits: Limits::default(),
            metrics_bind: None,
            log_format: LogFormat::Pretty,
            log_level: "info".into(),
        }
    }

    /// Host part of `public_url` (no brackets for IPv6).
    pub fn public_host(&self) -> &str {
        split_host_port(&self.public_url).0
    }

    /// Port clients connect to: explicit in `public_url`, else 443 for ACME, else the listen
    /// port.
    pub fn public_port(&self) -> u16 {
        split_host_port(&self.public_url)
            .1
            .unwrap_or(if self.tls.mode == TlsMode::Acme {
                443
            } else {
                self.listen.port()
            })
    }
}

/// Instance keys travel in a header and in the link-code URL fragment, so restrict them to
/// RFC 3986 unreserved characters and require ≥ 32 characters (base64url of 32 bytes is 43).
pub fn validate_instance_key(key: &str) -> anyhow::Result<()> {
    if key.len() < 32 {
        bail!("`instance_key` must be at least 32 characters");
    }
    if key.len() > 512 {
        bail!("`instance_key` must be at most 512 characters");
    }
    if !key
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~'))
    {
        bail!("`instance_key` may only contain A-Z a-z 0-9 - _ . ~");
    }
    Ok(())
}

/// Decode a base64 / base64url master key that must be exactly 32 bytes.
pub fn parse_master_key(s: &str) -> anyhow::Result<[u8; 32]> {
    let bytes = crate::ids::b64_decode_any(s).context("`master_key` must be base64url")?;
    bytes.as_slice().try_into().map_err(|_| {
        anyhow!(
            "`master_key` must decode to exactly 32 bytes (got {})",
            bytes.len()
        )
    })
}

/// Accepts `host`, `host:port`, `https://host[:port][/...]`, `[v6]:port`; returns `host[:port]`.
pub fn normalize_public_url(input: &str) -> anyhow::Result<String> {
    let mut s = input.trim();
    for scheme in ["https://", "http://", "copper-cloud://"] {
        if let Some(rest) = s.strip_prefix(scheme) {
            s = rest;
        }
    }
    let s = s.split(['/', '?', '#']).next().unwrap_or_default();
    if s.is_empty() || s.contains('@') || s.contains(char::is_whitespace) {
        bail!("`public_url` must look like host[:port]");
    }
    let (host, port) = split_host_port(s);
    if host.is_empty() {
        bail!("`public_url` has an empty host");
    }
    if let Some(p) = port {
        if p == 0 {
            bail!("`public_url` port must be non-zero");
        }
    } else if s.rsplit_once(':').is_some_and(|(h, _)| !h.contains(':')) {
        bail!("`public_url` has an invalid port");
    }
    Ok(s.to_ascii_lowercase())
}

/// Split `host[:port]` / `[v6][:port]` / bare `v6` into (host, port).
pub fn split_host_port(s: &str) -> (&str, Option<u16>) {
    if let Some(rest) = s.strip_prefix('[') {
        if let Some((host, after)) = rest.split_once(']') {
            let port = after.strip_prefix(':').and_then(|p| p.parse().ok());
            return (host, port);
        }
        return (rest, None);
    }
    match s.rsplit_once(':') {
        // A bare IPv6 address has several colons and no port.
        Some((h, _)) if h.contains(':') => (s, None),
        Some((h, p)) => match p.parse() {
            Ok(port) => (h, Some(port)),
            Err(_) => (s, None),
        },
        None => (s, None),
    }
}

/// Options for [`generate_toml`].
#[derive(Clone, Debug)]
pub struct GenerateOptions {
    pub public_url: String,
    pub database_url: String,
    pub listen: String,
    pub tls_mode: TlsMode,
    pub domain: Option<String>,
    pub acme_email: Option<String>,
    pub instance_key: Option<String>,
    pub master_key: Option<String>,
    pub allow_signup: bool,
    pub cert_path: PathBuf,
    pub key_path: PathBuf,
}

/// Render a complete, commented config file (fresh keys unless provided).
pub fn generate_toml(opts: &GenerateOptions) -> anyhow::Result<String> {
    let instance_key = opts
        .instance_key
        .clone()
        .unwrap_or_else(crate::ids::random_token);
    validate_instance_key(&instance_key)?;
    let master_key = opts
        .master_key
        .clone()
        .unwrap_or_else(|| crate::ids::b64url(&crate::ids::random_bytes::<32>()));
    parse_master_key(&master_key)?;
    let public_url = normalize_public_url(&opts.public_url)?;
    let q = |s: &str| toml::Value::String(s.to_owned()).to_string();
    let path_q = |p: &Path| q(&p.to_string_lossy());
    let mut out = String::with_capacity(1536);
    out.push_str("# copper-cloud configuration. Keep this file private (0600): it holds the\n");
    out.push_str("# instance key and the master key that wraps every user's data key.\n");
    out.push_str(
        "# Every key can be overridden by COPPER_CLOUD_<KEY> (nested: COPPER_CLOUD_TLS__MODE).\n\n",
    );
    writeln!(out, "listen = {}", q(&opts.listen))?;
    out.push_str("# host[:port] clients connect to (shown in the link code)\n");
    writeln!(out, "public_url = {}", q(&public_url))?;
    writeln!(out, "database_url = {}", q(&opts.database_url))?;
    out.push_str("db_max_connections = 20\n");
    writeln!(out, "instance_key = {}", q(&instance_key))?;
    out.push_str("# Losing the master key makes all stored sync data unreadable. Back it up.\n");
    writeln!(out, "master_key = {}", q(&master_key))?;
    writeln!(out, "allow_signup = {}", opts.allow_signup)?;
    out.push_str("trust_proxy = false\n");
    writeln!(out, "metrics_bind = {}", q(DEFAULT_METRICS_BIND))?;
    out.push_str("log_format = \"json\"\nlog_level = \"info\"\n\n");
    out.push_str("[tls]\n# \"self-signed\" | \"acme\" | \"off\" (off only behind a TLS proxy)\n");
    writeln!(out, "mode = {}", q(&opts.tls_mode.to_string()))?;
    writeln!(out, "cert_path = {}", path_q(&opts.cert_path))?;
    writeln!(out, "key_path = {}", path_q(&opts.key_path))?;
    if let Some(d) = &opts.domain {
        writeln!(out, "domain = {}", q(d))?;
    }
    if let Some(e) = &opts.acme_email {
        writeln!(out, "acme_email = {}", q(e))?;
    }
    out.push_str("acme_cache_dir = \"/var/lib/copper-cloud/acme\"\n\n");
    out.push_str("[limits]\nmax_blob_bytes = 8000000\nmax_history_batch = 2000\n");
    out.push_str("max_history_entry_bytes = 16384\nauth_per_minute = 10\n");
    // Round-trip check so we never write a file we cannot load.
    Config::from_toml_str(&out, &|_| None).context("generated config failed validation")?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const BASE: &str = r#"
        listen = "0.0.0.0:8443"
        public_url = "https://cloud.example.com:8443/"
        database_url = "postgres://cc:hunter2@localhost:5432/copper_cloud?sslmode=require"
        instance_key = "abcdefghijklmnopqrstuvwxyz0123456789-_ABCDE"
        master_key = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"
        [tls]
        mode = "self-signed"
    "#;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |k| map.get(k).cloned()
    }

    #[test]
    fn parses_file_with_defaults() {
        let c = Config::from_toml_str(BASE, &env(&[])).unwrap();
        assert_eq!(c.listen, "0.0.0.0:8443".parse().unwrap());
        assert_eq!(c.public_url, "cloud.example.com:8443");
        assert_eq!(c.public_host(), "cloud.example.com");
        assert_eq!(c.public_port(), 8443);
        assert_eq!(c.master_key[31], 31);
        assert_eq!(c.tls.mode, TlsMode::SelfSigned);
        assert_eq!(c.limits.max_blob_bytes, 8_000_000);
        assert_eq!(c.limits.max_history_batch, 2000);
        assert!(c.allow_signup);
        assert_eq!(c.metrics_bind, Some(DEFAULT_METRICS_BIND.parse().unwrap()));
        assert_eq!(c.log_format, LogFormat::Json);
    }

    #[test]
    fn env_overrides_top_level_and_nested() {
        let c = Config::from_toml_str(
            BASE,
            &env(&[
                ("COPPER_CLOUD_DATABASE_URL", "postgres://localhost/other"),
                ("COPPER_CLOUD_ALLOW_SIGNUP", "false"),
                ("COPPER_CLOUD_TLS__MODE", "acme"),
                ("COPPER_CLOUD_TLS__DOMAIN", "Cloud.Example.org."),
                ("COPPER_CLOUD_LIMITS__MAX_BLOB_BYTES", "4_000_000"),
                ("COPPER_CLOUD_METRICS_BIND", "off"),
                ("COPPER_CLOUD_LOG_FORMAT", "pretty"),
                // Unknown COPPER_CLOUD_* vars (e.g. installer inputs) are ignored.
                ("COPPER_CLOUD_BINARY", "/tmp/x"),
            ]),
        )
        .unwrap();
        assert_eq!(c.database_url, "postgres://localhost/other");
        assert!(!c.allow_signup);
        assert_eq!(c.tls.mode, TlsMode::Acme);
        assert_eq!(c.tls.domain.as_deref(), Some("cloud.example.org"));
        assert_eq!(c.limits.max_blob_bytes, 4_000_000);
        assert_eq!(c.metrics_bind, None);
        assert_eq!(c.log_format, LogFormat::Pretty);
    }

    #[test]
    fn env_only_config() {
        let c = Config::from_toml_str(
            "",
            &env(&[
                ("COPPER_CLOUD_DATABASE_URL", "postgres://localhost/db"),
                (
                    "COPPER_CLOUD_INSTANCE_KEY",
                    "0123456789012345678901234567890123456789",
                ),
                ("COPPER_CLOUD_MASTER_KEY", &crate::ids::b64url(&[1u8; 32])),
                ("COPPER_CLOUD_PUBLIC_URL", "203.0.113.7"),
            ]),
        )
        .unwrap();
        // A purely numeric instance key stays a string.
        assert_eq!(c.instance_key, "0123456789012345678901234567890123456789");
        assert_eq!(c.public_port(), 443);
        assert_eq!(c.listen.port(), 443);
    }

    #[test]
    fn rejects_bad_values() {
        let bad_bool = Config::from_toml_str(BASE, &env(&[("COPPER_CLOUD_TRUST_PROXY", "maybe")]));
        assert!(bad_bool.is_err());
        let bad_key = Config::from_toml_str(BASE, &env(&[("COPPER_CLOUD_INSTANCE_KEY", "short")]));
        assert!(bad_key.is_err());
        let bad_master = Config::from_toml_str(BASE, &env(&[("COPPER_CLOUD_MASTER_KEY", "AAAA")]));
        assert!(bad_master.is_err());
        let unknown = Config::from_toml_str(&format!("{BASE}\nnope = 1\n"), &env(&[]));
        assert!(
            unknown.is_err(),
            "unknown top-level key after [tls] lands in tls table"
        );
        let acme_no_domain =
            Config::from_toml_str(BASE, &env(&[("COPPER_CLOUD_TLS__MODE", "acme")]));
        assert!(acme_no_domain.is_err());
        let missing_db = Config::from_toml_str(
            "instance_key = \"0123456789012345678901234567890123456789\"",
            &env(&[]),
        );
        assert!(missing_db.is_err());
    }

    #[test]
    fn debug_redacts_secrets() {
        let c = Config::from_toml_str(BASE, &env(&[])).unwrap();
        let dbg = format!("{c:?}");
        assert!(!dbg.contains("hunter2"));
        assert!(!dbg.contains("abcdefghijklmnop"));
        assert!(!dbg.contains("AAECAwQF"));
        assert!(dbg.contains("postgres://cc:***@localhost:5432/copper_cloud"));
    }

    #[test]
    fn public_url_normalization() {
        assert_eq!(normalize_public_url("1.2.3.4").unwrap(), "1.2.3.4");
        assert_eq!(normalize_public_url("https://A.B:9/x").unwrap(), "a.b:9");
        assert_eq!(
            normalize_public_url("[2001:db8::1]:8443").unwrap(),
            "[2001:db8::1]:8443"
        );
        assert_eq!(
            split_host_port("[2001:db8::1]:8443"),
            ("2001:db8::1", Some(8443))
        );
        assert_eq!(split_host_port("2001:db8::1"), ("2001:db8::1", None));
        assert!(normalize_public_url("user@host").is_err());
        assert!(normalize_public_url("host:abc").is_err());
        assert!(normalize_public_url("").is_err());
    }

    #[test]
    fn generated_config_loads() {
        let text = generate_toml(&GenerateOptions {
            public_url: "198.51.100.4".into(),
            database_url: "postgres://localhost/copper_cloud".into(),
            listen: "0.0.0.0:443".into(),
            tls_mode: TlsMode::SelfSigned,
            domain: None,
            acme_email: None,
            instance_key: None,
            master_key: None,
            allow_signup: true,
            cert_path: "/etc/copper-cloud/tls/cert.pem".into(),
            key_path: "/etc/copper-cloud/tls/key.pem".into(),
        })
        .unwrap();
        let c = Config::from_toml_str(&text, &env(&[])).unwrap();
        assert_eq!(c.public_url, "198.51.100.4");
        assert_eq!(c.instance_key.len(), 43);
    }

    #[test]
    fn env_names() {
        assert_eq!(
            env_var_name("tls.acme_email"),
            "COPPER_CLOUD_TLS__ACME_EMAIL"
        );
        assert_eq!(env_var_name("database_url"), "COPPER_CLOUD_DATABASE_URL");
    }
}
