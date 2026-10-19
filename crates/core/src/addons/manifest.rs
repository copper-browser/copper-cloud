//! `manifest.json` (schema `copper-addon/1`): parsing and validation, identical in spirit to
//! the clients' (Rust core, Swift) so anything the cloud accepts installs everywhere.
//!
//! Unknown top-level keys are kept (in [`Manifest::raw`], served verbatim inside the bundle)
//! and ignored.

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

use super::Slot;
use crate::error::{ApiError, ApiResult};

pub const SCHEMA: &str = "copper-addon/1";
/// Bridge capabilities an addon may ask for.
pub const CAPABILITIES: &[&str] = &[
    "identity", "storage", "fetch", "oauth", "open-url", "page", "agent",
];
/// Client platforms. An absent `platforms` means all of them.
pub const PLATFORMS: &[&str] = &["mac", "linux", "windows"];
pub const MAX_NAME_CHARS: usize = 60;
pub const MAX_DESCRIPTION_CHARS: usize = 280;
pub const MAX_AUTHOR_CHARS: usize = 200;
pub const MAX_HOSTS: usize = 32;
/// Most settings one manifest may declare.
pub const MAX_SETTINGS: usize = 64;
const MAX_SETTING_TITLE_CHARS: usize = 100;
const MAX_SETTING_DESCRIPTION_CHARS: usize = 500;
/// Cap on the admin's settings values (serialized JSON).
pub const MAX_SETTINGS_BYTES: usize = 16 * 1024;
/// Longest bundle path.
pub const MAX_PATH_LEN: usize = 200;

/// The JSON type a setting holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingType {
    String,
    Number,
    Boolean,
    Object,
    Array,
}

impl SettingType {
    pub const ALL: &'static [Self] = &[
        Self::String,
        Self::Number,
        Self::Boolean,
        Self::Object,
        Self::Array,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Number => "number",
            Self::Boolean => "boolean",
            Self::Object => "object",
            Self::Array => "array",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|t| t.as_str() == raw)
    }

    /// Whether `v` is a value of this type (`null` never is).
    pub fn accepts(self, v: &Value) -> bool {
        matches!(
            (self, v),
            (Self::String, Value::String(_))
                | (Self::Number, Value::Number(_))
                | (Self::Boolean, Value::Bool(_))
                | (Self::Object, Value::Object(_))
                | (Self::Array, Value::Array(_))
        )
    }
}

/// One entry of `settings`.
#[derive(Clone, Debug, PartialEq)]
pub struct SettingSpec {
    pub kind: SettingType,
    pub default: Option<Value>,
}

/// A validated manifest.
#[derive(Clone, Debug)]
pub struct Manifest {
    /// The manifest object exactly as uploaded (unknown keys kept).
    pub raw: Value,
    pub id: String,
    pub name: String,
    pub version: String,
    pub slot: Slot,
    pub entry: String,
    pub description: Option<String>,
    pub bridge: u64,
    pub hosts: Vec<String>,
    pub capabilities: Vec<String>,
    pub settings: BTreeMap<String, SettingSpec>,
    /// `None` = every platform.
    pub platforms: Option<Vec<String>>,
}

fn bad(msg: impl std::fmt::Display) -> ApiError {
    ApiError::bad_request(format!("manifest.json: {msg}"))
}

/// `obj[key]` as an optional string (`null` = absent); any other type is an error.
fn opt_str<'a>(obj: &'a Map<String, Value>, key: &str) -> ApiResult<Option<&'a str>> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(bad(format!("\"{key}\" must be a string"))),
    }
}

fn req_str<'a>(obj: &'a Map<String, Value>, key: &str) -> ApiResult<&'a str> {
    opt_str(obj, key)?.ok_or_else(|| bad(format!("\"{key}\" is required")))
}

/// Optional array of strings (`null` = absent).
fn opt_str_array<'a>(obj: &'a Map<String, Value>, key: &str) -> ApiResult<Option<Vec<&'a str>>> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| {
                v.as_str()
                    .ok_or_else(|| bad(format!("\"{key}\" must be an array of strings")))
            })
            .collect::<ApiResult<Vec<_>>>()
            .map(Some),
        Some(_) => Err(bad(format!("\"{key}\" must be an array of strings"))),
    }
}

fn plain_text(s: &str) -> bool {
    !s.chars().any(char::is_control)
}

/// Addon id: `^[a-z0-9][a-z0-9.-]{2,63}$`, no `..`, at least one `.` (vendor prefix).
pub fn valid_id(id: &str) -> bool {
    let b = id.as_bytes();
    (3..=64).contains(&b.len())
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'.' | b'-'))
        && !id.contains("..")
        && id.contains('.')
}

fn numeric_identifier(n: &str) -> bool {
    !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) && (n == "0" || !n.starts_with('0'))
}

/// Semver `MAJOR.MINOR.PATCH` with an optional `-pre` (dot-separated `[0-9A-Za-z-]+`), no
/// build metadata, ≤ 64 characters.
pub fn valid_version(v: &str) -> bool {
    if v.len() > 64 {
        return false;
    }
    let (core, pre) = match v.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (v, None),
    };
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|p| numeric_identifier(p))
        && pre.is_none_or(|p| {
            p.split('.').all(|id| {
                !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            })
        })
}

/// Bundle path rule: `^[A-Za-z0-9._@-]+(/[A-Za-z0-9._@-]+)*$`, no `.` / `..` segment,
/// ≤ [`MAX_PATH_LEN`] characters.
pub fn valid_bundle_path(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= MAX_PATH_LEN
        && p.split('/').all(|seg| {
            !seg.is_empty()
                && seg != "."
                && seg != ".."
                && seg
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'@' | b'-'))
        })
}

fn valid_hostname(h: &str) -> bool {
    !h.is_empty()
        && h.len() <= 253
        && h.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

/// `permissions.hosts` entry: a lowercase hostname or `*.` + hostname (at least two labels
/// after the wildcard), optional `:port`, optional `https://` prefix. `http://` only for
/// `localhost` / `127.0.0.1` (tests and local dev servers).
pub fn valid_host_rule(rule: &str) -> bool {
    let (http, rest) = if let Some(r) = rule.strip_prefix("https://") {
        (false, r)
    } else if let Some(r) = rule.strip_prefix("http://") {
        (true, r)
    } else {
        (false, rule)
    };
    let (host, port) = match rest.rsplit_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (rest, None),
    };
    if let Some(p) = port {
        let ok = !p.is_empty()
            && p.len() <= 5
            && p.bytes().all(|b| b.is_ascii_digit())
            && !p.starts_with('0')
            && p.parse::<u16>().is_ok();
        if !ok {
            return false;
        }
    }
    let (wildcard, name) = match host.strip_prefix("*.") {
        Some(n) => (true, n),
        None => (false, host),
    };
    if !valid_hostname(name) || (wildcard && !name.contains('.')) {
        return false;
    }
    !http || (!wildcard && matches!(name, "localhost" | "127.0.0.1"))
}

/// Settings key: `^[A-Za-z_][A-Za-z0-9_.-]{0,63}$`.
fn valid_setting_key(k: &str) -> bool {
    let b = k.as_bytes();
    (1..=64).contains(&b.len())
        && (b[0].is_ascii_alphabetic() || b[0] == b'_')
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-'))
}

fn parse_settings(obj: &Map<String, Value>) -> ApiResult<BTreeMap<String, SettingSpec>> {
    let specs = match obj.get("settings") {
        None | Some(Value::Null) => return Ok(BTreeMap::new()),
        Some(Value::Object(m)) => m,
        Some(_) => return Err(bad("\"settings\" must be an object")),
    };
    if specs.len() > MAX_SETTINGS {
        return Err(bad(format!(
            "at most {MAX_SETTINGS} settings may be declared"
        )));
    }
    let mut out = BTreeMap::new();
    for (key, spec) in specs {
        if !valid_setting_key(key) {
            return Err(bad(format!(
                "setting name \"{key}\" must be 1-64 letters, digits, '_', '.' or '-' (starting with a letter or '_')"
            )));
        }
        let Some(spec) = spec.as_object() else {
            return Err(bad(format!("settings.{key} must be an object")));
        };
        let types = SettingType::ALL
            .iter()
            .map(|t| t.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let kind = spec
            .get("type")
            .and_then(Value::as_str)
            .and_then(SettingType::parse)
            .ok_or_else(|| bad(format!("settings.{key}.type must be one of: {types}")))?;
        let default = match spec.get("default") {
            None | Some(Value::Null) => None,
            Some(d) if kind.accepts(d) => Some(d.clone()),
            Some(_) => {
                return Err(bad(format!(
                    "settings.{key}.default must be a {}",
                    kind.as_str()
                )))
            }
        };
        for (field, max) in [
            ("title", MAX_SETTING_TITLE_CHARS),
            ("description", MAX_SETTING_DESCRIPTION_CHARS),
        ] {
            match spec.get(field) {
                None | Some(Value::Null) => {}
                Some(Value::String(s)) if s.chars().count() <= max => {}
                Some(_) => {
                    return Err(bad(format!(
                        "settings.{key}.{field} must be a string of at most {max} characters"
                    )))
                }
            }
        }
        out.insert(key.clone(), SettingSpec { kind, default });
    }
    Ok(out)
}

impl Manifest {
    /// Parse and validate `manifest.json` bytes.
    pub fn parse(bytes: &[u8]) -> ApiResult<Self> {
        let raw: Value =
            serde_json::from_slice(bytes).map_err(|e| bad(format!("not valid JSON ({e})")))?;
        Self::from_value(raw)
    }

    /// Validate a manifest object (also used for manifests read back from the database).
    #[allow(clippy::too_many_lines)]
    pub fn from_value(raw: Value) -> ApiResult<Self> {
        let Some(obj) = raw.as_object() else {
            return Err(bad("must be a JSON object"));
        };
        let schema = req_str(obj, "schema")?;
        if schema != SCHEMA {
            return Err(bad(format!("\"schema\" must be \"{SCHEMA}\"")));
        }
        let id = req_str(obj, "id")?;
        if !valid_id(id) {
            return Err(bad(format!(
                "\"id\" \"{id}\" must be 3-64 lowercase letters, digits, '.' or '-', start with a letter or digit, contain a '.' (vendor prefix, e.g. com.example.sidebar) and no \"..\""
            )));
        }
        let name = req_str(obj, "name")?;
        if name.trim().is_empty() || name.chars().count() > MAX_NAME_CHARS || !plain_text(name) {
            return Err(bad(format!(
                "\"name\" must be 1-{MAX_NAME_CHARS} characters"
            )));
        }
        let version = req_str(obj, "version")?;
        if !valid_version(version) {
            return Err(bad(format!(
                "\"version\" \"{version}\" must be MAJOR.MINOR.PATCH with an optional -prerelease (e.g. 1.2.0 or 1.2.0-beta.1)"
            )));
        }
        let slot_id = req_str(obj, "slot")?;
        let slot = Slot::parse(slot_id).ok_or_else(|| {
            bad(format!(
                "unknown slot \"{slot_id}\" (known: {})",
                Slot::known_list()
            ))
        })?;
        let entry = req_str(obj, "entry")?;
        let html = std::path::Path::new(entry)
            .extension()
            .is_some_and(|e| e == "html");
        if !(html && valid_bundle_path(entry)) {
            return Err(bad(format!(
                "\"entry\" \"{entry}\" must be a bundle path ending in .html"
            )));
        }
        let description = opt_str(obj, "description")?;
        if description.is_some_and(|d| d.chars().count() > MAX_DESCRIPTION_CHARS) {
            return Err(bad(format!(
                "\"description\" must be at most {MAX_DESCRIPTION_CHARS} characters"
            )));
        }
        if opt_str(obj, "author")?.is_some_and(|a| a.chars().count() > MAX_AUTHOR_CHARS) {
            return Err(bad(format!(
                "\"author\" must be at most {MAX_AUTHOR_CHARS} characters"
            )));
        }
        let bridge = obj
            .get("bridge")
            .and_then(Value::as_u64)
            .filter(|b| (1..=u64::from(u32::MAX)).contains(b))
            .ok_or_else(|| bad("\"bridge\" must be an integer >= 1 (the bridge version, 1)"))?;

        let (hosts, capabilities) = match obj.get("permissions") {
            Some(Value::Object(p)) => {
                let hosts = opt_str_array(p, "hosts")?.unwrap_or_default();
                if hosts.len() > MAX_HOSTS {
                    return Err(bad(format!(
                        "permissions.hosts may list at most {MAX_HOSTS} hosts"
                    )));
                }
                if let Some(h) = hosts.iter().find(|h| !valid_host_rule(h)) {
                    return Err(bad(format!(
                        "permissions.hosts entry \"{h}\" must be a lowercase hostname or *.hostname with an optional :port (http:// only for localhost / 127.0.0.1)"
                    )));
                }
                let caps = opt_str_array(p, "capabilities")?.unwrap_or_default();
                if let Some(c) = caps.iter().find(|c| !CAPABILITIES.contains(c)) {
                    return Err(bad(format!(
                        "unknown capability \"{c}\" (known: {})",
                        CAPABILITIES.join(", ")
                    )));
                }
                (
                    hosts.into_iter().map(str::to_owned).collect(),
                    caps.into_iter().map(str::to_owned).collect(),
                )
            }
            None => return Err(bad("\"permissions\" is required (it may be {})")),
            Some(_) => return Err(bad("\"permissions\" must be an object")),
        };

        let settings = parse_settings(obj)?;

        let platforms = match opt_str_array(obj, "platforms")? {
            None => None,
            Some(p) if p.is_empty() => {
                return Err(bad(
                    "\"platforms\" must not be empty (omit it for every platform)",
                ))
            }
            Some(p) => {
                if let Some(x) = p.iter().find(|x| !PLATFORMS.contains(x)) {
                    return Err(bad(format!(
                        "unknown platform \"{x}\" (known: {})",
                        PLATFORMS.join(", ")
                    )));
                }
                let mut seen = Vec::with_capacity(p.len());
                for x in p {
                    if !seen.contains(&x) {
                        seen.push(x);
                    }
                }
                Some(seen.into_iter().map(str::to_owned).collect())
            }
        };

        Ok(Self {
            id: id.to_owned(),
            name: name.to_owned(),
            version: version.to_owned(),
            slot,
            entry: entry.to_owned(),
            description: description.map(str::to_owned),
            bridge,
            hosts,
            capabilities,
            settings,
            platforms,
            raw,
        })
    }

    /// Platforms this addon runs on (every platform when the manifest names none).
    pub fn platforms(&self) -> Vec<String> {
        self.platforms
            .clone()
            .unwrap_or_else(|| PLATFORMS.iter().map(|p| (*p).to_owned()).collect())
    }

    /// `{hosts, capabilities}` as served by `GET /v1/addons`.
    pub fn permissions_view(&self) -> Value {
        json!({ "hosts": self.hosts, "capabilities": self.capabilities })
    }

    /// The settings a client receives: manifest defaults overlaid by the admin's values.
    /// Admin values the manifest no longer declares, or whose type no longer matches, are
    /// ignored; settings with neither a value nor a default are omitted.
    pub fn resolve_settings(&self, admin: &Value) -> Map<String, Value> {
        let admin = admin.as_object();
        let mut out = Map::new();
        for (key, spec) in &self.settings {
            let value = admin
                .and_then(|a| a.get(key))
                .filter(|v| spec.kind.accepts(v))
                .or(spec.default.as_ref());
            if let Some(v) = value {
                out.insert(key.clone(), v.clone());
            }
        }
        out
    }

    /// Validate admin settings values against this manifest: an object whose keys are
    /// declared settings and whose values have the declared type. `null` drops a key (back
    /// to the default). Returns the values to store.
    pub fn validate_settings(&self, values: &Value) -> ApiResult<Map<String, Value>> {
        let Some(values) = values.as_object() else {
            return Err(ApiError::bad_request("settings must be a JSON object"));
        };
        let mut out = Map::new();
        for (key, v) in values {
            let Some(spec) = self.settings.get(key) else {
                let declared = if self.settings.is_empty() {
                    "this addon declares no settings".to_owned()
                } else {
                    format!(
                        "declared: {}",
                        self.settings.keys().cloned().collect::<Vec<_>>().join(", ")
                    )
                };
                return Err(ApiError::bad_request(format!(
                    "unknown setting \"{key}\" ({declared})"
                )));
            };
            if v.is_null() {
                continue;
            }
            if !spec.kind.accepts(v) {
                return Err(ApiError::bad_request(format!(
                    "setting \"{key}\" must be a {}",
                    spec.kind.as_str()
                )));
            }
            out.insert(key.clone(), v.clone());
        }
        let size = serde_json::to_vec(&out).map_or(usize::MAX, |b| b.len());
        if size > MAX_SETTINGS_BYTES {
            return Err(ApiError::bad_request(format!(
                "settings must be at most {MAX_SETTINGS_BYTES} bytes of JSON"
            )));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn sample() -> Value {
        json!({
            "schema": "copper-addon/1",
            "id": "com.example.sidebar",
            "name": "Example sidebar",
            "version": "1.0.0",
            "slot": "agent-sidebar",
            "entry": "index.html",
            "description": "Says hello.",
            "author": "Example",
            "bridge": 1,
            "permissions": {
                "hosts": ["api.example.com", "*.example.org", "http://127.0.0.1:8080"],
                "capabilities": ["identity", "storage", "fetch"]
            },
            "settings": {
                "apiUrl": { "type": "string", "default": "https://api.example.com", "title": "API" },
                "verbose": { "type": "boolean" }
            },
            "platforms": ["mac", "linux"],
            "x-extra": { "kept": true }
        })
    }

    fn with(key: &str, v: Value) -> Value {
        let mut m = sample();
        if v.is_null() {
            m.as_object_mut().unwrap().remove(key);
        } else {
            m[key] = v;
        }
        m
    }

    #[test]
    fn valid_manifest_parses_and_keeps_unknown_keys() {
        let m = Manifest::from_value(sample()).unwrap();
        assert_eq!(m.id, "com.example.sidebar");
        assert_eq!(m.slot, Slot::AgentSidebar);
        assert_eq!(m.bridge, 1);
        assert_eq!(m.platforms(), vec!["mac", "linux"]);
        assert_eq!(m.raw["x-extra"]["kept"], true);
        assert_eq!(
            m.permissions_view(),
            json!({
                "hosts": ["api.example.com", "*.example.org", "http://127.0.0.1:8080"],
                "capabilities": ["identity", "storage", "fetch"]
            })
        );
        let all = Manifest::from_value(with("platforms", Value::Null)).unwrap();
        assert_eq!(all.platforms(), vec!["mac", "linux", "windows"]);
        let bare = Manifest::from_value(with("permissions", json!({}))).unwrap();
        assert!(bare.hosts.is_empty() && bare.capabilities.is_empty());
    }

    #[test]
    fn rejects_invalid_fields() {
        for (key, v) in [
            ("schema", json!("copper-addon/2")),
            ("schema", Value::Null),
            ("id", json!("nodot")),
            ("id", json!("Com.example.x")),
            ("id", json!("com..example")),
            ("id", json!(".com.example")),
            ("id", json!("a.")),
            ("id", json!(format!("com.{}", "x".repeat(61)))),
            ("name", json!("")),
            ("name", json!("   ")),
            ("name", json!("x".repeat(61))),
            ("name", json!("tab\there")),
            ("version", json!("1.0")),
            ("version", json!("1.0.0+build")),
            ("version", json!("01.0.0")),
            ("version", json!("1.0.0-")),
            ("version", json!("1.0.0-a..b")),
            ("version", json!(1)),
            ("slot", json!("toolbar")),
            ("slot", Value::Null),
            ("entry", json!("index.js")),
            ("entry", json!("../index.html")),
            ("entry", json!("/index.html")),
            ("entry", json!("a b.html")),
            ("description", json!("x".repeat(281))),
            ("author", json!(7)),
            ("bridge", json!(0)),
            ("bridge", json!(1.5)),
            ("bridge", json!("1")),
            ("bridge", Value::Null),
            ("permissions", Value::Null),
            ("permissions", json!([])),
            ("permissions", json!({ "hosts": ["Example.com"] })),
            ("permissions", json!({ "hosts": ["http://example.com"] })),
            ("permissions", json!({ "hosts": ["*.com"] })),
            ("permissions", json!({ "hosts": ["*"] })),
            ("permissions", json!({ "hosts": ["example.com/path"] })),
            ("permissions", json!({ "hosts": ["example.com:0"] })),
            ("permissions", json!({ "hosts": ["example.com:99999"] })),
            ("permissions", json!({ "hosts": "example.com" })),
            ("permissions", json!({ "capabilities": ["camera"] })),
            (
                "permissions",
                json!({ "hosts": (0..33).map(|i| format!("h{i}.example.com")).collect::<Vec<_>>() }),
            ),
            ("settings", json!([])),
            ("settings", json!({ "x": { "type": "date" } })),
            (
                "settings",
                json!({ "x": { "type": "string", "default": 3 } }),
            ),
            ("settings", json!({ "x": "string" })),
            ("settings", json!({ "bad key": { "type": "string" } })),
            ("platforms", json!([])),
            ("platforms", json!(["beos"])),
        ] {
            let err = Manifest::from_value(with(key, v.clone())).unwrap_err();
            assert_eq!(err.status(), 400, "{key}={v}");
            assert!(err.to_string().contains("manifest.json"), "{err}");
        }
        assert!(Manifest::parse(b"not json").is_err());
        assert!(Manifest::parse(b"[]").is_err());
    }

    #[test]
    fn grammar() {
        for ok in ["com.example.sidebar", "a.b", "x-1.y-2", "0.example"] {
            assert!(valid_id(ok), "{ok}");
        }
        for ok in ["0.0.0", "1.2.3", "10.20.30", "1.0.0-beta.1", "1.0.0-rc-1"] {
            assert!(valid_version(ok), "{ok}");
        }
        for ok in [
            "index.html",
            "a/b/c.js",
            "@scope/x_y-z.css",
            ".well-known/x",
        ] {
            assert!(valid_bundle_path(ok), "{ok}");
        }
        for bad in [
            "", "/a", "a/", "a//b", "./a", "a/../b", "a\\b", "a b", "a:b", "é",
        ] {
            assert!(!valid_bundle_path(bad), "{bad}");
        }
        assert!(!valid_bundle_path(&"a".repeat(201)));
        for ok in [
            "example.com",
            "*.example.com",
            "api.example.com:8443",
            "https://example.com",
            "http://localhost:8080",
            "http://127.0.0.1",
            "localhost",
        ] {
            assert!(valid_host_rule(ok), "{ok}");
        }
        for bad in [
            "",
            "EXAMPLE.com",
            "http://example.com",
            "http://*.localhost",
            "ftp://example.com",
            "-a.example.com",
            "example.com:",
            "example.com:080",
            "user@example.com",
        ] {
            assert!(!valid_host_rule(bad), "{bad}");
        }
    }

    #[test]
    fn settings_resolution_and_validation() {
        let m = Manifest::from_value(sample()).unwrap();
        // Defaults only; settings without a default are omitted.
        assert_eq!(
            Value::Object(m.resolve_settings(&json!({}))),
            json!({ "apiUrl": "https://api.example.com" })
        );
        // Admin values overlay defaults; stale or mistyped values are ignored.
        assert_eq!(
            Value::Object(m.resolve_settings(
                &json!({ "apiUrl": "https://other.example.com", "verbose": true, "gone": 1 })
            )),
            json!({ "apiUrl": "https://other.example.com", "verbose": true })
        );
        assert_eq!(
            Value::Object(m.resolve_settings(&json!({ "apiUrl": 3 }))),
            json!({ "apiUrl": "https://api.example.com" })
        );
        // Validation.
        assert_eq!(
            Value::Object(
                m.validate_settings(&json!({ "verbose": false, "apiUrl": null }))
                    .unwrap()
            ),
            json!({ "verbose": false })
        );
        for bad in [
            json!({ "nope": 1 }),
            json!({ "verbose": "yes" }),
            json!({ "apiUrl": 1 }),
            json!([]),
            json!({ "apiUrl": "x".repeat(MAX_SETTINGS_BYTES) }),
        ] {
            assert_eq!(
                m.validate_settings(&bad).unwrap_err().status(),
                400,
                "{bad}"
            );
        }
    }
}
