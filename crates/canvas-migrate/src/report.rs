//! What a migration run did: one row per canvas plus totals; a table (`Display`) or JSON
//! (`Serialize`).

use std::fmt::{self, Write as _};

use serde::Serialize;
use time::OffsetDateTime;
use uuid::Uuid;

/// Outcome of one canvas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Imported (created) and verified.
    Migrated,
    /// Re-imported over an earlier import (`--replace`) and verified.
    Replaced,
    /// Nothing to do: the ledger (or canvas-cloud, matching the ledger) shows it migrated.
    Already,
    /// `--dry-run`: read, decrypted and encoded, not sent.
    DryRun,
    /// Not migrated; see `error`. Legacy rows are untouched.
    Failed,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Migrated => "migrated",
            Self::Replaced => "replaced",
            Self::Already => "already",
            Self::DryRun => "dry-run",
            Self::Failed => "failed",
        }
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One canvas row.
#[derive(Debug, Clone, Serialize)]
pub struct CanvasReport {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub owner: Option<Uuid>,
    pub status: Status,
    /// Yjs updates sent (a snapshot counts as one, sent first).
    pub updates: u64,
    /// Plaintext Yjs bytes sent.
    pub bytes: u64,
    pub snapshot: bool,
    pub members: u64,
    pub invites: u64,
    pub links: u64,
    pub mentions: u64,
    /// Verified canvas-cloud digest (CONTRACT §6.3), when known.
    pub digest: Option<String>,
    /// Why a row is `already`, or other context.
    pub note: Option<String>,
    pub error: Option<String>,
    pub warnings: Vec<String>,
}

impl CanvasReport {
    pub(crate) fn new(id: Uuid, name: &str, kind: &str, owner: Option<Uuid>) -> Self {
        Self {
            id,
            name: name.to_owned(),
            kind: kind.to_owned(),
            owner,
            status: Status::Failed,
            updates: 0,
            bytes: 0,
            snapshot: false,
            members: 0,
            invites: 0,
            links: 0,
            mentions: 0,
            digest: None,
            note: None,
            error: None,
            warnings: Vec::new(),
        }
    }
}

/// A user canvas-cloud could not provision with its copper id.
#[derive(Debug, Clone, Serialize)]
pub struct UserFailure {
    pub user_id: Uuid,
    pub email: String,
    pub error: String,
}

/// User provisioning (skipped on a dry run).
#[derive(Debug, Clone, Default, Serialize)]
pub struct UsersReport {
    pub total: usize,
    pub provisioned: usize,
    pub created: usize,
    pub failed: Vec<UserFailure>,
}

/// Sums over the canvas rows.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Totals {
    pub canvases: usize,
    pub migrated: usize,
    pub replaced: usize,
    pub already: usize,
    pub dry_run: usize,
    pub failed: usize,
    pub updates: u64,
    pub bytes: u64,
    pub members: u64,
    pub invites: u64,
    pub links: u64,
    pub mentions: u64,
}

/// The result of [`crate::run`].
///
/// `Display` renders the per-canvas table, or pretty JSON when the run was started with
/// `MigrateOptions::json` (so a CLI can always just print it); `Serialize` is the JSON form.
#[derive(Debug, Clone, Serialize)]
#[allow(clippy::struct_excessive_bools)] // run flags + result, not a state machine
pub struct MigrateReport {
    pub canvas_cloud: String,
    pub dry_run: bool,
    pub replace: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub started_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub finished_at: OffsetDateTime,
    pub users: UsersReport,
    pub canvases: Vec<CanvasReport>,
    pub totals: Totals,
    /// A failure that stopped the run before (or instead of) migrating canvases.
    pub error: Option<String>,
    pub ok: bool,
    #[serde(skip)]
    pub(crate) json: bool,
}

impl MigrateReport {
    pub(crate) fn new(canvas_cloud: &str, dry_run: bool, replace: bool, json: bool) -> Self {
        let now = OffsetDateTime::now_utc();
        Self {
            canvas_cloud: canvas_cloud.to_owned(),
            dry_run,
            replace,
            started_at: now,
            finished_at: now,
            users: UsersReport::default(),
            canvases: Vec::new(),
            totals: Totals::default(),
            error: None,
            ok: false,
            json,
        }
    }

    /// `true` when every user was provisioned and no canvas failed (exit status 0).
    pub fn ok(&self) -> bool {
        self.error.is_none()
            && self.users.failed.is_empty()
            && self.canvases.iter().all(|c| c.status != Status::Failed)
    }

    pub(crate) fn finish(&mut self) {
        let mut t = Totals {
            canvases: self.canvases.len(),
            ..Totals::default()
        };
        for c in &self.canvases {
            match c.status {
                Status::Migrated => t.migrated += 1,
                Status::Replaced => t.replaced += 1,
                Status::Already => t.already += 1,
                Status::DryRun => t.dry_run += 1,
                Status::Failed => t.failed += 1,
            }
            t.updates += c.updates;
            t.bytes += c.bytes;
            t.members += c.members;
            t.invites += c.invites;
            t.links += c.links;
            t.mentions += c.mentions;
        }
        self.totals = t;
        self.finished_at = OffsetDateTime::now_utc();
        self.ok = self.ok();
    }

    /// The table, regardless of the `json` option.
    pub fn table(&self) -> String {
        let mut out = String::new();
        let _ = self.write_table(&mut out);
        out
    }

    fn write_table(&self, out: &mut String) -> fmt::Result {
        writeln!(
            out,
            "canvas migrate → {}{}{}",
            self.canvas_cloud,
            if self.dry_run {
                " (dry run: nothing sent)"
            } else {
                ""
            },
            if self.replace { " (replace)" } else { "" },
        )?;
        if self.dry_run {
            writeln!(
                out,
                "users: {} (not provisioned on a dry run)",
                self.users.total
            )?;
        } else {
            writeln!(
                out,
                "users: {} provisioned ({} new), {} failed",
                self.users.provisioned,
                self.users.created,
                self.users.failed.len()
            )?;
        }
        for u in &self.users.failed {
            writeln!(out, "  user {} <{}>: {}", u.user_id, u.email, u.error)?;
        }
        if !self.canvases.is_empty() {
            writeln!(
                out,
                "{:<36}  {:<24}  {:<8}  {:>7}  {:>9}  {:>7}  {:<8}  {:<12}  ERROR",
                "ID", "NAME", "KIND", "UPDATES", "BYTES", "MEMBERS", "STATUS", "DIGEST"
            )?;
        }
        for c in &self.canvases {
            let digest = c.digest.as_deref().map_or("-", |d| &d[..d.len().min(12)]);
            let detail = match (&c.error, &c.note) {
                (Some(e), _) => e.clone(),
                (None, Some(n)) => format!("({n})"),
                (None, None) => String::new(),
            };
            writeln!(
                out,
                "{:<36}  {:<24}  {:<8}  {:>7}  {:>9}  {:>7}  {:<8}  {:<12}  {}",
                c.id,
                clip(&c.name, 24),
                c.kind,
                c.updates,
                human_bytes(c.bytes),
                c.members,
                c.status.as_str(),
                digest,
                detail
            )?;
            for w in &c.warnings {
                writeln!(out, "  warning: {w}")?;
            }
        }
        let t = &self.totals;
        writeln!(
            out,
            "total: {} canvases — {} migrated, {} replaced, {} already, {} dry-run, {} failed; \
             {} updates, {}, {} members, {} invites, {} links, {} mentions",
            t.canvases,
            t.migrated,
            t.replaced,
            t.already,
            t.dry_run,
            t.failed,
            t.updates,
            human_bytes(t.bytes),
            t.members,
            t.invites,
            t.links,
            t.mentions
        )?;
        if let Some(e) = &self.error {
            writeln!(out, "error: {e}")?;
        }
        write!(out, "{}", if self.ok() { "ok" } else { "FAILED" })
    }
}

impl fmt::Display for MigrateReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.json {
            let text = serde_json::to_string_pretty(self).map_err(|_| fmt::Error)?;
            f.write_str(&text)
        } else {
            f.write_str(&self.table())
        }
    }
}

fn clip(s: &str, n: usize) -> String {
    let one_line: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if one_line.chars().count() <= n {
        one_line
    } else {
        let mut out: String = one_line.chars().take(n - 1).collect();
        out.push('…');
        out
    }
}

#[allow(clippy::cast_precision_loss)] // display only
fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64 / 1024.0;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    format!("{v:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_and_clipping() {
        assert_eq!(human_bytes(10), "10 B");
        assert_eq!(human_bytes(1536), "1.5 KiB");
        assert_eq!(human_bytes(88 * 1024 * 1024), "88.0 MiB");
        assert_eq!(clip("short", 24), "short");
        assert_eq!(clip("a\nb", 24), "a b");
        assert_eq!(clip(&"x".repeat(30), 5), "xxxx…");
    }

    #[test]
    fn ok_and_display() {
        let mut r = MigrateReport::new("http://127.0.0.1:8480", false, false, false);
        let mut c = CanvasReport::new(Uuid::nil(), "Roadmap", "shared", None);
        c.status = Status::Migrated;
        c.digest = Some("ab".repeat(32));
        c.updates = 3;
        r.canvases.push(c);
        r.finish();
        assert!(r.ok());
        let text = r.to_string();
        assert!(text.contains("Roadmap"), "{text}");
        assert!(text.contains("migrated"), "{text}");
        assert!(text.contains("abababababab "), "{text}");
        assert!(text.ends_with("ok"), "{text}");
        r.canvases[0].status = Status::Failed;
        r.finish();
        assert!(!r.ok());
        assert_eq!(r.totals.failed, 1);
        r.json = true;
        let v: serde_json::Value = serde_json::from_str(&r.to_string()).unwrap();
        assert_eq!(v["canvases"][0]["status"], "failed");
        assert_eq!(v["ok"], false);
        assert!(v.get("json").is_none());
    }
}
