//! TLS serving (self-signed + pinned probe) and the CLI surface (init-config, tls-init,
//! link-code, version) exercised through the real binary.

mod common;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::Command;

use copper_cloud_core::config::{Config, ConfigSource, TlsMode};
use copper_cloud_core::link::LinkCode;

fn bin() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_copper-cloud"));
    // Never let the developer's environment leak into the CLI under test.
    for (k, _) in std::env::vars() {
        if k.starts_with("COPPER_CLOUD_") {
            c.env_remove(k);
        }
    }
    c
}

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "copper-cloud-test-{tag}-{}",
        copper_cloud_core::ids::random_token()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
#[allow(clippy::too_many_lines)] // one linear CLI scenario
fn cli_init_config_tls_init_link_code() {
    let dir = temp_dir("cli");
    let cfg_path = dir.join("copper-cloud.toml");
    let out = bin()
        .args(["init-config", "--write"])
        .arg(&cfg_path)
        .args(["--public-url", "203.0.113.10", "--listen", "0.0.0.0:443"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&cfg_path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    // Refuses to clobber an existing config (keys!) without --force.
    let again = bin()
        .args(["init-config", "--write"])
        .arg(&cfg_path)
        .output()
        .unwrap();
    assert!(!again.status.success());

    let out = bin()
        .arg("--config")
        .arg(&cfg_path)
        .arg("tls-init")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let cfg = Config::load(&ConfigSource {
        path: cfg_path.clone(),
        explicit: true,
    })
    .unwrap();
    let info = copper_cloud_core::tls::cert_info(&cfg.tls.cert_path).unwrap();
    assert!(
        info.names.contains(&"203.0.113.10".to_owned()),
        "IP SAN: {:?}",
        info.names
    );

    let out = bin()
        .arg("--config")
        .arg(&cfg_path)
        .arg("link-code")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    let code: LinkCode = text.trim().parse().unwrap();
    assert_eq!(code.host, "203.0.113.10");
    assert_eq!(code.port, 443);
    assert_eq!(code.instance_key, cfg.instance_key);
    assert_eq!(code.fingerprint.as_deref(), Some(info.fingerprint.as_str()));
    assert!(text.starts_with("copper-cloud://203.0.113.10:443/#k="));

    // tls-init is idempotent (keeps the fingerprint) unless --force.
    bin()
        .arg("--config")
        .arg(&cfg_path)
        .arg("tls-init")
        .output()
        .unwrap();
    assert_eq!(
        copper_cloud_core::tls::leaf_fingerprint(&cfg.tls.cert_path).unwrap(),
        info.fingerprint
    );

    // Env overrides reach the CLI: ACME mode drops fp.
    let out = bin()
        .arg("--config")
        .arg(&cfg_path)
        .arg("link-code")
        .env("COPPER_CLOUD_TLS__MODE", "acme")
        .env("COPPER_CLOUD_TLS__DOMAIN", "cloud.example.com")
        .env("COPPER_CLOUD_PUBLIC_URL", "cloud.example.com")
        .output()
        .unwrap();
    let code: LinkCode = String::from_utf8(out.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(code.host, "cloud.example.com");
    assert_eq!(code.fingerprint, None);

    let out = bin().arg("version").output().unwrap();
    assert!(String::from_utf8(out.stdout)
        .unwrap()
        .starts_with("copper-cloud 0."));

    // A missing explicit config is an error, not a silent default.
    let out = bin()
        .args(["--config", "/nonexistent/copper-cloud.toml", "link-code"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn self_signed_https_with_pinned_probe() {
    copper_cloud_core::tls::install_default_provider();
    let dir = temp_dir("tls");
    let mut cfg = Config::for_tests(&common::database_url());
    cfg.tls.mode = TlsMode::SelfSigned;
    cfg.tls.cert_path = dir.join("cert.pem");
    cfg.tls.key_path = dir.join("key.pem");
    cfg.public_url = "127.0.0.1:8443".into();
    let pool = copper_cloud_core::db::connect_lazy(&cfg).unwrap();
    let state = copper_cloud_core::state::AppState::new(pool, cfg.clone());
    let app = copper_cloud::build_app(state);

    let handle: axum_server::Handle<SocketAddr> = axum_server::Handle::new();
    let server_cfg = cfg.clone();
    let h = handle.clone();
    let server =
        tokio::spawn(async move { copper_cloud_core::tls::serve(app, &server_cfg, h).await });
    let addr = handle.listening().await.expect("server bound");

    // The certificate was generated on first start with the public IP as a SAN.
    let fp = copper_cloud_core::tls::leaf_fingerprint(&cfg.tls.cert_path).unwrap();
    let detail = copper_cloud::doctor::probe_healthz_at(&cfg, addr)
        .await
        .unwrap();
    assert!(detail.contains("fingerprint matches"), "{detail}");

    // A different pinned fingerprint must fail the handshake.
    let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    let err = copper_cloud::doctor::tls_connect(tcp, "localhost", Some("0".repeat(64)))
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("handshake"), "{err:#}");

    // The observed fingerprint equals the link-code fp.
    let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (_, seen) = copper_cloud::doctor::tls_connect(tcp, "localhost", None)
        .await
        .unwrap();
    assert_eq!(seen, fp);
    let code = copper_cloud_core::tls::link_code(&cfg).unwrap();
    assert_eq!(code.fingerprint.as_deref(), Some(fp.as_str()));

    handle.graceful_shutdown(Some(std::time::Duration::from_secs(1)));
    server.await.unwrap().unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}
