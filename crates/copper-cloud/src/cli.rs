//! Command-line interface.

// Doc comments here are `--help` text, so no Markdown backticks.
#![allow(clippy::doc_markdown)]

use std::io::{BufRead as _, IsTerminal as _};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{bail, Context as _};
use clap::{Args, Parser, Subcommand};
use copper_cloud_core::config::{self, Config, ConfigSource, GenerateOptions, TlsMode};

#[derive(Parser, Debug)]
#[command(
    name = "copper-cloud",
    version = crate_version(),
    about = "Self-hosted sync + collaborative canvas server for the Copper browser",
    long_about = None
)]
pub struct Cli {
    /// Config file (default /etc/copper-cloud/copper-cloud.toml; env COPPER_CLOUD_CONFIG).
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

fn crate_version() -> &'static str {
    static V: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    V.get_or_init(crate::version)
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Run the server (default).
    Serve(ServeArgs),
    /// Apply pending database migrations and exit.
    Migrate,
    /// Print the link code (copper-cloud://HOST:PORT/#k=…&fp=…) Copper uses to connect.
    LinkCode,
    /// Check config, database, migrations, TLS and the listener; non-zero exit on failure.
    Doctor,
    /// Wait until the local server answers /healthz (used by install.sh).
    Healthcheck {
        /// Seconds to keep retrying.
        #[arg(long, default_value_t = 0)]
        wait: u64,
    },
    /// Create the self-signed certificate (tls.cert_path / tls.key_path) if missing.
    TlsInit {
        /// Replace an existing certificate (changes the fingerprint: every Copper must re-link).
        #[arg(long)]
        force: bool,
        /// Extra SAN (DNS name or IP); repeatable. The public_url host is always included.
        #[arg(long = "name", value_name = "NAME")]
        names: Vec<String>,
    },
    /// Print the version.
    Version,
    /// Write a new config file with freshly generated keys.
    InitConfig(InitConfigArgs),
    /// User, admin-account, access-mode and signup administration.
    #[command(subcommand)]
    Admin(AdminCommand),
    /// Cloud-wide intelligence keys (Jev + LLM router) every signed-in Copper receives, and the
    /// org-wide agent tool-call round budget.
    #[command(subcommand)]
    Intelligence(IntelligenceCommand),
    /// Link to a Canvas Cloud server (where canvases live), check it, provision users and
    /// migrate legacy canvases into it.
    #[command(subcommand)]
    Canvas(CanvasCommand),
}

#[derive(Subcommand, Debug)]
pub enum CanvasCommand {
    /// Link (or re-link) a Canvas Cloud: checks /v1/info and /v1/apps/self/stats with the
    /// app key, stores the key encrypted, then provisions every user. A running server picks
    /// the change up within seconds.
    Link {
        /// Canvas Cloud base URL: https://… (http:// only for a loopback host).
        #[arg(long, value_name = "URL")]
        url: String,
        /// File holding the Canvas Cloud app key (cva_…); "-" reads stdin. Never argv.
        #[arg(long, value_name = "FILE")]
        key_file: PathBuf,
        /// SHA-256 of a self-signed Canvas Cloud's certificate (hex, colons allowed).
        #[arg(long, value_name = "HEX")]
        fingerprint: Option<String>,
        /// Do not provision users now (the server's reconcile loop does it later).
        #[arg(long)]
        no_sync: bool,
    },
    /// Remove the link (canvas routes answer 503 canvas_unavailable until linked again).
    Unlink,
    /// Show the link (key masked) and whether Canvas Cloud is reachable.
    Status,
    /// Provision every user on the linked Canvas Cloud now.
    SyncUsers {
        /// Also delete Canvas Cloud users of this link whose Copper account no longer exists
        /// (and their canvases there). The background reconcile only reports them.
        #[arg(long)]
        prune: bool,
    },
    /// Copy legacy canvases (this database's canvas tables) into the linked Canvas Cloud,
    /// verifying each by digest. Legacy rows are never modified.
    Migrate(CanvasMigrateArgs),
}

#[derive(Args, Debug, Default)]
pub struct CanvasMigrateArgs {
    /// Read, decrypt and count everything without sending it.
    #[arg(long)]
    pub dry_run: bool,
    /// Only this canvas (repeatable); re-sends it even if already migrated.
    #[arg(long = "only", value_name = "ID")]
    pub only: Vec<uuid::Uuid>,
    /// Replace canvases that already exist on Canvas Cloud (from an earlier import).
    #[arg(long)]
    pub replace: bool,
    /// Canvases migrated in parallel.
    #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u16).range(1..=16))]
    pub concurrency: u16,
    /// Print the report as JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Subcommand, Debug)]
pub enum IntelligenceCommand {
    /// Set or update the keys and/or the agent round budget. Keys are read from files (or "-"
    /// for stdin), never argv.
    Set(IntelligenceSetArgs),
    /// Show the settings with keys masked (last 4 characters) and the agent round budget.
    Show,
    /// Remove the stored keys (both by default; the agent budget stays unless --agent).
    Clear {
        /// Only clear the Jev block.
        #[arg(long, conflicts_with = "router")]
        jev: bool,
        /// Only clear the router block.
        #[arg(long)]
        router: bool,
        /// Clear the org-wide agent round budget (each Copper uses its own setting again).
        #[arg(long)]
        agent: bool,
    },
    /// Hand the stored keys to Coppers (the default once keys are set).
    Enable,
    /// Stop handing out the keys (GET /v1/intelligence answers with nulls) without deleting them.
    Disable,
}

#[derive(Args, Debug, Default)]
pub struct IntelligenceSetArgs {
    /// File holding the Jev (TypeSafe) key; "-" reads stdin.
    #[arg(long, value_name = "FILE")]
    pub jev_key_file: Option<PathBuf>,
    /// Jev endpoint (default https://api.typesafe.ai/v1/systemone).
    #[arg(long, value_name = "URL")]
    pub jev_endpoint: Option<String>,
    /// Jev model (default jev-latest).
    #[arg(long, value_name = "MODEL")]
    pub jev_model: Option<String>,
    /// File holding the LLM router (LiteLLM) key; "-" reads stdin.
    #[arg(long, value_name = "FILE")]
    pub router_key_file: Option<PathBuf>,
    /// Router base URL, e.g. https://llm.example.com. Required the first time a router key is
    /// set (there is no default); later calls keep the stored URL unless this is given.
    #[arg(long, value_name = "URL")]
    pub router_url: Option<String>,
    /// Org-wide tool-call rounds per question for Copper's agent (1-500); overrides each
    /// Copper's own setting while set.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(i64).range(1..=500))]
    pub agent_max_turns: Option<i64>,
}

#[derive(Args, Debug, Default)]
pub struct ServeArgs {
    /// Do not apply pending migrations at startup.
    #[arg(long)]
    pub no_migrate: bool,
}

#[derive(Args, Debug)]
pub struct InitConfigArgs {
    /// Where to write the config (mode 0600).
    #[arg(long, value_name = "PATH")]
    pub write: PathBuf,
    /// Overwrite an existing file (generates NEW keys unless --instance-key/--master-key).
    #[arg(long)]
    pub force: bool,
    /// host[:port] clients connect to.
    #[arg(long, default_value = "localhost:8443")]
    pub public_url: String,
    /// Database URL (env COPPER_CLOUD_DATABASE_URL keeps passwords out of `ps`).
    #[arg(
        long,
        env = "COPPER_CLOUD_DATABASE_URL",
        hide_env_values = true,
        default_value = "postgres://localhost:5432/copper_cloud"
    )]
    pub database_url: String,
    #[arg(long, default_value = "0.0.0.0:8443")]
    pub listen: String,
    /// self-signed | acme | off
    #[arg(long, default_value = "self-signed", value_parser = parse_tls_mode)]
    pub tls_mode: TlsMode,
    /// Domain for ACME (implies the certificate name).
    #[arg(long)]
    pub domain: Option<String>,
    #[arg(long)]
    pub acme_email: Option<String>,
    /// Use this instance key instead of generating one (env COPPER_CLOUD_INSTANCE_KEY).
    #[arg(long, env = "COPPER_CLOUD_INSTANCE_KEY", hide_env_values = true)]
    pub instance_key: Option<String>,
    /// Use this master key instead of generating one (env COPPER_CLOUD_MASTER_KEY).
    #[arg(long, env = "COPPER_CLOUD_MASTER_KEY", hide_env_values = true)]
    pub master_key: Option<String>,
    /// allow_signup value written to the file.
    #[arg(
        long,
        default_value_t = true,
        action = clap::ArgAction::Set,
        value_parser = clap::builder::BoolishValueParser::new()
    )]
    pub allow_signup: bool,
    /// Certificate directory (default: `<config dir>/tls`).
    #[arg(long, value_name = "DIR")]
    pub tls_dir: Option<PathBuf>,
}

fn parse_tls_mode(s: &str) -> Result<TlsMode, String> {
    match s {
        "self-signed" | "selfsigned" | "self_signed" => Ok(TlsMode::SelfSigned),
        "acme" => Ok(TlsMode::Acme),
        "off" => Ok(TlsMode::Off),
        _ => Err("expected self-signed, acme or off".into()),
    }
}

#[derive(Subcommand, Debug)]
pub enum AdminCommand {
    /// List users.
    Users,
    /// Create a user (works even when signup is disabled).
    CreateUser {
        #[arg(long)]
        email: String,
        #[command(flatten)]
        password: PasswordArg,
        #[arg(long)]
        display_name: Option<String>,
    },
    /// Set a new password and revoke all of the user's sessions.
    ResetPassword {
        #[arg(long)]
        email: String,
        #[command(flatten)]
        password: PasswordArg,
    },
    /// Permanently delete a user and all their data.
    DeleteUser {
        #[arg(long)]
        email: String,
        /// Do not ask for confirmation.
        #[arg(long)]
        yes: bool,
    },
    /// Block sign-in for a user and revoke their sessions.
    DisableUser {
        #[arg(long)]
        email: String,
    },
    /// Re-enable a disabled user.
    EnableUser {
        #[arg(long)]
        email: String,
    },
    /// Turn self-service signup off (overrides allow_signup in the config).
    DisableSignup,
    /// Turn self-service signup on (overrides allow_signup in the config).
    EnableSignup,
    /// Create a portal admin account (signs in at https://HOST:PORT/).
    CreateAdmin {
        #[arg(long)]
        email: String,
        #[command(flatten)]
        password: PasswordArg,
        /// Succeed without changes if an admin with this email already exists.
        #[arg(long)]
        if_missing: bool,
    },
    /// Set a new password for a portal admin and sign out their sessions.
    ResetAdminPassword {
        #[arg(long)]
        email: String,
        #[command(flatten)]
        password: PasswordArg,
    },
    /// List portal admin accounts.
    ListAdmins,
    /// Delete a portal admin account.
    DeleteAdmin {
        #[arg(long)]
        email: String,
    },
    /// Who may pass the instance gate: open (shared instance key + access keys) or
    /// directory (personal access keys only).
    SetAccessMode {
        #[arg(value_parser = ["open", "directory"])]
        mode: String,
    },
    /// Print the current access mode.
    AccessMode,
    /// Mint a personal access key and print it with its link code (shown once).
    CreateAccessKey {
        #[arg(long)]
        label: String,
        /// Only this email may sign up with the key.
        #[arg(long)]
        email: Option<String>,
        #[arg(long)]
        expires_in_days: Option<i32>,
        /// Number of accounts the key may create.
        #[arg(long)]
        max_uses: Option<i32>,
    },
    /// A user's synced browsing history.
    #[command(subcommand)]
    History(HistoryCommand),
}

#[derive(Subcommand, Debug)]
pub enum HistoryCommand {
    /// Delete a user's synced history wholesale: all of it, or only the visits matching every
    /// metadata filter given (time window, device). Never reads history content; picking
    /// individual entries is up to the user in Copper. Prints the count only. Coppers keep their
    /// local history and see fewer rows on their next pull.
    Delete(HistoryDeleteArgs),
}

#[derive(Args, Debug, Clone)]
pub struct HistoryDeleteArgs {
    /// The user: email or user id.
    #[arg(long, value_name = "EMAIL|UUID")]
    pub user: String,
    /// Only visits at or after this time (RFC 3339, e.g. 2026-10-01T00:00:00Z).
    #[arg(long, value_name = "RFC3339", value_parser = parse_time)]
    pub since: Option<time::OffsetDateTime>,
    /// Only visits before this time (RFC 3339).
    #[arg(long, value_name = "RFC3339", value_parser = parse_time)]
    pub until: Option<time::OffsetDateTime>,
    /// Only rows synced from this device id.
    #[arg(long, value_name = "UUID")]
    pub device: Option<uuid::Uuid>,
    /// Only count what would be deleted.
    #[arg(long)]
    pub dry_run: bool,
}

fn parse_time(s: &str) -> Result<time::OffsetDateTime, String> {
    copper_cloud_core::sync::parse_history_time("the value", s).map_err(bad_value)
}

#[allow(clippy::needless_pass_by_value)] // used as `map_err(bad_value)`
fn bad_value(e: copper_cloud_core::error::ApiError) -> String {
    match e {
        copper_cloud_core::error::ApiError::BadRequest(msg) => msg,
        other => other.to_string(),
    }
}

#[derive(Args, Debug)]
pub struct PasswordArg {
    /// The password (visible in shell history/ps — prefer --password-stdin).
    #[arg(long, conflicts_with = "password_stdin")]
    pub password: Option<String>,
    /// Read the password from the first line of stdin.
    #[arg(long)]
    pub password_stdin: bool,
}

impl PasswordArg {
    pub fn resolve(&self) -> anyhow::Result<String> {
        if let Some(p) = &self.password {
            return Ok(p.clone());
        }
        if !self.password_stdin && std::io::stdin().is_terminal() {
            eprint!("Password: ");
        }
        let mut line = String::new();
        std::io::stdin()
            .lock()
            .read_line(&mut line)
            .context("reading password from stdin")?;
        let pw = line.trim_end_matches(['\r', '\n']).to_owned();
        if pw.is_empty() {
            bail!("empty password");
        }
        Ok(pw)
    }
}

/// Entry point used by `main`.
pub fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("copper-cloud")
        .build()
        .context("starting tokio runtime")
}

fn load_config(cli_path: Option<PathBuf>) -> anyhow::Result<Config> {
    Config::load(&ConfigSource::resolve(cli_path))
}

pub fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    copper_cloud_core::tls::install_default_provider();
    let command = cli.command.unwrap_or(Command::Serve(ServeArgs::default()));
    match command {
        Command::Version => {
            println!("copper-cloud {}", crate::version());
            Ok(ExitCode::SUCCESS)
        }
        Command::InitConfig(args) => {
            init_config(&args)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Serve(args) => {
            let cfg = load_config(cli.config)?;
            runtime()?.block_on(crate::serve::run(cfg, !args.no_migrate))?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Migrate => {
            let cfg = load_config(cli.config)?;
            runtime()?.block_on(async {
                let pool = copper_cloud_core::db::connect(&cfg).await?;
                copper_cloud_core::db::migrate(&pool).await?;
                let (applied, pending) = copper_cloud_core::db::pending_migrations(&pool).await?;
                println!("migrations applied: {applied}, pending: {}", pending.len());
                pool.close().await;
                anyhow::Ok(())
            })?;
            Ok(ExitCode::SUCCESS)
        }
        Command::LinkCode => {
            let cfg = load_config(cli.config)?;
            let code = copper_cloud_core::tls::link_code(&cfg).with_context(|| {
                if cfg.tls.mode == TlsMode::SelfSigned {
                    "no certificate yet — run `copper-cloud tls-init` (or start the server) first"
                } else {
                    "building link code"
                }
            })?;
            println!("{code}");
            Ok(ExitCode::SUCCESS)
        }
        Command::TlsInit { force, names } => {
            let cfg = load_config(cli.config)?;
            tls_init(&cfg, force, &names)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Doctor => {
            let ok = runtime()?.block_on(crate::doctor::run(&ConfigSource::resolve(cli.config)));
            Ok(if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
        Command::Healthcheck { wait } => {
            let cfg = load_config(cli.config)?;
            let ok = runtime()?.block_on(crate::doctor::wait_healthy(&cfg, wait));
            if ok {
                println!("healthy");
                Ok(ExitCode::SUCCESS)
            } else {
                eprintln!("not healthy: /healthz did not answer on {}", cfg.listen);
                Ok(ExitCode::FAILURE)
            }
        }
        Command::Admin(cmd) => {
            let cfg = load_config(cli.config)?;
            runtime()?.block_on(crate::admin::run(cfg, cmd))?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Intelligence(cmd) => {
            let cfg = load_config(cli.config)?;
            runtime()?.block_on(crate::admin::intelligence(cfg, cmd))?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Canvas(cmd) => {
            let cfg = load_config(cli.config)?;
            runtime()?.block_on(canvas(cfg, cmd))
        }
    }
}

// ---------------------------------------------------------------------------------------------
// `copper-cloud canvas …`

#[allow(clippy::needless_pass_by_value)] // used as `map_err(api_err)`
fn api_err(e: copper_cloud_core::error::ApiError) -> anyhow::Error {
    anyhow::anyhow!("{e}")
}

/// Read the app key from `path` (`-` = stdin), validated, never printed.
fn read_app_key(path: &std::path::Path) -> anyhow::Result<zeroize::Zeroizing<String>> {
    let raw = zeroize::Zeroizing::new(if path.as_os_str() == "-" {
        let mut s = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin().lock(), &mut s)
            .context("reading the app key from stdin")?;
        s
    } else {
        std::fs::read_to_string(path)
            .with_context(|| format!("reading the app key from {}", path.display()))?
    });
    copper_cloud_canvas_link::settings::clean_app_key(&raw).map_err(api_err)
}

async fn canvas(cfg: Config, cmd: CanvasCommand) -> anyhow::Result<ExitCode> {
    use copper_cloud_canvas_link::admin::{self as link_admin, Actor};
    use copper_cloud_canvas_link::{settings, CanvasLink};

    // Read the key before touching the database (fail fast).
    let key = match &cmd {
        CanvasCommand::Link { key_file, .. } => Some(read_app_key(key_file)?),
        _ => None,
    };
    let pool = copper_cloud_core::db::connect(&cfg).await?;
    let state = copper_cloud_core::state::AppState::new(pool.clone(), cfg);
    let link = CanvasLink::new(state.clone());
    let result = async {
        match cmd {
            CanvasCommand::Link {
                url,
                fingerprint,
                no_sync,
                ..
            } => {
                let key = key.unwrap_or_default();
                let active =
                    link_admin::set(&link, &Actor::Cli, &url, &key, fingerprint.as_deref())
                        .await
                        .map_err(api_err)?;
                let health = link.health();
                println!(
                    "linked {} (Canvas Cloud {}, key …{})",
                    active.settings.base(),
                    health.version.as_deref().unwrap_or("?"),
                    settings::last4(&active.settings.app_key)
                );
                if no_sync {
                    println!("users not provisioned (--no-sync); the server's reconcile loop will");
                    return Ok(ExitCode::SUCCESS);
                }
                sync_users(&link, &active).await
            }
            CanvasCommand::Unlink => {
                match link_admin::remove(&link, &Actor::Cli)
                    .await
                    .map_err(api_err)?
                {
                    Some(url) => println!("unlinked {url}"),
                    None => println!("no Canvas Cloud was linked"),
                }
                Ok(ExitCode::SUCCESS)
            }
            CanvasCommand::Status => {
                let view = link_admin::view(&link).await.map_err(api_err)?;
                print_status(&view);
                let reachable = view["status"]["reachable"].as_bool();
                Ok(if view["linked"] == true && reachable != Some(true) {
                    ExitCode::FAILURE
                } else {
                    ExitCode::SUCCESS
                })
            }
            CanvasCommand::SyncUsers { prune } => {
                let active = link
                    .current()
                    .await
                    .map_err(api_err)?
                    .context("no Canvas Cloud is linked (copper-cloud canvas link …)")?;
                let code = sync_users(&link, &active).await?;
                if !prune {
                    return Ok(code);
                }
                let (deleted, failed) =
                    copper_cloud_canvas_link::provision::prune_orphans(&link, &active).await?;
                println!(
                    "pruned {} Canvas Cloud user(s) with no Copper account",
                    deleted.len()
                );
                for (id, err) in &failed {
                    println!("  failed {id}: {err}");
                }
                Ok(if failed.is_empty() {
                    code
                } else {
                    ExitCode::FAILURE
                })
            }
            CanvasCommand::Migrate(args) => migrate_canvases(&state, &link, args).await,
        }
    }
    .await;
    pool.close().await;
    result
}

async fn migrate_canvases(
    state: &copper_cloud_core::state::SharedState,
    link: &copper_cloud_canvas_link::CanvasLink,
    args: CanvasMigrateArgs,
) -> anyhow::Result<ExitCode> {
    let active = link
        .current()
        .await
        .map_err(api_err)?
        .context("no Canvas Cloud is linked (copper-cloud canvas link …)")?;
    let opts = copper_cloud_canvas_migrate::MigrateOptions {
        dry_run: args.dry_run,
        only: args.only,
        replace: args.replace,
        concurrency: usize::from(args.concurrency),
        json: args.json,
    };
    let report = copper_cloud_canvas_migrate::run(
        &state.db,
        &state.crypto,
        active.upstream.client(),
        &active.settings.url,
        &active.settings.app_key,
        opts,
    )
    .await?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("{report}");
    }
    Ok(if report.ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

async fn sync_users(
    link: &copper_cloud_canvas_link::CanvasLink,
    active: &copper_cloud_canvas_link::Active,
) -> anyhow::Result<ExitCode> {
    let report = copper_cloud_canvas_link::provision::sync_all(link, active).await?;
    println!(
        "provisioned {}/{} users on Canvas Cloud",
        report.provisioned, report.total
    );
    for (id, err) in &report.failed {
        println!("  failed {id}: {err}");
    }
    for id in &report.id_mismatch {
        println!(
            "  {id}: Canvas Cloud linked an existing account with a different id (chat author ids will not match)"
        );
    }
    Ok(if report.ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn print_status(v: &serde_json::Value) {
    let s = |v: &serde_json::Value| v.as_str().map_or_else(|| "-".to_owned(), str::to_owned);
    if v["linked"] == true {
        println!("canvas link: {}", s(&v["url"]));
        println!("  enabled:      {}", v["enabled"]);
        println!("  pinned cert:  {}", s(&v["tls_fingerprint"]));
        println!("  linked at:    {}", s(&v["linked_at"]));
        println!(
            "  updated:      {} by {}",
            s(&v["updated_at"]),
            s(&v["updated_by"])
        );
        match v["status"]["reachable"].as_bool() {
            Some(true) => println!(
                "  reachable:    yes (Canvas Cloud {})",
                s(&v["status"]["version"])
            ),
            Some(false) => println!("  reachable:    NO — {}", s(&v["status"]["error"])),
            None => println!("  reachable:    unknown"),
        }
    } else {
        println!("canvas link: not linked (canvas routes answer 503 canvas_unavailable)");
    }
    let m = &v["migrations"];
    println!(
        "  migrations:   {} migrated, {} failed, {} legacy canvases",
        m["migrated"], m["failed"], m["legacy_canvases"]
    );
}

fn tls_init(cfg: &Config, force: bool, extra: &[String]) -> anyhow::Result<()> {
    if cfg.tls.mode != TlsMode::SelfSigned {
        println!("tls.mode = {}: nothing to generate", cfg.tls.mode);
        return Ok(());
    }
    let exists = cfg.tls.cert_path.exists() && cfg.tls.key_path.exists();
    if exists && !force {
        println!(
            "certificate exists: {} (fp={})",
            cfg.tls.cert_path.display(),
            copper_cloud_core::tls::leaf_fingerprint(&cfg.tls.cert_path)?
        );
        return Ok(());
    }
    let mut names = copper_cloud_core::tls::self_signed_names(cfg);
    for n in extra {
        let n = n.trim().to_ascii_lowercase();
        if !n.is_empty() && !names.contains(&n) {
            names.push(n);
        }
    }
    copper_cloud_core::tls::generate_self_signed(&names, &cfg.tls.cert_path, &cfg.tls.key_path)?;
    println!(
        "generated self-signed certificate {} for {} (fp={})",
        cfg.tls.cert_path.display(),
        names.join(", "),
        copper_cloud_core::tls::leaf_fingerprint(&cfg.tls.cert_path)?
    );
    Ok(())
}

fn init_config(args: &InitConfigArgs) -> anyhow::Result<()> {
    if args.write.exists() && !args.force {
        bail!(
            "{} already exists (use --force to overwrite; that generates new keys)",
            args.write.display()
        );
    }
    let dir = args
        .write
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), std::path::Path::to_path_buf);
    let tls_dir = args.tls_dir.clone().unwrap_or_else(|| dir.join("tls"));
    let text = config::generate_toml(&GenerateOptions {
        public_url: args.public_url.clone(),
        database_url: args.database_url.clone(),
        listen: args.listen.clone(),
        tls_mode: args.tls_mode,
        domain: args.domain.clone(),
        acme_email: args.acme_email.clone(),
        instance_key: args.instance_key.clone().filter(|s| !s.trim().is_empty()),
        master_key: args.master_key.clone().filter(|s| !s.trim().is_empty()),
        allow_signup: args.allow_signup,
        cert_path: tls_dir.join("cert.pem"),
        key_path: tls_dir.join("key.pem"),
    })?;
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    write_private(&args.write, text.as_bytes())?;
    println!("wrote {}", args.write.display());
    Ok(())
}

fn write_private(path: &std::path::Path, contents: &[u8]) -> anyhow::Result<()> {
    use std::io::Write as _;
    let tmp = path.with_extension("tmp");
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    let mut f = opts
        .open(&tmp)
        .with_context(|| format!("writing {}", tmp.display()))?;
    f.write_all(contents)?;
    f.sync_all()?;
    drop(f);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, path).with_context(|| format!("installing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory as _;

    #[test]
    fn cli_is_well_formed() {
        Cli::command().debug_assert();
    }

    #[test]
    fn default_command_is_serve() {
        let cli = Cli::try_parse_from(["copper-cloud", "--config", "/tmp/x.toml"]).unwrap();
        assert!(cli.command.is_none());
        let cli = Cli::try_parse_from([
            "copper-cloud",
            "admin",
            "create-user",
            "--email",
            "a@b.co",
            "--password",
            "x",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Admin(AdminCommand::CreateUser { .. }))
        ));
        let cli = Cli::try_parse_from([
            "copper-cloud",
            "admin",
            "create-admin",
            "--email",
            "admin@example.com",
            "--password-stdin",
            "--if-missing",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Admin(AdminCommand::CreateAdmin {
                if_missing: true,
                ..
            }))
        ));
        assert!(
            Cli::try_parse_from(["copper-cloud", "admin", "set-access-mode", "closed"]).is_err()
        );
        assert!(
            Cli::try_parse_from(["copper-cloud", "admin", "set-access-mode", "directory"]).is_ok()
        );
        let cli = Cli::try_parse_from([
            "copper-cloud",
            "intelligence",
            "set",
            "--jev-key-file",
            "/tmp/j",
            "--router-key-file",
            "-",
            "--router-url",
            "https://llm.example.com",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Intelligence(IntelligenceCommand::Set(_)))
        ));
        // Keys are never accepted on argv.
        assert!(
            Cli::try_parse_from(["copper-cloud", "intelligence", "set", "--jev-key", "x"]).is_err()
        );
        assert!(Cli::try_parse_from([
            "copper-cloud",
            "intelligence",
            "clear",
            "--jev",
            "--router"
        ])
        .is_err());
    }

    #[test]
    fn admin_history_delete_flags() {
        let cli = Cli::try_parse_from([
            "copper-cloud",
            "admin",
            "history",
            "delete",
            "--user",
            "ada@example.com",
            "--since",
            "2026-10-01T00:00:00Z",
            "--until",
            "2026-10-02T00:00:00+02:00",
            "--device",
            "00000000-0000-0000-0000-000000000001",
            "--dry-run",
        ])
        .unwrap();
        let Some(Command::Admin(AdminCommand::History(HistoryCommand::Delete(a)))) = cli.command
        else {
            panic!("parsed as {:?}", cli.command);
        };
        assert_eq!(a.user, "ada@example.com");
        assert_eq!(a.since.unwrap().unix_timestamp(), 1_790_812_800);
        assert_eq!(a.until.unwrap().unix_timestamp(), 1_790_812_800 + 79_200);
        assert_eq!(a.device, Some(uuid::Uuid::from_u128(1)));
        assert!(a.dry_run);
        let parse = |extra: &[&str]| {
            let mut args = vec!["copper-cloud", "admin", "history", "delete"];
            args.extend_from_slice(extra);
            Cli::try_parse_from(args)
        };
        assert!(parse(&["--user", "a@b.co"]).is_ok());
        assert!(parse(&[]).is_err(), "--user is required");
        for bad in [
            ["--since", "yesterday"],
            ["--until", "1790812800"],
            // No content filters: the server never inspects history to delete.
            ["--host", "x.com"],
            ["--url", "https://x.com"],
            ["--device", "me"],
        ] {
            let mut args = vec!["--user", "a@b.co"];
            args.extend_from_slice(&bad);
            assert!(parse(&args).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn canvas_flags() {
        let cli = Cli::try_parse_from([
            "copper-cloud",
            "canvas",
            "link",
            "--url",
            "http://127.0.0.1:8480",
            "--key-file",
            "-",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Canvas(CanvasCommand::Link { no_sync: false, .. }))
        ));
        // Keys are never accepted on argv.
        assert!(Cli::try_parse_from([
            "copper-cloud",
            "canvas",
            "link",
            "--url",
            "https://c.example",
            "--key",
            "cva_x"
        ])
        .is_err());
        let cli = Cli::try_parse_from([
            "copper-cloud",
            "canvas",
            "migrate",
            "--dry-run",
            "--only",
            "00000000-0000-0000-0000-000000000001",
            "--only",
            "00000000-0000-0000-0000-000000000002",
            "--concurrency",
            "4",
            "--json",
        ])
        .unwrap();
        let Some(Command::Canvas(CanvasCommand::Migrate(a))) = cli.command else {
            panic!("parsed as {:?}", cli.command);
        };
        assert!(a.dry_run && a.json && !a.replace);
        assert_eq!(a.only.len(), 2);
        assert_eq!(a.concurrency, 4);
        for sub in ["unlink", "status", "sync-users"] {
            assert!(
                Cli::try_parse_from(["copper-cloud", "canvas", sub]).is_ok(),
                "{sub}"
            );
        }
        assert!(
            Cli::try_parse_from(["copper-cloud", "canvas", "migrate", "--concurrency", "0"])
                .is_err()
        );
    }

    #[test]
    fn intelligence_agent_flags() {
        // Agent round budget: 1..=500 on argv, cleared with `clear --agent`.
        let cli = Cli::try_parse_from([
            "copper-cloud",
            "intelligence",
            "set",
            "--agent-max-turns",
            "100",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Intelligence(IntelligenceCommand::Set(
                IntelligenceSetArgs {
                    agent_max_turns: Some(100),
                    ..
                }
            )))
        ));
        for bad in ["0", "501", "-1", "1.5", "x"] {
            assert!(
                Cli::try_parse_from([
                    "copper-cloud",
                    "intelligence",
                    "set",
                    "--agent-max-turns",
                    bad
                ])
                .is_err(),
                "{bad}"
            );
        }
        let cli =
            Cli::try_parse_from(["copper-cloud", "intelligence", "clear", "--agent"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Intelligence(IntelligenceCommand::Clear {
                agent: true,
                jev: false,
                router: false
            }))
        ));
    }
}
