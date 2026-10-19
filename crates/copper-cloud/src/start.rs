//! `GET /start` — the onboarding page admins share once single sign-on is set up: install
//! Copper, open the join link in it, continue with the company account. Server-rendered,
//! ungated, no external assets (one inline script, pinned by hash in the CSP, for the copy
//! button). Without an enabled provider it politely says SSO is not set up yet.

use std::sync::OnceLock;

use axum::extract::State;
use axum::http::{header, HeaderValue};
use axum::response::{Html, IntoResponse, Response};
use copper_cloud_core::error::ApiResult;
use copper_cloud_core::state::SharedState;
use copper_cloud_core::{ids, sso};

pub const MAC_DOWNLOAD: &str =
    "https://github.com/copper-browser/Copper/releases/latest/download/copper-macos-arm64.zip";
pub const MAC_BREW: &str = "brew install --cask copper-browser/copper/copper";
pub const LINUX_INSTALL: &str =
    "curl -fsSL https://github.com/copper-browser/copper-linux/releases/latest/download/install.sh | sh";

const COPY_SCRIPT: &str = "(function(){var b=document.getElementById('copy'),i=document.getElementById('join-link');if(!b||!i)return;b.hidden=false;b.addEventListener('click',function(){i.select();var done=function(){b.textContent='Copied';setTimeout(function(){b.textContent='Copy'},2000)};if(navigator.clipboard){navigator.clipboard.writeText(i.value).then(done,function(){document.execCommand('copy');done()})}else{document.execCommand('copy');done()}})})();";

const STYLE: &str = "\
:root{color-scheme:light dark;--bg:#faf8f5;--ink:#1d1a17;--muted:#655c53;--rule:#e4ddd3;--well:#f1ece5;--copper:#a5501f;--on-copper:#fff}\
@media (prefers-color-scheme:dark){:root{--bg:#161412;--ink:#f1ece6;--muted:#a9a097;--rule:#342e29;--well:#221e1b;--copper:#e3925f;--on-copper:#1d1a17}}\
*{box-sizing:border-box}\
body{margin:0;background:var(--bg);color:var(--ink);font:16px/1.55 system-ui,-apple-system,'Segoe UI',sans-serif}\
main{max-width:38rem;margin:0 auto;padding:3.5rem 1.25rem 4rem}\
.eyebrow{margin:0;color:var(--copper);font-size:.8rem;font-weight:600;letter-spacing:.08em;text-transform:uppercase}\
h1{margin:.35rem 0 .75rem;font-size:clamp(1.6rem,4vw,2.1rem);line-height:1.2;text-wrap:balance}\
.lede{margin:0 0 2.25rem;color:var(--muted)}\
ol{list-style:none;margin:0;padding:0;counter-reset:step}\
ol>li{position:relative;counter-increment:step;padding:0 0 2rem 3.25rem}\
ol>li::before{content:counter(step);position:absolute;left:0;top:0;width:2.1rem;height:2.1rem;border-radius:50%;display:grid;place-items:center;background:var(--copper);color:var(--on-copper);font-weight:700}\
ol>li:not(:last-child)::after{content:'';position:absolute;left:1.02rem;top:2.4rem;bottom:.4rem;border-left:2px solid var(--rule)}\
h2{margin:.2rem 0 .5rem;font-size:1.15rem}\
p{margin:.5rem 0}\
.muted{color:var(--muted);font-size:.92rem}\
code,pre,input{font:.88rem/1.5 ui-monospace,SFMono-Regular,Menlo,Consolas,monospace}\
pre{margin:.4rem 0 .9rem;padding:.65rem .8rem;background:var(--well);border:1px solid var(--rule);border-radius:.5rem;overflow-x:auto;white-space:pre}\
a{color:var(--copper)}\
a:focus-visible,button:focus-visible,input:focus-visible{outline:3px solid var(--copper);outline-offset:2px}\
.button{display:inline-block;margin:.35rem 0 .6rem;padding:.7rem 1.2rem;border-radius:.55rem;background:var(--copper);color:var(--on-copper);font-weight:600;text-decoration:none}\
.copy{display:flex;gap:.5rem;margin:.4rem 0}\
.copy input{flex:1;min-width:0;padding:.55rem .7rem;color:var(--ink);background:var(--well);border:1px solid var(--rule);border-radius:.5rem}\
.copy button{padding:.55rem .9rem;border:1px solid var(--rule);border-radius:.5rem;background:transparent;color:var(--ink);font:inherit;cursor:pointer}\
.notice{padding:1rem 1.1rem;border:1px solid var(--rule);border-radius:.6rem;background:var(--well)}";

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn percent_encode_component(value: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(value.len() * 3);
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// `copper://cloud/join?code=<percent-encoded join link>` — the deep link Copper handles.
pub fn open_in_copper_href(join_link: &str) -> String {
    format!(
        "copper://cloud/join?code={}",
        percent_encode_component(join_link)
    )
}

/// `@a.com` / `@a.com or @b.com` for the copy.
fn domain_phrase(domains: &[String]) -> String {
    let list: Vec<String> = domains.iter().map(|d| format!("@{d}")).collect();
    list.join(" or ")
}

fn csp() -> &'static str {
    static CSP: OnceLock<String> = OnceLock::new();
    CSP.get_or_init(|| {
        format!(
            "default-src 'none'; style-src 'unsafe-inline'; script-src 'sha256-{}'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
            ids::b64_std(&ids::sha256(COPY_SCRIPT.as_bytes()))
        )
    })
}

fn page(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
<meta name=\"robots\" content=\"noindex\"><title>{title}</title><style>{STYLE}</style></head>\
<body><main>{body}</main><script>{COPY_SCRIPT}</script></body></html>",
        title = escape_html(title),
    )
}

/// "Join Acme on Copper Cloud", or just "Join Copper Cloud" under the default name.
fn join_title(instance: &str) -> String {
    if instance == sso::DEFAULT_INSTANCE_NAME {
        format!("Join {instance}")
    } else {
        format!("Join {instance} on Copper Cloud")
    }
}

/// The page for an enabled provider.
fn ready_page(instance: &str, provider_name: &str, domains: &[String], join_link: &str) -> String {
    let title = join_title(instance);
    let heading = escape_html(&title);
    let provider = escape_html(provider_name);
    let account = escape_html(&domain_phrase(domains));
    let link = escape_html(join_link);
    let href = escape_html(&open_in_copper_href(join_link));
    let body = format!(
        "<p class=\"eyebrow\">Copper Cloud</p>\
<h1>{heading}</h1>\
<p class=\"lede\">Three steps, no new password: you sign in with your {account} account and \
your tabs, bookmarks and history sync across your Coppers.</p>\
<ol>\
<li><h2>Install Copper</h2>\
<p><strong>Mac</strong> (Apple silicon): <a href=\"{MAC_DOWNLOAD}\">download Copper for Mac</a>, \
or with Homebrew:</p><pre><code>{brew}</code></pre>\
<p><strong>Linux</strong>:</p><pre><code>{linux}</code></pre>\
<p class=\"muted\">Already have Copper? Go to step 2.</p></li>\
<li><h2>Open in Copper</h2>\
<a class=\"button\" href=\"{href}\">Open in Copper</a>\
<p class=\"muted\">Nothing happened? Copy this join link and paste it in Copper under \
Settings › Cloud:</p>\
<div class=\"copy\"><input id=\"join-link\" readonly value=\"{link}\" \
aria-label=\"Join link\"><button id=\"copy\" type=\"button\" hidden>Copy</button></div></li>\
<li><h2>Continue with {provider}</h2>\
<p>Copper opens your browser. Choose your {account} account — that's it, you're in.</p></li>\
</ol>",
        brew = escape_html(MAC_BREW),
        linux = escape_html(LINUX_INSTALL),
    );
    page(&title, &body)
}

/// The page while no provider is enabled.
fn not_ready_page(instance: &str) -> String {
    let instance = escape_html(instance);
    let body = format!(
        "<p class=\"eyebrow\">Copper Cloud</p>\
<h1>{instance} isn't ready for sign-in yet</h1>\
<div class=\"notice\"><p>Single sign-on has not been set up on this Copper Cloud yet, so there \
is nothing to join here for now.</p><p class=\"muted\">If someone sent you this page, let them \
know — an administrator turns it on in the admin portal under Single sign-on.</p></div>"
    );
    page(&format!("{instance} on Copper Cloud"), &body)
}

/// `GET /start`.
pub async fn start(State(state): State<SharedState>) -> ApiResult<Response> {
    let instance = sso::instance_name(&state.db).await?;
    let html = match sso::primary_provider(&state.db).await? {
        Some(p) => ready_page(
            &instance,
            &p.name,
            &p.domains,
            &sso::join_link(&state.cfg, &p.id),
        ),
        None => not_ready_page(&instance),
    };
    let mut response = Html(html).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_str(csp()).expect("CSP is ASCII"),
    );
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deep_link_encodes_the_join_link() {
        assert_eq!(
            open_in_copper_href("copper-cloud://h:443/#sso=google&fp=ab"),
            "copper://cloud/join?code=copper-cloud%3A%2F%2Fh%3A443%2F%23sso%3Dgoogle%26fp%3Dab"
        );
    }

    #[test]
    fn pages_escape_and_pin_the_script() {
        let html = ready_page(
            "<Acme & Co>",
            "Google",
            &["example.com".to_owned(), "b.io".to_owned()],
            "copper-cloud://h:443/#sso=google",
        );
        assert!(html.contains("Join &lt;Acme &amp; Co&gt; on Copper Cloud"));
        assert!(!html.contains("<Acme"));
        assert!(html.contains("@example.com or @b.io"));
        assert!(html.contains(MAC_DOWNLOAD) && html.contains("copper-browser/copper/copper"));
        assert!(html.contains("copper-linux/releases/latest/download/install.sh | sh"));
        assert!(html.contains("Continue with Google"));
        assert!(html.contains(&format!("<script>{COPY_SCRIPT}</script>")));
        assert!(csp().contains(&ids::b64_std(&ids::sha256(COPY_SCRIPT.as_bytes()))));
        let html = not_ready_page("Copper Cloud");
        assert!(html.contains("isn't ready for sign-in yet"));
        assert!(
            ready_page("Copper Cloud", "Google", &[], "x").contains("<h1>Join Copper Cloud</h1>")
        );
        assert!(!html.contains("Open in Copper"));
    }
}
