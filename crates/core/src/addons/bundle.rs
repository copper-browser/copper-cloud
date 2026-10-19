//! Bundles. Authors upload a zip (`.copperaddon`) with `manifest.json` at the root or inside
//! one top-level folder (stripped). The cloud turns it into one canonical JSON document that
//! clients download, so they need no zip code:
//!
//! ```json
//! {"format":"copper-addon-bundle/1","manifest":{…},
//!  "files":[{"path":"index.html","size":1234,"sha256":"<hex>","data":"<base64>"}]}
//! ```
//!
//! Files are sorted by path (bytewise), data is standard base64 (padded), the document is
//! serialized compactly, and the lowercase hex SHA-256 of those exact bytes is the version's
//! identity. The bundle keeps `manifest.json` among its files, so an unpacked bundle is the
//! author's folder.
//!
//! Upload guards: ≤ [`MAX_FILES`] files, ≤ [`MAX_DECODED_BYTES`] decoded, ≤
//! [`MAX_SERVED_BYTES`] served; paths follow [`valid_bundle_path`]; absolute, backslash, `.`/`..`
//! and symlink entries are refused; `__MACOSX/` and `.DS_Store` are dropped; directories are
//! implied; entries that expand more than [`MAX_RATIO`]× (zip bombs) are refused; only Stored
//! and Deflate entries, no encryption.

use std::io::{Cursor, Read as _};

use serde::Serialize;
use serde_json::Value;

use super::manifest::{valid_bundle_path, Manifest};
use crate::error::{ApiError, ApiResult};

pub const FORMAT: &str = "copper-addon-bundle/1";
pub const MANIFEST_PATH: &str = "manifest.json";
/// Most files in one bundle.
pub const MAX_FILES: usize = 1000;
/// Most decoded bytes across all files.
pub const MAX_DECODED_BYTES: u64 = 10_000_000;
/// Largest served document.
pub const MAX_SERVED_BYTES: usize = 14_000_000;
/// Largest `manifest.json`.
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
/// Zip central-directory entries examined at most (files + directories + dropped junk).
const MAX_ZIP_ENTRIES: usize = 4 * MAX_FILES;
/// An entry that decodes to at least [`RATIO_MIN_BYTES`] may be at most this many times its
/// compressed size.
pub const MAX_RATIO: u64 = 100;
const RATIO_MIN_BYTES: u64 = 256 * 1024;

/// One file of a bundle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleFile {
    pub path: String,
    pub data: Vec<u8>,
}

/// A validated, serialized bundle.
#[derive(Clone, Debug)]
pub struct Built {
    pub manifest: Manifest,
    /// The served document.
    pub bytes: Vec<u8>,
    /// Lowercase hex SHA-256 of [`Built::bytes`].
    pub sha256: String,
    pub files: usize,
    pub decoded_bytes: u64,
}

fn bad(msg: impl std::fmt::Display) -> ApiError {
    ApiError::bad_request(format!("bundle: {msg}"))
}

/// Unzip, validate and serialize an uploaded `.copperaddon`. CPU-bound (≤ 10 MB inflate +
/// hashing): call from a blocking task.
pub fn ingest_zip(zip: &[u8]) -> ApiResult<Built> {
    build(read_zip(zip)?)
}

/// `true` for zip names the cloud refuses outright (even for directories and junk).
fn unsafe_name(name: &str) -> bool {
    let trimmed = name.strip_suffix('/').unwrap_or(name);
    name.is_empty()
        || name.starts_with('/')
        || name.contains('\\')
        || name.contains('\0')
        || trimmed.split('/').any(|seg| seg == "." || seg == "..")
        // `C:...` drive prefixes.
        || trimmed
            .split('/')
            .next()
            .is_some_and(|first| first.len() == 2 && first.ends_with(':'))
}

/// macOS Finder litter: dropped silently.
fn is_junk(name: &str) -> bool {
    name.split('/')
        .any(|seg| seg == "__MACOSX" || seg == ".DS_Store")
}

/// Read every file of the zip (directories implied, junk dropped), strip a single top-level
/// folder holding `manifest.json`, validate paths. Returns files sorted by path.
pub fn read_zip(zip: &[u8]) -> ApiResult<Vec<BundleFile>> {
    let mut archive = zip::ZipArchive::new(Cursor::new(zip))
        .map_err(|e| bad(format!("not a valid zip file ({e})")))?;
    if archive.len() > MAX_ZIP_ENTRIES {
        return Err(bad(format!(
            "the zip has {} entries (at most {MAX_FILES} files)",
            archive.len()
        )));
    }
    let mut files: Vec<BundleFile> = Vec::new();
    let mut decoded: u64 = 0;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| bad(format!("entry {i}: {e}")))?;
        let name = entry.name().to_owned();
        if unsafe_name(&name) {
            return Err(bad(format!(
                "unsafe path \"{name}\" (absolute, backslash, drive or ./.. segments)"
            )));
        }
        if entry.is_symlink() {
            return Err(bad(format!("\"{name}\" is a symlink (not allowed)")));
        }
        if entry.is_dir() || is_junk(&name) {
            continue;
        }
        if files.len() >= MAX_FILES {
            return Err(bad(format!("more than {MAX_FILES} files")));
        }
        let remaining = MAX_DECODED_BYTES - decoded;
        if entry.size() > remaining {
            return Err(bad(format!(
                "more than {MAX_DECODED_BYTES} bytes once unzipped"
            )));
        }
        let mut data = Vec::with_capacity(usize::try_from(entry.size()).unwrap_or(0));
        (&mut entry)
            .take(remaining + 1)
            .read_to_end(&mut data)
            .map_err(|e| bad(format!("\"{name}\": {e}")))?;
        let len = data.len() as u64;
        if len > remaining {
            return Err(bad(format!(
                "more than {MAX_DECODED_BYTES} bytes once unzipped"
            )));
        }
        if len >= RATIO_MIN_BYTES && len > entry.compressed_size().saturating_mul(MAX_RATIO) {
            return Err(bad(format!(
                "\"{name}\" expands more than {MAX_RATIO}x (refusing a possible zip bomb)"
            )));
        }
        decoded += len;
        files.push(BundleFile { path: name, data });
    }
    if files.is_empty() {
        return Err(bad("the zip holds no files"));
    }

    // manifest.json at the root, or inside exactly one top-level folder (stripped).
    let prefix = if files.iter().any(|f| f.path == MANIFEST_PATH) {
        String::new()
    } else {
        let first = files[0].path.split('/').next().unwrap_or_default();
        let prefix = format!("{first}/");
        let nested = format!("{prefix}{MANIFEST_PATH}");
        if files.iter().all(|f| f.path.starts_with(&prefix))
            && files.iter().any(|f| f.path == nested)
        {
            prefix
        } else {
            return Err(bad(format!(
                "{MANIFEST_PATH} must be at the top of the zip, or inside a single top-level folder"
            )));
        }
    };
    for f in &mut files {
        f.path.drain(..prefix.len());
        if !valid_bundle_path(&f.path) {
            return Err(bad(format!(
                "path \"{}\" is not allowed (letters, digits, '.', '_', '@', '-' and '/', at most 200 characters)",
                f.path
            )));
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    if let Some(w) = files.windows(2).find(|w| w[0].path == w[1].path) {
        return Err(bad(format!("duplicate path \"{}\"", w[0].path)));
    }
    Ok(files)
}

#[derive(Serialize)]
struct Served<'a> {
    format: &'static str,
    manifest: &'a Value,
    files: Vec<ServedFile<'a>>,
}

#[derive(Serialize)]
struct ServedFile<'a> {
    path: &'a str,
    size: usize,
    sha256: String,
    data: String,
}

/// Validate `manifest.json` and the entry, then serialize the served document.
/// `files` must already follow the path rule (as [`read_zip`] returns them).
pub fn build(mut files: Vec<BundleFile>) -> ApiResult<Built> {
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let manifest_file = files
        .iter()
        .find(|f| f.path == MANIFEST_PATH)
        .ok_or_else(|| bad(format!("no {MANIFEST_PATH}")))?;
    if manifest_file.data.len() > MAX_MANIFEST_BYTES {
        return Err(bad(format!(
            "{MANIFEST_PATH} is larger than {MAX_MANIFEST_BYTES} bytes"
        )));
    }
    let manifest = Manifest::parse(&manifest_file.data)?;
    if !files.iter().any(|f| f.path == manifest.entry) {
        return Err(bad(format!(
            "the entry \"{}\" named in {MANIFEST_PATH} is not in the bundle",
            manifest.entry
        )));
    }
    let decoded_bytes = files.iter().map(|f| f.data.len() as u64).sum();
    let served = Served {
        format: FORMAT,
        manifest: &manifest.raw,
        files: files
            .iter()
            .map(|f| ServedFile {
                path: &f.path,
                size: f.data.len(),
                sha256: crate::ids::sha256_hex(&f.data),
                data: crate::ids::b64_std(&f.data),
            })
            .collect(),
    };
    let bytes = serde_json::to_vec(&served).map_err(ApiError::internal)?;
    if bytes.len() > MAX_SERVED_BYTES {
        return Err(bad(format!(
            "the served bundle would be {} bytes (at most {MAX_SERVED_BYTES})",
            bytes.len()
        )));
    }
    let sha256 = crate::ids::sha256_hex(&bytes);
    Ok(Built {
        files: files.len(),
        manifest,
        bytes,
        sha256,
        decoded_bytes,
    })
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use serde_json::json;

    use super::*;

    fn manifest() -> Value {
        json!({
            "schema": "copper-addon/1",
            "id": "com.example.hello",
            "name": "Hello",
            "version": "1.0.0",
            "slot": "agent-sidebar",
            "entry": "index.html",
            "bridge": 1,
            "permissions": { "hosts": [], "capabilities": [] }
        })
    }

    enum Entry<'a> {
        File(&'a str, &'a [u8]),
        Stored(&'a str, &'a [u8]),
        Dir(&'a str),
        Symlink(&'a str, &'a str),
    }

    fn zip_of(entries: &[Entry<'_>]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for e in entries {
            match e {
                Entry::File(name, data) => {
                    w.start_file(*name, opts).unwrap();
                    w.write_all(data).unwrap();
                }
                Entry::Stored(name, data) => {
                    let stored = opts.compression_method(zip::CompressionMethod::Stored);
                    w.start_file(*name, stored).unwrap();
                    w.write_all(data).unwrap();
                }
                Entry::Dir(name) => w.add_directory(*name, opts).unwrap(),
                Entry::Symlink(name, target) => w.add_symlink(*name, *target, opts).unwrap(),
            }
        }
        w.finish().unwrap().into_inner()
    }

    fn ok_files(m: &Value) -> Vec<u8> {
        serde_json::to_vec(m).unwrap()
    }

    #[test]
    fn root_and_single_folder_layouts_serve_identical_bytes() {
        let m = ok_files(&manifest());
        let root = ingest_zip(&zip_of(&[
            Entry::File("index.html", b"<p>hi</p>"),
            Entry::File("manifest.json", &m),
            Entry::Dir("css/"),
            Entry::File("css/a.css", b"p{}"),
            Entry::File(".DS_Store", b"junk"),
            Entry::File("__MACOSX/._index.html", b"junk"),
        ]))
        .unwrap();
        let nested = ingest_zip(&zip_of(&[
            Entry::Dir("hello/"),
            Entry::File("hello/manifest.json", &m),
            Entry::File("hello/css/a.css", b"p{}"),
            Entry::File("hello/index.html", b"<p>hi</p>"),
            Entry::File("hello/css/.DS_Store", b"junk"),
        ]))
        .unwrap();
        assert_eq!(root.bytes, nested.bytes);
        assert_eq!(root.sha256, nested.sha256);
        assert_eq!(root.sha256, crate::ids::sha256_hex(&root.bytes));
        assert_eq!(root.files, 3);
        let doc: Value = serde_json::from_slice(&root.bytes).unwrap();
        assert_eq!(doc["format"], FORMAT);
        assert_eq!(doc["manifest"], manifest());
        let paths: Vec<&str> = doc["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["path"].as_str().unwrap())
            .collect();
        assert_eq!(paths, ["css/a.css", "index.html", "manifest.json"]);
        let f = &doc["files"][1];
        assert_eq!(f["size"], 9);
        assert_eq!(f["sha256"], crate::ids::sha256_hex(b"<p>hi</p>"));
        assert_eq!(f["data"], crate::ids::b64_std(b"<p>hi</p>"));
        // Compact: no whitespace outside strings, keys in contract order.
        let text = String::from_utf8(root.bytes.clone()).unwrap();
        assert!(
            text.starts_with(r#"{"format":"copper-addon-bundle/1","manifest":{"#),
            "{text}"
        );
        assert!(!text.contains(": ") && !text.contains('\n'));
    }

    #[test]
    fn refuses_unsafe_or_invalid_bundles() {
        let m = ok_files(&manifest());
        let cases: Vec<(&str, Vec<u8>)> = vec![
            (
                "traversal",
                zip_of(&[
                    Entry::File("manifest.json", &m),
                    Entry::File("index.html", b"x"),
                    Entry::File("../evil.html", b"x"),
                ]),
            ),
            (
                "nested traversal",
                zip_of(&[
                    Entry::File("manifest.json", &m),
                    Entry::File("index.html", b"x"),
                    Entry::File("a/../../evil", b"x"),
                ]),
            ),
            (
                "absolute",
                zip_of(&[
                    Entry::File("manifest.json", &m),
                    Entry::File("index.html", b"x"),
                    Entry::File("/etc/passwd", b"x"),
                ]),
            ),
            (
                "backslash",
                zip_of(&[
                    Entry::File("manifest.json", &m),
                    Entry::File("index.html", b"x"),
                    Entry::File("a\\b.js", b"x"),
                ]),
            ),
            (
                "symlink",
                zip_of(&[
                    Entry::File("manifest.json", &m),
                    Entry::File("index.html", b"x"),
                    Entry::Symlink("link.html", "/etc/passwd"),
                ]),
            ),
            (
                "bad path chars",
                zip_of(&[
                    Entry::File("manifest.json", &m),
                    Entry::File("index.html", b"x"),
                    Entry::File("my file.js", b"x"),
                ]),
            ),
            (
                "missing entry",
                zip_of(&[
                    Entry::File("manifest.json", &m),
                    Entry::File("main.html", b"x"),
                ]),
            ),
            ("no manifest", zip_of(&[Entry::File("index.html", b"x")])),
            (
                "two folders",
                zip_of(&[
                    Entry::File("a/manifest.json", &m),
                    Entry::File("a/index.html", b"x"),
                    Entry::File("b/x.js", b"x"),
                ]),
            ),
            (
                "deep manifest",
                zip_of(&[
                    Entry::File("a/b/manifest.json", &m),
                    Entry::File("a/b/index.html", b"x"),
                ]),
            ),
            ("empty", zip_of(&[Entry::Dir("a/")])),
            ("not a zip", b"PK\x03\x04 nope".to_vec()),
            (
                "bad manifest",
                zip_of(&[
                    Entry::File("manifest.json", b"{"),
                    Entry::File("index.html", b"x"),
                ]),
            ),
        ];
        for (what, zip) in cases {
            let err = ingest_zip(&zip).unwrap_err();
            assert_eq!(err.status(), 400, "{what}: {err}");
        }
    }

    #[test]
    fn limits_and_zip_bombs() {
        let m = ok_files(&manifest());
        // Highly compressible content above the ratio threshold.
        let zeros = vec![0u8; 2 * 1024 * 1024];
        let err = ingest_zip(&zip_of(&[
            Entry::File("manifest.json", &m),
            Entry::File("index.html", b"x"),
            Entry::File("bomb.bin", &zeros),
        ]))
        .unwrap_err();
        assert!(err.to_string().contains("zip bomb"), "{err}");
        // Too many files.
        let names: Vec<String> = (0..=MAX_FILES).map(|i| format!("f{i}.js")).collect();
        let mut entries = vec![
            Entry::File("manifest.json", &m),
            Entry::File("index.html", b"x"),
        ];
        entries.extend(names.iter().map(|n| Entry::File(n, b"x")));
        let err = ingest_zip(&zip_of(&entries)).unwrap_err();
        assert!(err.to_string().contains("more than 1000 files"), "{err}");
        // Too many decoded bytes (stored data stays under the ratio guard).
        let noise = vec![7u8; 11_000_000];
        let err = ingest_zip(&zip_of(&[
            Entry::File("manifest.json", &m),
            Entry::File("index.html", b"x"),
            Entry::Stored("big.bin", &noise),
        ]))
        .unwrap_err();
        assert!(err.to_string().contains("unzipped"), "{err}");
    }
}
