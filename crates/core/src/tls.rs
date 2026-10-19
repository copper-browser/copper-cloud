//! TLS: self-signed certificates (rcgen, pinned by fingerprint), ACME (rustls-acme,
//! TLS-ALPN-01) and plain HTTP behind a proxy. All rustls usage goes through the `ring`
//! provider.
//!
//! Hybrid (`tls.mode = "acme"` + `tls.fallback_self_signed = true`): the ACME certificate for
//! TLS clients that ask for `tls.domain` by SNI, the existing self-signed certificate for
//! everyone else (raw IP, no SNI) — see [`HybridResolver`]. Old Coppers keep their pinned
//! IP + fingerprint link ([`link_code`]); onboarding links use the domain without a
//! fingerprint ([`onboarding_link`]).

use std::fs;
use std::io::Write as _;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context as _};
use axum::Router;
use axum_server::tls_rustls::RustlsConfig;
use axum_server::Handle;
use futures::StreamExt as _;
use rustls::crypto::CryptoProvider;
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use rustls::ServerConfig;

use crate::config::{Config, TlsMode};

/// Validity of generated self-signed certificates. Clients pin the exact leaf fingerprint,
/// so a long lifetime avoids silently breaking every linked Copper on renewal.
pub const SELF_SIGNED_DAYS: i64 = 3650;

/// The process-wide rustls crypto provider (`ring`).
pub fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Install `ring` as the rustls default provider (idempotent).
pub fn install_default_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// SAN list for a self-signed certificate: the public host (+ `tls.domain`), plus localhost.
pub fn self_signed_names(cfg: &Config) -> Vec<String> {
    let mut names = vec![cfg.public_host().to_owned()];
    if let Some(d) = &cfg.tls.domain {
        names.push(d.clone());
    }
    for extra in ["localhost", "127.0.0.1"] {
        names.push(extra.to_owned());
    }
    names.dedup();
    let mut seen = std::collections::HashSet::new();
    names.retain(|n| seen.insert(n.clone()));
    names
}

/// Generate a self-signed ECDSA P-256 certificate for `names` (IP literals become IP SANs).
/// Writes the key with mode 0600 and the cert with 0644, creating parent dirs.
pub fn generate_self_signed(
    names: &[String],
    cert_path: &Path,
    key_path: &Path,
) -> anyhow::Result<()> {
    let primary = names
        .first()
        .context("at least one certificate name is required")?;
    let mut params = rcgen::CertificateParams::new(names.to_vec()).context("certificate names")?;
    params.distinguished_name = rcgen::DistinguishedName::new();
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, primary.as_str());
    params
        .distinguished_name
        .push(rcgen::DnType::OrganizationName, "copper-cloud");
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::days(1);
    params.not_after = now + time::Duration::days(SELF_SIGNED_DAYS);
    params.serial_number = Some(rcgen::SerialNumber::from_slice(
        &crate::ids::random_bytes::<16>(),
    ));
    params.key_usages = vec![
        rcgen::KeyUsagePurpose::DigitalSignature,
        rcgen::KeyUsagePurpose::KeyEncipherment,
    ];
    params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
    params.is_ca = rcgen::IsCa::NoCa;
    let key = rcgen::KeyPair::generate().context("generating key pair")?;
    let cert = params.self_signed(&key).context("signing certificate")?;
    write_file(key_path, key.serialize_pem().as_bytes(), 0o600)?;
    write_file(cert_path, cert.pem().as_bytes(), 0o644)?;
    Ok(())
}

fn write_file(path: &Path, contents: &[u8], mode: u32) -> anyhow::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(mode);
        }
        let mut f = opts
            .open(&tmp)
            .with_context(|| format!("writing {}", tmp.display()))?;
        f.write_all(contents)?;
        f.sync_all()?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    let _ = mode;
    fs::rename(&tmp, path).with_context(|| format!("installing {}", path.display()))?;
    Ok(())
}

/// Whether this config serves the self-signed certificate (`self-signed` mode, or `acme` with
/// `fallback_self_signed`).
pub fn serves_self_signed(cfg: &Config) -> bool {
    match cfg.tls.mode {
        TlsMode::SelfSigned => true,
        TlsMode::Acme => cfg.tls.fallback_self_signed,
        TlsMode::Off => false,
    }
}

/// When the self-signed certificate is served (see [`serves_self_signed`]): create it if
/// either file is missing (an existing certificate is never replaced).
/// Returns `true` when a new certificate was generated.
pub fn ensure_self_signed(cfg: &Config) -> anyhow::Result<bool> {
    if !serves_self_signed(cfg) {
        return Ok(false);
    }
    if cfg.tls.cert_path.exists() && cfg.tls.key_path.exists() {
        return Ok(false);
    }
    generate_self_signed(
        &self_signed_names(cfg),
        &cfg.tls.cert_path,
        &cfg.tls.key_path,
    )?;
    tracing::info!(cert = %cfg.tls.cert_path.display(), "generated self-signed certificate");
    Ok(true)
}

/// First certificate (the leaf) in a PEM file, DER-encoded.
pub fn read_leaf_der(cert_path: &Path) -> anyhow::Result<CertificateDer<'static>> {
    CertificateDer::pem_file_iter(cert_path)
        .with_context(|| format!("reading {}", cert_path.display()))?
        .next()
        .with_context(|| format!("no certificate in {}", cert_path.display()))?
        .with_context(|| format!("parsing {}", cert_path.display()))
}

/// Lower-case hex SHA-256 of the leaf certificate DER (the link-code `fp`).
pub fn leaf_fingerprint(cert_path: &Path) -> anyhow::Result<String> {
    Ok(crate::ids::sha256_hex(&read_leaf_der(cert_path)?))
}

/// Facts about a certificate file (for `doctor`).
#[derive(Debug, Clone)]
pub struct CertInfo {
    pub fingerprint: String,
    pub subject: String,
    pub names: Vec<String>,
    pub not_before: time::OffsetDateTime,
    pub not_after: time::OffsetDateTime,
}

pub fn cert_info(cert_path: &Path) -> anyhow::Result<CertInfo> {
    let der = read_leaf_der(cert_path)?;
    let (_, cert) = x509_parser::parse_x509_certificate(&der)
        .map_err(|e| anyhow::anyhow!("parsing certificate: {e}"))?;
    let mut names = Vec::new();
    if let Ok(Some(san)) = cert.subject_alternative_name() {
        for n in &san.value.general_names {
            match n {
                x509_parser::extensions::GeneralName::DNSName(d) => names.push((*d).to_owned()),
                x509_parser::extensions::GeneralName::IPAddress(b) => match b.len() {
                    4 => names.push(IpAddr::from(<[u8; 4]>::try_from(*b)?).to_string()),
                    16 => names.push(IpAddr::from(<[u8; 16]>::try_from(*b)?).to_string()),
                    _ => {}
                },
                _ => {}
            }
        }
    }
    let ts = |t: x509_parser::time::ASN1Time| {
        time::OffsetDateTime::from_unix_timestamp(t.timestamp())
            .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
    };
    Ok(CertInfo {
        fingerprint: crate::ids::sha256_hex(&der),
        subject: cert.subject().to_string(),
        names,
        not_before: ts(cert.validity().not_before),
        not_after: ts(cert.validity().not_after),
    })
}

/// rustls server config from PEM cert chain + key, ALPN h2 + http/1.1.
pub fn server_config_from_files(
    cert_path: &Path,
    key_path: &Path,
) -> anyhow::Result<Arc<ServerConfig>> {
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(cert_path)
        .with_context(|| format!("reading {}", cert_path.display()))?
        .collect::<Result<_, _>>()
        .with_context(|| format!("parsing {}", cert_path.display()))?;
    if certs.is_empty() {
        bail!("no certificates in {}", cert_path.display());
    }
    let key = PrivateKeyDer::from_pem_file(key_path)
        .with_context(|| format!("reading private key {}", key_path.display()))?;
    let mut config = ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .context("certificate/key mismatch or unsupported key")?;
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

/// The link code for this instance: `public_url` + `fp` from the cert file unless ACME/off.
/// With hybrid TLS this stays the legacy `public_url` (e.g. the raw IP) + self-signed `fp`
/// link, which every Copper linked before the domain was added understands.
pub fn link_code(cfg: &Config) -> anyhow::Result<crate::link::LinkCode> {
    let fingerprint = match cfg.tls.mode {
        TlsMode::SelfSigned => Some(leaf_fingerprint(&cfg.tls.cert_path)?),
        TlsMode::Acme if cfg.tls.fallback_self_signed => {
            Some(leaf_fingerprint(&cfg.tls.cert_path)?)
        }
        TlsMode::Acme => None,
        // Behind a proxy: pin whatever cert the operator configured, if present.
        TlsMode::Off => leaf_fingerprint(&cfg.tls.cert_path).ok(),
    };
    Ok(crate::link::LinkCode {
        host: cfg.public_host().to_owned(),
        port: cfg.public_port(),
        instance_key: cfg.instance_key.clone(),
        fingerprint,
    })
}

/// The link onboarding hands out (SSO join link, `/start`, agent pairing links): with hybrid
/// TLS `tls.domain` on the public port without a fingerprint (publicly trusted certificate);
/// otherwise the same as [`link_code`].
pub fn onboarding_link(cfg: &Config) -> anyhow::Result<crate::link::LinkCode> {
    match (&cfg.tls.domain, cfg.tls.mode) {
        (Some(domain), TlsMode::Acme) if cfg.tls.fallback_self_signed => {
            Ok(crate::link::LinkCode {
                host: domain.clone(),
                port: cfg.public_port(),
                instance_key: cfg.instance_key.clone(),
                fingerprint: None,
            })
        }
        _ => link_code(cfg),
    }
}

/// A certificate chain + key from PEM files, ready for a [`ResolvesServerCert`].
pub fn certified_key_from_files(
    cert_path: &Path,
    key_path: &Path,
) -> anyhow::Result<Arc<CertifiedKey>> {
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(cert_path)
        .with_context(|| format!("reading {}", cert_path.display()))?
        .collect::<Result<_, _>>()
        .with_context(|| format!("parsing {}", cert_path.display()))?;
    if certs.is_empty() {
        bail!("no certificates in {}", cert_path.display());
    }
    let key = PrivateKeyDer::from_pem_file(key_path)
        .with_context(|| format!("reading private key {}", key_path.display()))?;
    let signing_key = provider()
        .key_provider
        .load_private_key(key)
        .context("unsupported private key")?;
    Ok(Arc::new(CertifiedKey::new(certs, signing_key)))
}

/// Hybrid TLS certificate choice: `primary` (the ACME resolver) for clients that ask for
/// `domain` by SNI, `fallback` (the self-signed certificate) for everyone else — raw-IP
/// clients send no SNI. Until the first ACME certificate exists the domain gets the fallback
/// too, rather than a failed handshake. TLS-ALPN-01 challenge handshakes never get here: the
/// ACME acceptor answers them itself.
#[derive(Debug)]
pub struct HybridResolver {
    domain: String,
    primary: Arc<dyn ResolvesServerCert>,
    fallback: Arc<CertifiedKey>,
}

impl HybridResolver {
    pub fn new(
        domain: &str,
        primary: Arc<dyn ResolvesServerCert>,
        fallback: Arc<CertifiedKey>,
    ) -> Self {
        Self {
            domain: domain.trim_end_matches('.').to_ascii_lowercase(),
            primary,
            fallback,
        }
    }

    /// Whether a client hello with this SNI gets the primary (ACME) certificate.
    pub fn wants_primary(&self, sni: Option<&str>) -> bool {
        sni.is_some_and(|name| {
            name.trim_end_matches('.')
                .eq_ignore_ascii_case(&self.domain)
        })
    }
}

impl ResolvesServerCert for HybridResolver {
    fn resolve(&self, client_hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        if self.wants_primary(client_hello.server_name()) {
            self.primary
                .resolve(client_hello)
                .or_else(|| Some(self.fallback.clone()))
        } else {
            Some(self.fallback.clone())
        }
    }
}

/// Serve `app` on `cfg.listen` per `cfg.tls.mode` until `handle` shuts it down.
pub async fn serve(app: Router, cfg: &Config, handle: Handle<SocketAddr>) -> anyhow::Result<()> {
    let make_service = app.into_make_service_with_connect_info::<SocketAddr>();
    match cfg.tls.mode {
        TlsMode::Off => {
            tracing::warn!(listen = %cfg.listen, "TLS is OFF: plain HTTP (only behind a TLS-terminating proxy)");
            axum_server::bind(cfg.listen)
                .handle(handle)
                .serve(make_service)
                .await
                .context("http server")?;
        }
        TlsMode::SelfSigned => {
            ensure_self_signed(cfg)?;
            let config = server_config_from_files(&cfg.tls.cert_path, &cfg.tls.key_path)?;
            tracing::info!(
                listen = %cfg.listen,
                fingerprint = %leaf_fingerprint(&cfg.tls.cert_path)?,
                "serving HTTPS (self-signed)"
            );
            axum_server::bind_rustls(cfg.listen, RustlsConfig::from_config(config))
                .handle(handle)
                .serve(make_service)
                .await
                .context("https server")?;
        }
        TlsMode::Acme => {
            let acceptor = acme_acceptor(cfg)?;
            if cfg.tls.fallback_self_signed {
                tracing::info!(
                    listen = %cfg.listen,
                    domain = ?cfg.tls.domain,
                    fingerprint = %leaf_fingerprint(&cfg.tls.cert_path)?,
                    "serving HTTPS (ACME for the domain, self-signed for everything else)"
                );
            } else {
                tracing::info!(listen = %cfg.listen, domain = ?cfg.tls.domain, "serving HTTPS (ACME)");
            }
            axum_server::bind(cfg.listen)
                .acceptor(acceptor)
                .handle(handle)
                .serve(make_service)
                .await
                .context("https server (acme)")?;
        }
    }
    Ok(())
}

/// Build the ACME acceptor and spawn the certificate order/renewal driver.
fn acme_acceptor(cfg: &Config) -> anyhow::Result<rustls_acme::axum::AxumAcceptor> {
    let domain = cfg
        .tls
        .domain
        .clone()
        .context("tls.domain is required for ACME")?;
    let cache_dir: PathBuf = cfg.tls.acme_cache_dir.clone();
    fs::create_dir_all(&cache_dir)
        .with_context(|| format!("creating ACME cache dir {}", cache_dir.display()))?;
    let mut acme = rustls_acme::AcmeConfig::new([domain.as_str()])
        .cache(rustls_acme::caches::DirCache::new(cache_dir))
        .directory_lets_encrypt(!cfg.tls.acme_staging);
    if let Some(email) = &cfg.tls.acme_email {
        acme = acme.contact_push(format!("mailto:{email}"));
    }
    let mut state = acme.state();
    let resolver: Arc<dyn ResolvesServerCert> = if cfg.tls.fallback_self_signed {
        ensure_self_signed(cfg)?;
        let fallback = certified_key_from_files(&cfg.tls.cert_path, &cfg.tls.key_path)?;
        Arc::new(HybridResolver::new(&domain, state.resolver(), fallback))
    } else {
        state.resolver()
    };
    let mut server_config = ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_cert_resolver(resolver);
    server_config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    let acceptor = state.axum_acceptor(Arc::new(server_config));
    tokio::spawn(async move {
        while let Some(event) = state.next().await {
            match event {
                Ok(ok) => tracing::info!(event = ?ok, "acme"),
                Err(err) => tracing::error!(error = ?err, "acme"),
            }
        }
    });
    Ok(acceptor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_signed_roundtrip() {
        let dir = std::env::temp_dir().join(format!("cc-tls-{}", crate::ids::random_token()));
        let (cert, key) = (dir.join("tls/cert.pem"), dir.join("tls/key.pem"));
        let names = vec!["203.0.113.5".to_owned(), "localhost".to_owned()];
        generate_self_signed(&names, &cert, &key).unwrap();
        let info = cert_info(&cert).unwrap();
        assert_eq!(info.fingerprint, leaf_fingerprint(&cert).unwrap());
        assert_eq!(info.fingerprint.len(), 64);
        assert!(info.names.contains(&"203.0.113.5".to_owned()));
        assert!(info.names.contains(&"localhost".to_owned()));
        assert!(info.not_after > time::OffsetDateTime::now_utc() + time::Duration::days(3000));
        server_config_from_files(&cert, &key).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(&key).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        fs::remove_dir_all(dir).unwrap();
    }

    /// Always the same certificate (stands in for the ACME resolver).
    #[derive(Debug)]
    struct Fixed(Arc<CertifiedKey>);

    impl ResolvesServerCert for Fixed {
        fn resolve(&self, _: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
            Some(self.0.clone())
        }
    }

    /// An ACME resolver before the first certificate arrived.
    #[derive(Debug)]
    struct Nothing;

    impl ResolvesServerCert for Nothing {
        fn resolve(&self, _: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
            None
        }
    }

    /// Accepts any server certificate (the test only looks at which one was presented).
    #[derive(Debug)]
    struct AcceptAny;

    impl rustls::client::danger::ServerCertVerifier for AcceptAny {
        fn verify_server_cert(
            &self,
            _: &CertificateDer<'_>,
            _: &[CertificateDer<'_>],
            _: &rustls::pki_types::ServerName<'_>,
            _: &[u8],
            _: rustls::pki_types::UnixTime,
        ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            _: &[u8],
            _: &CertificateDer<'_>,
            _: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }

        fn verify_tls13_signature(
            &self,
            _: &[u8],
            _: &CertificateDer<'_>,
            _: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }

        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            provider()
                .signature_verification_algorithms
                .supported_schemes()
        }
    }

    /// In-memory handshake; returns the leaf certificate the server presented.
    fn presented_leaf(server: &Arc<ServerConfig>, name: &str) -> CertificateDer<'static> {
        let client_config = rustls::ClientConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAny))
            .with_no_client_auth();
        let server_name = rustls::pki_types::ServerName::try_from(name.to_owned()).unwrap();
        let mut client =
            rustls::ClientConnection::new(Arc::new(client_config), server_name).unwrap();
        let mut srv = rustls::ServerConnection::new(server.clone()).unwrap();
        for _ in 0..10 {
            let mut buf = Vec::new();
            client.write_tls(&mut buf).unwrap();
            srv.read_tls(&mut buf.as_slice()).unwrap();
            srv.process_new_packets().unwrap();
            let mut buf = Vec::new();
            srv.write_tls(&mut buf).unwrap();
            client.read_tls(&mut buf.as_slice()).unwrap();
            client.process_new_packets().unwrap();
            if !client.is_handshaking() && !srv.is_handshaking() {
                break;
            }
        }
        assert!(
            !client.is_handshaking(),
            "handshake with {name} did not finish"
        );
        client.peer_certificates().unwrap()[0].clone().into_owned()
    }

    #[test]
    fn hybrid_resolver_picks_by_sni() {
        let dir = std::env::temp_dir().join(format!("cc-hybrid-{}", crate::ids::random_token()));
        let pair = |stem: &str, names: &[&str]| {
            let (cert, key) = (
                dir.join(format!("{stem}.pem")),
                dir.join(format!("{stem}.key")),
            );
            let names: Vec<String> = names.iter().map(|n| (*n).to_owned()).collect();
            generate_self_signed(&names, &cert, &key).unwrap();
            certified_key_from_files(&cert, &key).unwrap()
        };
        let acme = pair("acme", &["copper.example.com"]);
        let pinned = pair("pinned", &["203.0.113.10", "localhost"]);
        let resolver = HybridResolver::new(
            "Copper.Example.COM.",
            Arc::new(Fixed(acme.clone())),
            pinned.clone(),
        );

        assert!(resolver.wants_primary(Some("copper.example.com")));
        assert!(resolver.wants_primary(Some("COPPER.example.com.")));
        for other in [
            None,
            Some("203.0.113.10"),
            Some("localhost"),
            Some("evil.example.com"),
        ] {
            assert!(!resolver.wants_primary(other), "{other:?}");
        }

        let mut config = ServerConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_cert_resolver(Arc::new(resolver));
        config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        let config = Arc::new(config);
        assert_eq!(presented_leaf(&config, "copper.example.com"), acme.cert[0]);
        // IP literals are never sent as SNI: the pinned self-signed certificate.
        assert_eq!(presented_leaf(&config, "203.0.113.10"), pinned.cert[0]);
        assert_eq!(presented_leaf(&config, "other.example.com"), pinned.cert[0]);

        // No ACME certificate yet: the domain gets the fallback instead of a failed handshake.
        let config = ServerConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_cert_resolver(Arc::new(HybridResolver::new(
                "copper.example.com",
                Arc::new(Nothing),
                pinned.clone(),
            )));
        assert_eq!(
            presented_leaf(&Arc::new(config), "copper.example.com"),
            pinned.cert[0]
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn hybrid_links() {
        let dir =
            std::env::temp_dir().join(format!("cc-hybrid-link-{}", crate::ids::random_token()));
        let mut cfg = Config::for_tests("postgres://localhost/x");
        cfg.public_url = "203.0.113.10".into();
        cfg.listen = "0.0.0.0:443".parse().unwrap();
        cfg.tls.mode = TlsMode::Acme;
        cfg.tls.domain = Some("copper.example.com".into());
        cfg.tls.cert_path = dir.join("cert.pem");
        cfg.tls.key_path = dir.join("key.pem");

        // Plain ACME: no fingerprint, the onboarding link is the link code.
        assert!(!serves_self_signed(&cfg));
        assert!(!ensure_self_signed(&cfg).unwrap());
        assert_eq!(link_code(&cfg).unwrap().fingerprint, None);
        assert_eq!(onboarding_link(&cfg).unwrap(), link_code(&cfg).unwrap());

        // Hybrid: legacy IP + fp link; onboarding on the domain without fp.
        cfg.tls.fallback_self_signed = true;
        assert!(ensure_self_signed(&cfg).unwrap());
        let fp = leaf_fingerprint(&cfg.tls.cert_path).unwrap();
        assert!(!ensure_self_signed(&cfg).unwrap(), "never regenerated");
        assert_eq!(leaf_fingerprint(&cfg.tls.cert_path).unwrap(), fp);
        let legacy = link_code(&cfg).unwrap();
        assert_eq!(legacy.host, "203.0.113.10");
        assert_eq!(legacy.port, 443);
        assert_eq!(legacy.fingerprint.as_deref(), Some(fp.as_str()));
        let onboarding = onboarding_link(&cfg).unwrap();
        assert_eq!(onboarding.host, "copper.example.com");
        assert_eq!(onboarding.port, 443);
        assert_eq!(onboarding.fingerprint, None);
        assert_eq!(
            onboarding.sso_link("google"),
            "copper-cloud://copper.example.com:443/#sso=google"
        );
        assert_eq!(
            onboarding.https_url("/start"),
            "https://copper.example.com/start"
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
