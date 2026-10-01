//! Link codes: everything a Copper needs to connect to this instance, in one string.
//!
//! `copper-cloud://HOST:PORT/#k=<instance_key>&fp=<sha256 hex of leaf cert DER>`
//!
//! The secret parts live in the URL fragment. `fp` is omitted when the certificate is
//! publicly trusted (ACME) — clients then use normal `WebPKI` validation.

use std::fmt;
use std::str::FromStr;

use crate::config::{split_host_port, validate_instance_key};

pub const SCHEME: &str = "copper-cloud://";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkCode {
    /// Hostname or IP literal (IPv6 without brackets).
    pub host: String,
    pub port: u16,
    pub instance_key: String,
    /// Lower-case hex SHA-256 of the leaf certificate DER.
    pub fingerprint: Option<String>,
}

impl fmt::Display for LinkCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(SCHEME)?;
        if self.host.contains(':') {
            write!(f, "[{}]", self.host)?;
        } else {
            f.write_str(&self.host)?;
        }
        write!(f, ":{}/#k={}", self.port, self.instance_key)?;
        if let Some(fp) = &self.fingerprint {
            write!(f, "&fp={fp}")?;
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LinkCodeError {
    #[error("link code must start with copper-cloud://")]
    Scheme,
    #[error("link code is missing host:port")]
    Authority,
    #[error("link code has an invalid port")]
    Port,
    #[error("link code is missing the instance key (k=)")]
    MissingKey,
    #[error("link code instance key is invalid")]
    InvalidKey,
    #[error("link code fingerprint (fp=) must be 64 hex characters")]
    Fingerprint,
}

/// Normalize a SHA-256 fingerprint: accepts `AB:CD:…` or plain hex, any case.
pub fn normalize_fingerprint(raw: &str) -> Option<String> {
    let hex: String = raw
        .chars()
        .filter(|c| *c != ':')
        .map(|c| c.to_ascii_lowercase())
        .collect();
    (hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit())).then_some(hex)
}

impl FromStr for LinkCode {
    type Err = LinkCodeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        let rest = s
            .get(..SCHEME.len())
            .filter(|p| p.eq_ignore_ascii_case(SCHEME))
            .map(|_| &s[SCHEME.len()..])
            .ok_or(LinkCodeError::Scheme)?;
        let (before_frag, fragment) = rest.split_once('#').unwrap_or((rest, ""));
        let authority = before_frag.split('/').next().unwrap_or_default();
        if authority.is_empty() {
            return Err(LinkCodeError::Authority);
        }
        let (host, port) = split_host_port(authority);
        if host.is_empty() {
            return Err(LinkCodeError::Authority);
        }
        let port = match port {
            Some(0) => return Err(LinkCodeError::Port),
            Some(p) => p,
            // No port: plain host, `[v6]`, or a bare (unbracketed) IPv6 literal.
            None if !authority.contains(':')
                || authority.ends_with(']')
                || authority.matches(':').count() >= 2 =>
            {
                443
            }
            None => return Err(LinkCodeError::Port),
        };
        let mut key = None;
        let mut fp = None;
        for pair in fragment.split('&').filter(|p| !p.is_empty()) {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            match k {
                "k" => key = Some(v.to_owned()),
                "fp" if !v.is_empty() => {
                    fp = Some(normalize_fingerprint(v).ok_or(LinkCodeError::Fingerprint)?);
                }
                _ => {}
            }
        }
        let instance_key = key.ok_or(LinkCodeError::MissingKey)?;
        validate_instance_key(&instance_key).map_err(|_| LinkCodeError::InvalidKey)?;
        Ok(Self {
            host: host.to_ascii_lowercase(),
            port,
            instance_key,
            fingerprint: fp,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "Zm9vYmFyYmF6cXV4cXV1eGNvcmdlZ3JhdWx0Z2FycGx5";

    #[test]
    fn roundtrip_with_fingerprint() {
        let fp = "a".repeat(64);
        let code = LinkCode {
            host: "203.0.113.9".into(),
            port: 443,
            instance_key: KEY.into(),
            fingerprint: Some(fp.clone()),
        };
        let s = code.to_string();
        assert_eq!(
            s,
            format!("copper-cloud://203.0.113.9:443/#k={KEY}&fp={fp}")
        );
        assert_eq!(s.parse::<LinkCode>().unwrap(), code);
    }

    #[test]
    fn roundtrip_acme_ipv6() {
        let code = LinkCode {
            host: "2001:db8::1".into(),
            port: 8443,
            instance_key: KEY.into(),
            fingerprint: None,
        };
        let s = code.to_string();
        assert_eq!(s, format!("copper-cloud://[2001:db8::1]:8443/#k={KEY}"));
        assert_eq!(s.parse::<LinkCode>().unwrap(), code);
    }

    #[test]
    fn lenient_parsing() {
        let fp_colons = (0..32).map(|_| "AB").collect::<Vec<_>>().join(":");
        let c: LinkCode =
            format!("  COPPER-CLOUD://Cloud.Example.com/#fp={fp_colons}&k={KEY}&x=1 ")
                .parse()
                .unwrap();
        assert_eq!(c.host, "cloud.example.com");
        assert_eq!(c.port, 443);
        assert_eq!(c.fingerprint.as_deref(), Some("ab".repeat(32).as_str()));
    }

    #[test]
    fn rejects_bad_codes() {
        use LinkCodeError as E;
        let p = |s: &str| s.parse::<LinkCode>().unwrap_err();
        assert_eq!(p("https://x:1/#k=abc"), E::Scheme);
        assert_eq!(p("copper-cloud:///#k=abc"), E::Authority);
        assert_eq!(p("copper-cloud://h:0/#k=abc"), E::Port);
        assert_eq!(p("copper-cloud://h:x/#k=abc"), E::Port);
        assert_eq!(p("copper-cloud://h:1/"), E::MissingKey);
        assert_eq!(p("copper-cloud://h:1/#k=short"), E::InvalidKey);
        assert_eq!(
            p(&format!("copper-cloud://h:1/#k={KEY}&fp=zz")),
            E::Fingerprint
        );
    }
}
