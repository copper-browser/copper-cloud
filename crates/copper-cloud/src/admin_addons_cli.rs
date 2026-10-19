//! `copper-cloud addons …` (also `copper-cloud admin addons …`): addon administration straight
//! against the database, through the same domain module as the admin API
//! ([`copper_cloud_core::addons`]) with [`Actor::Cli`] in the audit rows.
//!
//! The CLI runs outside the server process, so it cannot push the SSE `addons` event: Coppers
//! pick CLI changes up at their next check (launch, update check, or any portal change).

// Doc comments here are `--help` text, so no Markdown backticks.
#![allow(clippy::doc_markdown)]

use std::io::{BufRead as _, IsTerminal as _, Read as _, Write as _};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context as _};
use clap::{Args, Subcommand};
use copper_cloud_core::addons::{self, store, Actor, AddonError, Slot};
use copper_cloud_core::config::Config;
use copper_cloud_core::db;
use serde_json::Value;

#[derive(Subcommand, Debug)]
pub enum AddonsCommand {
    /// List addons: slot, id, current version, on/off, audience, priority.
    List {
        /// Print the admin API's JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// List the slots of the Copper window an addon can replace.
    Slots,
    /// Upload a .copperaddon zip. A new id becomes an addon that is OFF for everyone; a new
    /// version of an existing addon becomes its current version.
    Upload(AddonsUploadArgs),
    /// Turn an addon on, for everyone or only for the listed people (without either flag the
    /// current audience is kept).
    Enable(AddonsEnableArgs),
    /// Turn an addon off (Coppers fall back to the slot's built-in default).
    Disable(AddonIdArg),
    /// Replace an addon's settings values (validated against its manifest's settings).
    Settings(AddonsSettingsArgs),
    /// Delete an addon and every uploaded version.
    Delete {
        #[command(flatten)]
        id: AddonIdArg,
        /// Do not ask for confirmation.
        #[arg(long)]
        yes: bool,
    },
}

/// The addon, by id (positional or --id).
#[derive(Args, Debug, Clone)]
pub struct AddonIdArg {
    /// The addon id (manifest.json "id"), e.g. com.example.sidebar.
    #[arg(
        value_name = "ID",
        required_unless_present = "id_flag",
        conflicts_with = "id_flag"
    )]
    pub id: Option<String>,
    /// Same as the positional ID.
    #[arg(long = "id", value_name = "ID")]
    pub id_flag: Option<String>,
}

impl AddonIdArg {
    pub fn get(&self) -> &str {
        self.id
            .as_deref()
            .or(self.id_flag.as_deref())
            .unwrap_or_default()
            .trim()
    }
}

#[derive(Args, Debug, Clone)]
pub struct AddonsUploadArgs {
    /// The .copperaddon (zip) file.
    #[arg(
        value_name = "FILE",
        required_unless_present = "file_flag",
        conflicts_with = "file_flag"
    )]
    pub file: Option<PathBuf>,
    /// Same as the positional FILE.
    #[arg(long = "file", value_name = "FILE")]
    pub file_flag: Option<PathBuf>,
    /// The slot this addon replaces; must equal the manifest's "slot" (see `addons slots`).
    #[arg(long, value_name = "SLOT")]
    pub slot: String,
}

impl AddonsUploadArgs {
    pub fn path(&self) -> &Path {
        self.file
            .as_deref()
            .or(self.file_flag.as_deref())
            .unwrap_or_else(|| Path::new(""))
    }
}

#[derive(Args, Debug, Clone)]
pub struct AddonsEnableArgs {
    #[command(flatten)]
    pub id: AddonIdArg,
    /// Every user of this instance gets the addon.
    #[arg(long, conflicts_with = "people")]
    pub everyone: bool,
    /// Only these emails get it (comma-separated or repeated; replaces the list). People who
    /// have not signed up yet get it once they do.
    #[arg(long, value_name = "EMAILS", value_delimiter = ',', num_args = 1..)]
    pub people: Option<Vec<String>>,
    /// Within a slot the highest priority wins (-1000..1000, default 0).
    #[arg(long, value_name = "N", allow_negative_numbers = true,
          value_parser = clap::value_parser!(i32).range(-1000..=1000))]
    pub priority: Option<i32>,
}

#[derive(Args, Debug, Clone)]
pub struct AddonsSettingsArgs {
    #[command(flatten)]
    pub id: AddonIdArg,
    /// The settings as a JSON object, e.g. '{"apiUrl":"https://api.example.com"}'. Keys must
    /// be declared in the manifest; null resets a key to its default.
    #[arg(
        long,
        value_name = "JSON",
        conflicts_with = "file",
        required_unless_present = "file"
    )]
    pub json: Option<String>,
    /// Read the settings JSON from this file ("-" = stdin).
    #[arg(long, value_name = "FILE")]
    pub file: Option<PathBuf>,
}

/// Entry point for `copper-cloud addons …`.
pub async fn run(cfg: Config, cmd: AddonsCommand) -> anyhow::Result<()> {
    let pool = db::connect(&cfg).await?;
    let result = dispatch(&cfg, &pool, cmd).await;
    pool.close().await;
    result
}

#[allow(clippy::needless_pass_by_value)] // used as `map_err(err)`
fn err(e: impl Into<AddonError>) -> anyhow::Error {
    anyhow::anyhow!("{}", e.into())
}

fn fmt_time(rfc3339: &str) -> String {
    // 2026-10-19T18:11:48.680204Z → 2026-10-19 18:11
    rfc3339.get(..16).unwrap_or(rfc3339).replace('T', " ")
}

fn audience_label(v: &store::AddonView) -> String {
    if v.audience == "everyone" {
        "everyone".to_owned()
    } else {
        let n = v.people.len();
        format!("{n} {}", if n == 1 { "person" } else { "people" })
    }
}

fn confirm(prompt: &str) -> anyhow::Result<bool> {
    if !std::io::stdin().is_terminal() {
        bail!("refusing to delete without --yes (stdin is not a terminal)");
    }
    eprint!("{prompt}");
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes"))
}

fn read_settings(args: &AddonsSettingsArgs) -> anyhow::Result<Value> {
    let text = match (&args.json, &args.file) {
        (Some(j), _) => j.clone(),
        (None, Some(p)) if p.as_os_str() == "-" => {
            let mut s = String::new();
            std::io::stdin()
                .lock()
                .read_to_string(&mut s)
                .context("reading settings from stdin")?;
            s
        }
        (None, Some(p)) => {
            std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?
        }
        (None, None) => bail!("pass --json '{{…}}' or --file FILE"),
    };
    serde_json::from_str(&text).context("settings must be a JSON object")
}

fn print_view(v: &store::AddonView) {
    println!("id:        {}", v.id);
    println!("name:      {}", v.name);
    println!("slot:      {}", v.slot);
    println!(
        "version:   {}",
        v.current.as_ref().map_or("-", |c| c.version.as_str())
    );
    println!("status:    {}", if v.enabled { "on" } else { "off" });
    println!("audience:  {}", audience_label(v));
    if v.audience == "people" && !v.people.is_empty() {
        let emails: Vec<&str> = v.people.iter().map(|p| p.email.as_str()).collect();
        println!("people:    {}", emails.join(", "));
    }
    println!("priority:  {}", v.priority);
    println!("revision:  {}", v.revision);
}

#[allow(clippy::too_many_lines)]
pub async fn dispatch(cfg: &Config, pool: &sqlx::PgPool, cmd: AddonsCommand) -> anyhow::Result<()> {
    let actor = Actor::Cli;
    match cmd {
        AddonsCommand::List { json } => {
            let rows = store::list(pool).await.map_err(err)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&rows)?);
                return Ok(());
            }
            println!(
                "{:<16} {:<36} {:<12} {:<6} {:<12} {:>8}  {:<16}",
                "SLOT", "ID", "VERSION", "STATUS", "AUDIENCE", "PRIORITY", "UPDATED"
            );
            for v in &rows {
                println!(
                    "{:<16} {:<36} {:<12} {:<6} {:<12} {:>8}  {:<16}",
                    v.slot,
                    v.id,
                    v.current.as_ref().map_or("-", |c| c.version.as_str()),
                    if v.enabled { "on" } else { "off" },
                    audience_label(v),
                    v.priority,
                    fmt_time(&v.updated_at),
                );
            }
            println!("{} addon(s)", rows.len());
        }
        AddonsCommand::Slots => {
            for s in Slot::ALL {
                println!("{:<16} {} — {}", s.id(), s.name(), s.description());
            }
        }
        AddonsCommand::Upload(args) => {
            let slot = Slot::parse(args.slot.trim()).with_context(|| {
                format!(
                    "unknown slot \"{}\" (known: {})",
                    args.slot,
                    Slot::known_list()
                )
            })?;
            let path = args.path().to_owned();
            let limit = cfg.limits.max_addon_bytes;
            let len = std::fs::metadata(&path)
                .with_context(|| format!("reading {}", path.display()))?
                .len();
            if len > limit as u64 {
                bail!(
                    "{} is {len} bytes; limits.max_addon_bytes is {limit}",
                    path.display()
                );
            }
            let zip =
                std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
            let built = tokio::task::spawn_blocking(move || addons::bundle::ingest_zip(&zip))
                .await?
                .map_err(err)?;
            let (outcome, v) = store::upload(pool, &actor, slot, &built)
                .await
                .map_err(err)?;
            let version = &built.manifest.version;
            match outcome {
                store::UploadOutcome::Created => {
                    println!(
                        "created {} {version} ({}, {} files, sha256 {})",
                        v.id, v.slot, built.files, built.sha256
                    );
                    println!(
                        "it is off; turn it on with: copper-cloud addons enable {} --everyone (or --people a@example.com,…)",
                        v.id
                    );
                }
                store::UploadOutcome::NewVersion => println!(
                    "uploaded {} {version}, now current (sha256 {}); Coppers pick it up at their next check",
                    v.id, built.sha256
                ),
                store::UploadOutcome::Unchanged => {
                    println!("unchanged: {} {version} is already stored with the same contents", v.id);
                }
            }
        }
        AddonsCommand::Enable(args) => {
            let patch = store::Patch {
                enabled: Some(true),
                audience: if args.everyone {
                    Some(store::Audience::Everyone)
                } else if args.people.is_some() {
                    Some(store::Audience::People)
                } else {
                    None
                },
                people: args.people.clone(),
                priority: args.priority,
                ..store::Patch::default()
            };
            let v = store::update(pool, &actor, args.id.get(), &patch)
                .await
                .map_err(err)?;
            print_view(&v);
            if v.audience == "people" && v.people.is_empty() {
                eprintln!(
                    "note: nobody is in the audience yet; pass --everyone or --people a@example.com,…"
                );
            }
        }
        AddonsCommand::Disable(id) => {
            let patch = store::Patch {
                enabled: Some(false),
                ..store::Patch::default()
            };
            let v = store::update(pool, &actor, id.get(), &patch)
                .await
                .map_err(err)?;
            print_view(&v);
        }
        AddonsCommand::Settings(args) => {
            let settings = read_settings(&args)?;
            let patch = store::Patch {
                settings: Some(settings),
                ..store::Patch::default()
            };
            let v = store::update(pool, &actor, args.id.get(), &patch)
                .await
                .map_err(err)?;
            println!("{}", serde_json::to_string_pretty(&v.settings)?);
        }
        AddonsCommand::Delete { id, yes } => {
            let id = id.get().to_owned();
            // 404 before prompting.
            let v = store::get(pool, &id).await.map_err(err)?;
            if !yes
                && !confirm(&format!(
                    "Delete addon {id} ({}) and all {} version(s)? [y/N] ",
                    v.name,
                    v.versions.len()
                ))?
            {
                bail!("aborted");
            }
            store::delete(pool, &actor, &id).await.map_err(err)?;
            println!("deleted {id}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use super::*;
    use crate::cli::{AdminCommand, Cli, Command};

    fn parse(args: &[&str]) -> Result<AddonsCommand, clap::Error> {
        let mut full = vec!["copper-cloud", "addons"];
        full.extend_from_slice(args);
        match Cli::try_parse_from(full)?.command {
            Some(Command::Addons(cmd)) => Ok(cmd),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn addons_flags() {
        // Positional and flag forms.
        for argv in [
            &["upload", "a.copperaddon", "--slot", "agent-sidebar"][..],
            &[
                "upload",
                "--file",
                "a.copperaddon",
                "--slot",
                "agent-sidebar",
            ][..],
        ] {
            let AddonsCommand::Upload(a) = parse(argv).unwrap() else {
                panic!()
            };
            assert_eq!(a.path(), Path::new("a.copperaddon"));
            assert_eq!(a.slot, "agent-sidebar");
        }
        assert!(
            parse(&["upload", "a.copperaddon"]).is_err(),
            "--slot is required"
        );
        assert!(
            parse(&["upload", "--slot", "agent-sidebar"]).is_err(),
            "a file is required"
        );

        let AddonsCommand::Enable(e) = parse(&[
            "enable",
            "--id",
            "com.example.x",
            "--people",
            "a@example.com,b@example.com",
            "--people",
            "c@example.com",
        ])
        .unwrap() else {
            panic!()
        };
        assert_eq!(e.id.get(), "com.example.x");
        assert_eq!(
            e.people.as_deref().unwrap(),
            ["a@example.com", "b@example.com", "c@example.com"]
        );
        assert!(!e.everyone);
        let AddonsCommand::Enable(e) =
            parse(&["enable", "com.example.x", "--everyone", "--priority", "-5"]).unwrap()
        else {
            panic!()
        };
        assert!(e.everyone && e.people.is_none());
        assert_eq!(e.priority, Some(-5));
        assert!(parse(&[
            "enable",
            "com.example.x",
            "--everyone",
            "--people",
            "a@example.com"
        ])
        .is_err());
        assert!(parse(&["enable", "com.example.x", "--priority", "1001"]).is_err());
        assert!(parse(&["enable"]).is_err(), "an id is required");
        assert!(parse(&["enable", "a.b", "--id", "c.d"]).is_err());

        let AddonsCommand::Disable(d) = parse(&["disable", "com.example.x"]).unwrap() else {
            panic!()
        };
        assert_eq!(d.get(), "com.example.x");

        let AddonsCommand::Settings(s) =
            parse(&["settings", "--id", "com.example.x", "--json", "{\"k\":1}"]).unwrap()
        else {
            panic!()
        };
        assert_eq!(read_settings(&s).unwrap(), serde_json::json!({ "k": 1 }));
        assert!(
            parse(&["settings", "com.example.x"]).is_err(),
            "--json or --file"
        );
        assert!(parse(&["settings", "com.example.x", "--json", "{}", "--file", "x"]).is_err());

        let AddonsCommand::Delete { id, yes } =
            parse(&["delete", "--id", "com.example.x", "--yes"]).unwrap()
        else {
            panic!()
        };
        assert_eq!(id.get(), "com.example.x");
        assert!(yes);
        assert!(matches!(
            parse(&["list", "--json"]).unwrap(),
            AddonsCommand::List { json: true }
        ));
        assert!(matches!(parse(&["slots"]).unwrap(), AddonsCommand::Slots));

        // The contract's spelling: `copper-cloud admin addons …`.
        let cli =
            Cli::try_parse_from(["copper-cloud", "admin", "addons", "disable", "a.b"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Admin(AdminCommand::Addons(
                AddonsCommand::Disable(_)
            )))
        ));
    }
}
