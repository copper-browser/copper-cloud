//! The Canvas Cloud client against the in-process mock: probe, error mapping and TLS pinning
//! (no database needed; the proxy/bridge/admin tests live in `crates/copper-cloud/tests`).

#![allow(clippy::missing_panics_doc)]

#[path = "support/mock.rs"]
mod mock;

use std::path::PathBuf;
use std::time::Duration;

use copper_cloud_canvas_link::upstream::{Upstream, UpstreamError};
use copper_cloud_canvas_link::LinkSettings;
use time::OffsetDateTime;
use zeroize::Zeroizing;

fn settings(base: &str, key: &str, pin: Option<&str>) -> LinkSettings {
    LinkSettings {
        url: copper_cloud_canvas_link::settings::parse_base_url(base).unwrap(),
        app_key: Zeroizing::new(key.to_owned()),
        tls_fingerprint: pin.map(str::to_owned),
        enabled: true,
        linked_at: OffsetDateTime::now_utc(),
        updated_at: OffsetDateTime::now_utc(),
        updated_by: None,
    }
}

const T: Duration = Duration::from_secs(5);

#[tokio::test]
async fn probe_and_error_mapping() {
    copper_cloud_core::tls::install_default_provider();
    let m = mock::start().await;
    m.st.add_user("u1", "u1@example.com");
    m.st.add_canvas("u1", "One", "shared");

    let up = Upstream::new(&settings(&m.base, mock::APP_KEY, None)).unwrap();
    let probe = up.check(T).await.unwrap();
    assert_eq!(probe.version.as_deref(), Some("0.0.0-mock"));
    assert_eq!(probe.name.as_deref(), Some("canvas-cloud"));
    assert_eq!(probe.stats.users, 1);
    assert_eq!(probe.stats.canvases, 1);
    // The app key travels as a bearer token, never as X-Canvas-User.
    let seen = m.st.requests_for("/v1/apps/self/stats");
    assert_eq!(
        seen[0].headers["authorization"],
        format!("Bearer {}", mock::APP_KEY)
    );
    assert!(!seen[0].headers.contains_key("x-canvas-user"));

    // A wrong key: a Status error with the upstream code (the admin API turns it into 400).
    let bad = Upstream::new(&settings(&m.base, "cva_wrong_key_123456", None)).unwrap();
    let e = bad.check(T).await.unwrap_err();
    assert_eq!(e.status().map(|s| s.as_u16()), Some(401));
    assert_eq!(e.code(), Some("unauthorized"));
    assert!(e.to_string().contains("401"), "{e}");

    // Nothing listening: a transport error.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let down = Upstream::new(&settings(
        &format!("http://127.0.0.1:{port}"),
        mock::APP_KEY,
        None,
    ))
    .unwrap();
    let e = down.check(T).await.unwrap_err();
    assert!(matches!(e, UpstreamError::Transport(_)), "{e:?}");
    assert!(e.to_string().contains("unreachable"), "{e}");
}

fn temp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cc-link-tls-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test]
async fn tls_fingerprint_pinning() {
    copper_cloud_core::tls::install_default_provider();
    let dir = temp_dir();
    let (cert, key) = (dir.join("cert.pem"), dir.join("key.pem"));
    copper_cloud_core::tls::generate_self_signed(
        &["canvas.internal".to_owned(), "127.0.0.1".to_owned()],
        &cert,
        &key,
    )
    .unwrap();
    let fp = copper_cloud_core::tls::leaf_fingerprint(&cert).unwrap();
    let m = mock::start_tls(&cert, &key).await;
    assert!(m.base.starts_with("https://"));

    // Pinned to the right certificate: accepted (names and issuer do not matter).
    let pinned = Upstream::new(&settings(&m.base, mock::APP_KEY, Some(&fp))).unwrap();
    let probe = pinned.check(T).await.unwrap();
    assert_eq!(probe.version.as_deref(), Some("0.0.0-mock"));

    // Pinned to another certificate: refused.
    let wrong = Upstream::new(&settings(&m.base, mock::APP_KEY, Some(&"ab".repeat(32)))).unwrap();
    let e = wrong.check(T).await.unwrap_err();
    assert!(matches!(e, UpstreamError::Transport(_)), "{e:?}");
    assert!(e.to_string().contains("pinned fingerprint"), "{e}");

    // No pin: the web PKI does not know a self-signed certificate.
    let unpinned = Upstream::new(&settings(&m.base, mock::APP_KEY, None)).unwrap();
    let e = unpinned.check(T).await.unwrap_err();
    assert!(matches!(e, UpstreamError::Transport(_)), "{e:?}");

    // The WebSocket connector follows the same policy: pinned connects, wrong pin fails.
    for (up, ok) in [(&pinned, true), (&wrong, false)] {
        let mut req =
            tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(
                up.ws_url("/v1/canvases/c1/ws"),
            )
            .unwrap();
        req.headers_mut().insert(
            "authorization",
            format!("Bearer {}", mock::APP_KEY).parse().unwrap(),
        );
        m.st.add_user("ws-user", "ws@example.com");
        req.headers_mut()
            .insert("x-canvas-user", "ws-user".parse().unwrap());
        let r = tokio_tungstenite::connect_async_tls_with_config(
            req,
            None,
            true,
            Some(up.ws_connector()),
        )
        .await;
        assert_eq!(r.is_ok(), ok, "{:?}", r.err());
    }
    let _ = std::fs::remove_dir_all(dir);
}
