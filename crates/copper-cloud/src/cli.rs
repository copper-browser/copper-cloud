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
    /// Router base URL (default https://llm.example.com).
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
    }
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
