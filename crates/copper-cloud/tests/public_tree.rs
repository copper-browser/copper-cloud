//! Public-release hygiene for the source tree (no database needed):
//!
//! * generic checks that no deployment-specific infrastructure leaks into a tracked file —
//!   opaque cloud account ids, routable IPv4 addresses, and internal / environment-tier
//!   hostnames in URLs; docs and examples use reserved placeholders instead (RFC 5737
//!   addresses, RFC 2606 domains). These are heuristics: the authoritative check for exact
//!   private values is a separate, independent all-history scanner that is not part of this
//!   repository, so nothing here names a real value;
//! * the public install/source URLs agree everywhere (Cargo metadata, `install.sh`, the
//!   systemd unit, the README one-liner, `deploy/aws`);
//! * the MIT license ships in the tree and in the release tarball.

use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::process::Command;

const REPO_SLUG: &str = "copper-browser/copper-cloud";
/// Directories skipped by the fallback walk (build output, local deploy state).
const SKIP: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    ".next",
    "out",
    ".terraform",
    "state",
    "bin",
];
/// 12-digit numbers that may appear: well-known public AMI owners and the documentation
/// placeholder AWS itself uses in examples.
const ALLOWED_ACCOUNT_IDS: &[&str] = &[
    "099720109477", // Canonical (Ubuntu AMIs)
    "123456789012", // AWS documentation placeholder
];
/// Public IPv4 addresses conventionally used as fixtures (outside the reserved ranges).
const FIXTURE_IPV4: &[Ipv4Addr] = &[Ipv4Addr::new(1, 2, 3, 4)];
/// Top-level / suffix domains that only resolve on private networks or tailnets.
const INTERNAL_SUFFIXES: &[&str] = &[
    "internal",
    "intranet",
    "corp",
    "lan",
    "home",
    "private",
    "localdomain",
    "ts.net",
];
/// Subdomain labels that name an environment tier — a URL carrying one points at a specific
/// deployment rather than a public product endpoint.
const ENVIRONMENT_LABELS: &[&str] = &[
    "dev", "staging", "stage", "stg", "qa", "uat", "preprod", "int", "internal", "corp",
];
/// RFC 2606 / RFC 6761 names reserved for examples and tests.
const RESERVED_DOMAINS: &[&str] = &[
    "example.com",
    "example.net",
    "example.org",
    "example",
    "test",
    "invalid",
    "localhost",
];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// Tracked + untracked-but-not-ignored files (`git ls-files`), or a filtered walk when git is unavailable.
fn tracked_files() -> Vec<String> {
    let root = root();
    if let Ok(out) = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .output()
    {
        if out.status.success() && !out.stdout.is_empty() {
            return String::from_utf8(out.stdout)
                .unwrap()
                .split('\0')
                .filter(|p| !p.is_empty() && root.join(p).is_file())
                .map(str::to_owned)
                .collect();
        }
    }
    let mut files = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if path.is_dir() {
                if !SKIP.contains(&name.as_str()) {
                    stack.push(path);
                }
            } else {
                files.push(
                    path.strip_prefix(&root)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
    }
    files
}

/// Runs of exactly `n` ASCII digits not touching other alphanumerics (or `-`/`.`/`_`, so
/// hashes, versions and UUID-ish ids don't count).
fn digit_runs(text: &str, n: usize) -> Vec<String> {
    let b = text.as_bytes();
    let boundary = |c: u8| !(c.is_ascii_alphanumeric() || c == b'-' || c == b'.' || c == b'_');
    let mut hits = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_digit() && (i == 0 || boundary(b[i - 1])) {
            let start = i;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            if i - start == n && (i == b.len() || boundary(b[i])) {
                hits.push(text[start..i].to_owned());
            }
        } else {
            i += 1;
        }
    }
    hits
}

/// 12-digit runs that look like cloud account ids (not on the allowlist).
fn account_id_hits(text: &str) -> Vec<String> {
    digit_runs(text, 12)
        .into_iter()
        .filter(|run| !ALLOWED_ACCOUNT_IDS.contains(&run.as_str()))
        .collect()
}

/// Dotted-quad IPv4 literals not embedded in a longer dotted/alphanumeric token (so
/// four-part version strings inside identifiers don't count).
fn ipv4_literals(text: &str) -> Vec<Ipv4Addr> {
    let b = text.as_bytes();
    let token = |c: u8| c.is_ascii_alphanumeric() || c == b'.' || c == b'_';
    let mut hits = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if !(b[i].is_ascii_digit() || b[i] == b'.') || (i > 0 && token(b[i - 1])) {
            i += 1;
            continue;
        }
        let start = i;
        while i < b.len() && token(b[i]) {
            i += 1;
        }
        let word = text[start..i].trim_end_matches('.');
        if let Ok(ip) = word.parse::<Ipv4Addr>() {
            hits.push(ip);
        }
    }
    hits
}

/// Whether an IPv4 address is acceptable in the public tree: loopback, unspecified,
/// link-local (incl. the cloud instance-metadata endpoint), RFC 1918 private, RFC 5737
/// documentation, or a conventional fixture. Anything else — publicly routable addresses
/// and shared/CGNAT space (often a tailnet) — names a real network.
fn ipv4_allowed(ip: Ipv4Addr) -> bool {
    ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_link_local()
        || ip.is_private()
        || ip.is_documentation()
        || ip.is_broadcast()
        || FIXTURE_IPV4.contains(&ip)
}

/// Hosts of `scheme://host` URLs in `text`.
fn url_hosts(text: &str) -> Vec<String> {
    text.match_indices("://")
        .filter_map(|(i, _)| {
            let rest = &text[i + 3..];
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '-'))
                .unwrap_or(rest.len());
            let host = rest[..end].trim_end_matches('.').to_ascii_lowercase();
            (host.contains('.') || host == "localhost").then_some(host)
        })
        .collect()
}

fn under(host: &str, domain: &str) -> bool {
    host == domain || host.ends_with(&format!(".{domain}"))
}

/// Whether a URL host points at a private network or a specific deployment tier: an
/// internal-only suffix, or an environment label among its subdomains. Reserved example
/// domains are always fine.
fn internal_host(host: &str) -> bool {
    if RESERVED_DOMAINS.iter().any(|d| under(host, d)) {
        return false;
    }
    if INTERNAL_SUFFIXES.iter().any(|s| under(host, s)) {
        return true;
    }
    let labels: Vec<&str> = host.split('.').collect();
    // Only subdomain positions: the registrable name and TLD are the last two labels.
    labels.len() > 2
        && labels[..labels.len() - 2]
            .iter()
            .any(|l| ENVIRONMENT_LABELS.contains(l))
}

/// Every hygiene finding in one file's text.
fn hygiene_problems(rel: &str, text: &str) -> Vec<String> {
    let mut problems = Vec::new();
    for run in account_id_hits(text) {
        problems.push(format!("{rel}: 12-digit number {run} (cloud account id?)"));
    }
    for ip in ipv4_literals(text) {
        if !ipv4_allowed(ip) {
            problems.push(format!(
                "{rel}: IPv4 address {ip} (use 192.0.2.0/24, 198.51.100.0/24 or 203.0.113.0/24)"
            ));
        }
    }
    for host in url_hosts(text) {
        if internal_host(&host) {
            problems.push(format!(
                "{rel}: internal/environment host {host} (use an example.com placeholder)"
            ));
        }
    }
    problems
}

#[test]
fn no_internal_infrastructure_in_tracked_files() {
    let mut problems = Vec::new();
    for rel in tracked_files() {
        // Lockfiles are generated (hashes, registry URLs) and checked by their tools.
        let name = Path::new(&rel).file_name().unwrap().to_string_lossy();
        let lockfile = Path::new(&rel)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("lock"))
            || name == ".terraform.lock.hcl"
            || name == "package-lock.json";
        if lockfile {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(root().join(&rel)) else {
            continue; // binary
        };
        problems.extend(hygiene_problems(&rel, &text));
    }
    assert!(
        problems.is_empty(),
        "deployment-specific details in the public tree:\n{}",
        problems.join("\n")
    );
}

#[test]
fn digit_run_detection() {
    assert_eq!(digit_runs("account 123456789012 in", 12), ["123456789012"]);
    assert_eq!(
        digit_runs("sha 0123456789abcdef0123", 12),
        Vec::<String>::new()
    );
    assert_eq!(digit_runs("1234567890123", 12), Vec::<String>::new());
    assert_eq!(digit_runs("v1.123456789012", 12), Vec::<String>::new());
    // Allowlisted ids pass; any other 12-digit run is flagged. The flagged value is built
    // at run time from a single repeated digit so this file stays clean under its own scan.
    assert_eq!(
        account_id_hits("owner 099720109477, account 123456789012"),
        Vec::<String>::new()
    );
    let synthetic = "9".repeat(12);
    assert_eq!(account_id_hits(&format!("id={synthetic}")), [synthetic]);
}

#[test]
fn generic_network_detection() {
    // Extraction: dotted quads only as standalone tokens.
    assert_eq!(
        ipv4_literals("bind 127.0.0.1:8443, peer 203.0.113.7/32 and 198.51.100.4."),
        [
            Ipv4Addr::LOCALHOST,
            Ipv4Addr::new(203, 0, 113, 7),
            Ipv4Addr::new(198, 51, 100, 4)
        ]
    );
    assert_eq!(
        ipv4_literals("v1.2.3.4.5 lib1.2.3.4 999.1.1.1"),
        Vec::<Ipv4Addr>::new()
    );
    // Classification (addresses built numerically, so this file has no routable literal).
    for ok in [
        Ipv4Addr::LOCALHOST,
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::new(169, 254, 169, 254),
        Ipv4Addr::new(10, 0, 0, 5),
        Ipv4Addr::new(192, 0, 2, 1),
        Ipv4Addr::new(203, 0, 113, 10),
    ] {
        assert!(ipv4_allowed(ok), "{ok} should be allowed");
    }
    for bad in [Ipv4Addr::new(8, 8, 8, 8), Ipv4Addr::new(100, 64, 0, 1)] {
        assert!(!ipv4_allowed(bad), "{bad} should be flagged");
    }

    // Hosts: reserved example domains and public product endpoints pass; internal suffixes
    // and environment-tier subdomains are flagged.
    assert_eq!(
        url_hosts("see https://llm.example.com/v1 and http://localhost:3000/x"),
        ["llm.example.com", "localhost"]
    );
    for ok in [
        "github.com",
        "raw.githubusercontent.com",
        "api.dev.example.com",
        "gw.staging.example.org",
        "dev.to",
        "cloud.example.com",
    ] {
        assert!(!internal_host(ok), "{ok} should be allowed");
    }
    for bad in [
        "gateway.dev.acme.io",
        "api.staging.acme.io",
        "db.corp",
        "build.internal",
        "node.tail0000.ts.net",
    ] {
        assert!(internal_host(bad), "{bad} should be flagged");
    }
}

#[test]
fn public_source_and_install_urls_agree() {
    let repo_url = format!("https://github.com/{REPO_SLUG}");
    let raw_install = format!("https://raw.githubusercontent.com/{REPO_SLUG}/main/install.sh");

    let cargo = read("Cargo.toml");
    assert!(
        cargo.contains(&format!("repository = \"{repo_url}\"")),
        "Cargo.toml repository"
    );
    assert!(cargo.contains("license = \"MIT\""), "Cargo.toml license");
    assert_eq!(env!("CARGO_PKG_REPOSITORY"), repo_url);
    assert_eq!(env!("CARGO_PKG_LICENSE"), "MIT");

    let install = read("install.sh");
    assert!(
        install.contains(&format!("REPO=\"{REPO_SLUG}\"")),
        "install.sh REPO"
    );
    assert!(install.contains(&raw_install), "install.sh usage line");
    assert!(install.contains("https://api.github.com/repos/$REPO/releases"));
    assert!(install.contains("https://github.com/$REPO/releases/download/"));
    assert!(install.contains(&format!("Documentation={repo_url}")));
    assert!(read("packaging/copper-cloud.service").contains(&format!("Documentation={repo_url}")));

    for doc in ["README.md", "docs/install.md"] {
        assert!(
            read(doc).contains(&format!("curl -fsSL {raw_install} | sudo sh")),
            "{doc}: install one-liner"
        );
    }
    assert!(read("deploy/aws/lib.sh").contains(&format!(
        "REPO_SLUG=\"${{COPPER_CLOUD_REPO:-{REPO_SLUG}}}\""
    )));

    // Version-pinned examples follow the crate version.
    let tarball = format!("copper-cloud-{}-linux-", env!("CARGO_PKG_VERSION"));
    for doc in ["README.md", "docs/install.md"] {
        assert!(read(doc).contains(&tarball), "{doc}: tarball example");
    }
    let notes = format!("docs/v{}.md", env!("CARGO_PKG_VERSION"));
    assert!(root().join(&notes).is_file(), "{notes} missing");
    assert!(read("README.md").contains(&format!("({notes})")));
}

#[test]
fn mit_license_ships() {
    let license = read("LICENSE");
    assert!(license.starts_with("MIT License\n"), "LICENSE is MIT");
    assert!(license.contains("Permission is hereby granted, free of charge"));
    assert!(read("README.md").contains("[MIT](LICENSE)"));
    let release = read(".github/workflows/release.yml");
    assert!(release.contains("cp LICENSE \"$stage/LICENSE\""));
    assert!(
        release
            .lines()
            .any(|l| l.contains("tar -C \"$stage\"") && l.contains(" LICENSE")),
        "LICENSE is in the release tarball"
    );
}
