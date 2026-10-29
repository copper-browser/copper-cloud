//! `copper-cloud admin …` — user, admin-account, access-mode and signup administration
//! straight against the database.

use std::io::{BufRead as _, IsTerminal as _, Write as _};

use anyhow::{bail, Context as _};
use copper_cloud_core::access::{self, AccessMode, NewAccessKey};
use copper_cloud_core::auth;
use copper_cloud_core::config::Config;
use copper_cloud_core::crypto::Crypto;
use copper_cloud_core::db;
use time::OffsetDateTime;
use uuid::Uuid;

use copper_cloud_core::intelligence;

use crate::cli::{
    AdminCommand, DocsCommand, HistoryCommand, HistoryDeleteArgs, IntegrationCommand,
    IntelligenceCommand, SsoCommand,
};
use copper_cloud_core::{agents, sso};

pub async fn run(cfg: Config, cmd: AdminCommand) -> anyhow::Result<()> {
    let pool = db::connect(&cfg).await?;
    let result = dispatch(&cfg, &pool, cmd).await;
    pool.close().await;
    result
}

async fn find_user(pool: &sqlx::PgPool, email: &str) -> anyhow::Result<Uuid> {
    auth::user_id_by_email(pool, email)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .with_context(|| format!("no user with email {email}"))
}

#[allow(clippy::needless_pass_by_value)] // used as `map_err(api)`
fn api(e: copper_cloud_core::error::ApiError) -> anyhow::Error {
    anyhow::anyhow!("{e}")
}

#[allow(clippy::too_many_lines)]
async fn dispatch(cfg: &Config, pool: &sqlx::PgPool, cmd: AdminCommand) -> anyhow::Result<()> {
    match cmd {
        AdminCommand::Users => {
            let rows: Vec<(
                String,
                String,
                OffsetDateTime,
                bool,
                i64,
                Option<OffsetDateTime>,
            )> = sqlx::query_as(
                "SELECT u.email, u.display_name, u.created_at, u.disabled,
                            (SELECT count(*) FROM devices d WHERE d.user_id = u.id),
                            (SELECT max(d.last_seen_at) FROM devices d WHERE d.user_id = u.id)
                     FROM users u ORDER BY u.created_at",
            )
            .fetch_all(pool)
            .await?;
            println!(
                "{:<36} {:<24} {:<10} {:>7}  {:<20} {:<20}",
                "EMAIL", "NAME", "STATUS", "DEVICES", "CREATED", "LAST SEEN"
            );
            for (email, name, created, disabled, devices, seen) in &rows {
                println!(
                    "{:<36} {:<24} {:<10} {:>7}  {:<20} {:<20}",
                    email,
                    name,
                    if *disabled { "disabled" } else { "active" },
                    devices,
                    fmt_time(*created),
                    seen.map_or_else(|| "-".to_owned(), fmt_time)
                );
            }
            println!("{} user(s)", rows.len());
        }
        AdminCommand::CreateUser {
            email,
            password,
            display_name,
        } => {
            let email = auth::normalize_email(&email).map_err(api)?;
            let password = password.resolve()?;
            auth::validate_password(&password).map_err(api)?;
            let display_name = auth::clean_name(display_name.as_deref(), 200)
                .unwrap_or_else(|| auth::default_display_name(&email));
            let hash = auth::hash_password(password).await.map_err(api)?;
            let crypto = Crypto::from_master_key(&cfg.master_key);
            let mut conn = pool.acquire().await?;
            let user = auth::insert_user(&mut conn, &crypto, &email, &display_name, Some(&hash))
                .await
                .map_err(api)?;
            println!("created user {} ({})", user.email, user.id);
        }
        AdminCommand::ResetPassword { email, password } => {
            let id = find_user(pool, &email).await?;
            let password = password.resolve()?;
            auth::validate_password(&password).map_err(api)?;
            let hash = auth::hash_password(password).await.map_err(api)?;
            let mut tx = pool.begin().await?;
            sqlx::query("UPDATE users SET password_hash = $2, updated_at = now() WHERE id = $1")
                .bind(id)
                .bind(&hash)
                .execute(&mut *tx)
                .await?;
            let revoked = sqlx::query("DELETE FROM sessions WHERE user_id = $1")
                .bind(id)
                .execute(&mut *tx)
                .await?
                .rows_affected();
            tx.commit().await?;
            println!("password reset for {email}; revoked {revoked} session(s)");
        }
        AdminCommand::DeleteUser { email, yes } => {
            let id = find_user(pool, &email).await?;
            if !yes && !confirm(&format!("Delete {email} and ALL their synced data? [y/N] "))? {
                bail!("aborted");
            }
            sqlx::query("DELETE FROM users WHERE id = $1")
                .bind(id)
                .execute(pool)
                .await?;
            println!("deleted {email}");
        }
        AdminCommand::DisableUser { email } => {
            let id = find_user(pool, &email).await?;
            let mut tx = pool.begin().await?;
            sqlx::query("UPDATE users SET disabled = true, updated_at = now() WHERE id = $1")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            let revoked = sqlx::query("DELETE FROM sessions WHERE user_id = $1")
                .bind(id)
                .execute(&mut *tx)
                .await?
                .rows_affected();
            tx.commit().await?;
            println!("disabled {email}; revoked {revoked} session(s)");
        }
        AdminCommand::EnableUser { email } => {
            let id = find_user(pool, &email).await?;
            sqlx::query("UPDATE users SET disabled = false, updated_at = now() WHERE id = $1")
                .bind(id)
                .execute(pool)
                .await?;
            println!("enabled {email}");
        }
        AdminCommand::DisableSignup | AdminCommand::EnableSignup => {
            let on = matches!(cmd, AdminCommand::EnableSignup);
            sqlx::query(
                "INSERT INTO server_settings (key, value) VALUES ('allow_signup', $1)
                 ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = now()",
            )
            .bind(serde_json::Value::Bool(on))
            .execute(pool)
            .await?;
            println!(
                "signup {} (the first user can always sign up)",
                if on { "enabled" } else { "disabled" }
            );
        }
        AdminCommand::CreateAdmin {
            email,
            password,
            if_missing,
        } => {
            if let Some(existing) = crate::admin_api::find_admin(pool, &email)
                .await
                .map_err(api)?
            {
                if if_missing {
                    println!("admin {} already exists", existing.email);
                    return Ok(());
                }
                bail!("an admin with email {} already exists", existing.email);
            }
            let password = password.resolve()?;
            let admin = crate::admin_api::create_admin(pool, &email, password)
                .await
                .map_err(api)?;
            println!("created admin {} ({})", admin.email, admin.id);
        }
        AdminCommand::ResetAdminPassword { email, password } => {
            let admin = crate::admin_api::find_admin(pool, &email)
                .await
                .map_err(api)?
                .with_context(|| format!("no admin with email {email}"))?;
            let password = password.resolve()?;
            let revoked = crate::admin_api::set_admin_password(pool, admin.id, password, None)
                .await
                .map_err(api)?;
            println!(
                "password reset for admin {}; revoked {revoked} session(s)",
                admin.email
            );
        }
        AdminCommand::ListAdmins => {
            let admins = crate::admin_api::list_admins(pool).await.map_err(api)?;
            println!("{:<40} {:<20} {:<20}", "EMAIL", "CREATED", "LAST LOGIN");
            for a in &admins {
                println!(
                    "{:<40} {:<20} {:<20}",
                    a.email,
                    fmt_time(a.created_at),
                    a.last_login_at.map_or_else(|| "-".to_owned(), fmt_time)
                );
            }
            println!("{} admin(s)", admins.len());
        }
        AdminCommand::DeleteAdmin { email } => {
            let deleted = sqlx::query("DELETE FROM admins WHERE lower(email) = lower($1)")
                .bind(email.trim())
                .execute(pool)
                .await?
                .rows_affected();
            if deleted == 0 {
                bail!("no admin with email {email}");
            }
            println!("deleted admin {email}");
        }
        AdminCommand::SetAccessMode { mode } => {
            let mode = AccessMode::parse(&mode).context("mode must be open or directory")?;
            access::set_access_mode(pool, mode).await.map_err(api)?;
            println!("access mode: {mode} (running servers follow within 5 s)");
        }
        AdminCommand::AccessMode => {
            let mode = access::load_access_mode(pool).await.map_err(api)?;
            println!("{mode}");
        }
        AdminCommand::CreateAccessKey {
            label,
            email,
            expires_in_days,
            max_uses,
        } => {
            let email = email
                .as_deref()
                .map(auth::normalize_email)
                .transpose()
                .map_err(api)?;
            let mut conn = pool.acquire().await?;
            let (id, key) = access::mint_access_key(
                &mut conn,
                &NewAccessKey {
                    label: &label,
                    email: email.as_deref(),
                    expires_in_days,
                    max_uses,
                    ..NewAccessKey::default()
                },
            )
            .await
            .map_err(api)?;
            eprintln!("created access key {id} ({label}); shown once:");
            println!("{key}");
            match copper_cloud_core::tls::link_code(cfg) {
                Ok(code) => println!("{}", code.with_key(&key)),
                Err(err) => eprintln!("(no link code: {err:#})"),
            }
        }
        AdminCommand::Sso(cmd) => sso_command(cfg, pool, cmd).await?,
        AdminCommand::Integration(cmd) => integration_command(pool, cmd).await?,
        AdminCommand::SetInstanceName { name } => {
            let name = sso::set_instance_name(pool, Some(&name))
                .await
                .map_err(api)?;
            println!("instance name: {name}");
        }
        AdminCommand::History(HistoryCommand::Delete(args)) => {
            // Counts only: never print history content.
            let (email, n) = history_delete(pool, &args).await?;
            let entries = format!("{n} history {}", if n == 1 { "entry" } else { "entries" });
            if args.dry_run {
                println!("dry run: would delete {entries} of {email}");
            } else {
                println!("deleted {entries} of {email}");
            }
        }
        AdminCommand::Docs(cmd) => docs_command(pool, cmd).await?,
        AdminCommand::Addons(cmd) => crate::admin_addons_cli::dispatch(cfg, pool, cmd).await?,
    }
    Ok(())
}

/// `admin docs …`: kept revisions of a user's sync doc (see `sync_doc_revisions`).
pub async fn docs_command(pool: &sqlx::PgPool, cmd: DocsCommand) -> anyhow::Result<()> {
    use copper_cloud_core::sync::{self, Domain};
    match cmd {
        DocsCommand::Revisions { user, domain } => {
            let (user_id, email) = resolve_user(pool, &user).await?;
            let domain = Domain::parse(&domain).map_err(api)?.as_str();
            let revs = sync::list_revisions(pool, user_id, &domain)
                .await
                .map_err(api)?;
            if revs.is_empty() {
                println!("no kept revisions of {domain} for {email}");
            }
            for r in revs {
                println!(
                    "v{}\twritten {}\treplaced {}\t{} bytes\tdevice {}\tbuild {}",
                    r.version,
                    fmt_time(r.updated_at),
                    fmt_time(r.replaced_at),
                    r.bytes,
                    r.device_id.map_or_else(|| "-".into(), |d| d.to_string()),
                    r.client_build.as_deref().unwrap_or("-"),
                );
            }
        }
        DocsCommand::Restore {
            user,
            domain,
            version,
        } => {
            let (user_id, email) = resolve_user(pool, &user).await?;
            let domain = Domain::parse(&domain).map_err(api)?.as_str();
            let (new, _) = sync::restore_revision(pool, user_id, &domain, version, None)
                .await
                .map_err(|e| anyhow::anyhow!("restoring v{version} of {domain}: {e}"))?;
            println!("restored {domain} v{version} of {email} as v{new}");
        }
    }
    Ok(())
}

/// `--user`: a user id or an email (case-insensitive) → (id, email).
async fn resolve_user(pool: &sqlx::PgPool, user: &str) -> anyhow::Result<(Uuid, String)> {
    let row: Option<(Uuid, String)> = match Uuid::parse_str(user.trim()) {
        Ok(id) => {
            sqlx::query_as("SELECT id, email FROM users WHERE id = $1")
                .bind(id)
                .fetch_optional(pool)
                .await?
        }
        Err(_) => {
            sqlx::query_as("SELECT id, email FROM users WHERE lower(email) = lower($1)")
                .bind(user.trim())
                .fetch_optional(pool)
                .await?
        }
    };
    row.with_context(|| format!("no user {user}"))
}

/// `admin history delete`: resolves `--user` (email or id) and deletes or, with `--dry-run`,
/// counts their history rows matching the metadata filter (`--since` / `--until` /
/// `--device`; none = everything). Wholesale only: no payload is opened and no data key is
/// unwrapped. Returns the user's email and the count. Running servers do not learn about it
/// (no `history_deleted` event); Coppers see fewer rows on their next pull.
pub async fn history_delete(
    pool: &sqlx::PgPool,
    args: &HistoryDeleteArgs,
) -> anyhow::Result<(String, u64)> {
    use copper_cloud_core::sync::{HistoryFilter, HistorySelection};
    let filter = HistoryFilter::new(args.since, args.until, args.device).map_err(api)?;
    let (user_id, email) = resolve_user(pool, &args.user).await?;
    let n = copper_cloud_core::sync::delete_history(
        pool,
        user_id,
        &HistorySelection::Filter(filter),
        args.dry_run,
    )
    .await
    .map_err(api)?;
    Ok((email, n))
}

fn fmt_time(t: OffsetDateTime) -> String {
    let f = time::macros::format_description!("[year]-[month]-[day] [hour]:[minute]");
    t.format(&f).unwrap_or_default()
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

// ---------------------------------------------------------------------------------------------
// `copper-cloud intelligence …`

/// Read a key from `path` (`-` = stdin). The value is validated, never printed.
fn read_key_file(path: &std::path::Path, what: &str) -> anyhow::Result<String> {
    let raw = if path.as_os_str() == "-" {
        let mut s = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin().lock(), &mut s)
            .with_context(|| format!("reading the {what} from stdin"))?;
        s
    } else {
        std::fs::read_to_string(path)
            .with_context(|| format!("reading the {what} from {}", path.display()))?
    };
    let raw = zeroize::Zeroizing::new(raw);
    intelligence::clean_key(&raw, what).map_err(api)
}

fn print_intelligence(s: &intelligence::Settings) {
    let masked = |last4: String| {
        if last4.is_empty() {
            "set (too short to show any characters)".to_owned()
        } else {
            format!("…{last4}")
        }
    };
    println!(
        "sharing:  {}",
        if s.enabled {
            "on (signed-in Coppers receive the keys)"
        } else {
            "off (GET /v1/intelligence answers with nulls)"
        }
    );
    match &s.jev {
        Some(j) => println!(
            "jev:      key {}  endpoint {}  model {}",
            masked(intelligence::last4(&j.key)),
            j.endpoint,
            j.model
        ),
        None => println!("jev:      not set"),
    }
    match &s.router {
        Some(r) => println!(
            "router:   key {}  url {}",
            masked(intelligence::last4(&r.key)),
            r.url
        ),
        None => println!("router:   not set"),
    }
    match s.agent_max_turns {
        Some(n) => println!(
            "agent:    {n} tool-call rounds per question (whole org; served even when sharing is off)"
        ),
        None => println!(
            "agent:    not set (each Copper uses its own setting, default {})",
            intelligence::DEFAULT_AGENT_MAX_TURNS
        ),
    }
    if let Some(at) = s.updated_at {
        println!(
            "updated:  {} by {}",
            fmt_time(at),
            s.updated_by.as_deref().unwrap_or("-")
        );
    }
}

pub async fn intelligence(cfg: Config, cmd: IntelligenceCommand) -> anyhow::Result<()> {
    use intelligence::{AgentInput, Change, JevInput, RouterInput, Update};

    // Read key files before touching the database (fail fast, stdin read once).
    let update = match &cmd {
        IntelligenceCommand::Set(a) => {
            let stdin_users = [&a.jev_key_file, &a.router_key_file]
                .iter()
                .filter(|p| p.as_deref().is_some_and(|p| p.as_os_str() == "-"))
                .count();
            if stdin_users > 1 {
                bail!("only one of --jev-key-file / --router-key-file can be \"-\" (stdin)");
            }
            let jev_key = a
                .jev_key_file
                .as_deref()
                .map(|p| read_key_file(p, "jev key"))
                .transpose()?;
            let router_key = a
                .router_key_file
                .as_deref()
                .map(|p| read_key_file(p, "router key"))
                .transpose()?;
            let jev = if jev_key.is_some() || a.jev_endpoint.is_some() || a.jev_model.is_some() {
                Change::Set(JevInput {
                    key: jev_key,
                    endpoint: a.jev_endpoint.clone(),
                    model: a.jev_model.clone(),
                })
            } else {
                Change::Keep
            };
            let router = if router_key.is_some() || a.router_url.is_some() {
                Change::Set(RouterInput {
                    key: router_key,
                    url: a.router_url.clone(),
                })
            } else {
                Change::Keep
            };
            let agent = a.agent_max_turns.map_or(Change::Keep, |max_turns| {
                Change::Set(AgentInput { max_turns })
            });
            let u = Update {
                jev,
                router,
                agent,
                enabled: None,
            };
            if u.is_noop() {
                bail!("nothing to set: pass --jev-key-file, --router-key-file and/or --agent-max-turns (or a URL/model to change)");
            }
            Some(u)
        }
        IntelligenceCommand::Clear { jev, router, agent } if *jev || *router || *agent => {
            Some(Update {
                jev: if *jev { Change::Clear } else { Change::Keep },
                router: if *router { Change::Clear } else { Change::Keep },
                agent: if *agent { Change::Clear } else { Change::Keep },
                enabled: None,
            })
        }
        IntelligenceCommand::Enable => Some(Update {
            enabled: Some(true),
            ..Update::default()
        }),
        IntelligenceCommand::Disable => Some(Update {
            enabled: Some(false),
            ..Update::default()
        }),
        IntelligenceCommand::Show | IntelligenceCommand::Clear { .. } => None,
    };

    let pool = db::connect(&cfg).await?;
    let crypto = Crypto::from_master_key(&cfg.master_key);
    let actor = intelligence::Actor::Cli;
    let result = async {
        let settings = match (&cmd, update) {
            (_, Some(u)) => intelligence::apply(&pool, &crypto, &actor, &u)
                .await
                .map_err(api)?,
            (IntelligenceCommand::Clear { .. }, None) => {
                let removed = intelligence::clear(&pool, &actor).await.map_err(api)?;
                println!(
                    "{}",
                    if removed {
                        "cleared"
                    } else {
                        "no keys were set"
                    }
                );
                intelligence::load(&pool, &crypto).await.map_err(api)?
            }
            _ => intelligence::load(&pool, &crypto).await.map_err(api)?,
        };
        print_intelligence(&settings);
        anyhow::Ok(())
    }
    .await;
    pool.close().await;
    result
}

/// The first line of stdin, for `--client-secret-stdin` (never argv).
fn read_secret_line(what: &str) -> anyhow::Result<zeroize::Zeroizing<String>> {
    if std::io::stdin().is_terminal() {
        eprint!("{what}: ");
    }
    let mut line = zeroize::Zeroizing::new(String::new());
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .with_context(|| format!("reading the {what} from stdin"))?;
    let secret = zeroize::Zeroizing::new(line.trim().to_owned());
    if secret.is_empty() {
        bail!("empty {what} on stdin");
    }
    Ok(secret)
}

/// `copper-cloud link-code --sso`: the join link on stdout, the /start URL on stderr.
pub async fn sso_link_code(cfg: &Config) -> anyhow::Result<()> {
    let pool = db::connect(cfg).await?;
    let provider = sso::primary_provider(&pool).await.map_err(api);
    pool.close().await;
    let Some(provider) = provider? else {
        bail!("single sign-on is not set up — run `copper-cloud admin sso set-google …` first (docs/sso.md)");
    };
    println!("{}", sso::join_link(cfg, &provider.id));
    eprintln!(
        "share {} (install steps + this link); provider: {}",
        sso::start_url(cfg),
        provider.id
    );
    Ok(())
}

async fn print_sso(cfg: &Config, pool: &sqlx::PgPool) -> anyhow::Result<()> {
    let providers = sso::list_providers(pool).await.map_err(api)?;
    let password = sso::password_signin(pool).await.map_err(api)?;
    println!(
        "password sign-in: {}",
        if password {
            "on"
        } else {
            "off (single sign-on only; admins unaffected)"
        }
    );
    if providers.is_empty() {
        println!("providers:        none (copper-cloud admin sso set-google …)");
        return Ok(());
    }
    for p in &providers {
        let secret = match (p.has_secret, p.kind) {
            (true, _) => "stored",
            (false, sso::ProviderKind::Oidc) => "none (public client)",
            (false, sso::ProviderKind::Google) => "MISSING",
        };
        println!();
        println!("provider:    {} ({})", p.id, p.kind.as_str());
        println!("  name:        {}", p.name);
        println!(
            "  status:      {}",
            if p.enabled { "enabled" } else { "disabled" }
        );
        println!("  client id:   {}", p.client_id);
        println!("  secret:      {secret}");
        if let Some(issuer) = &p.issuer {
            println!("  issuer:      {issuer}");
        }
        println!("  domains:     {}", p.domains.join(", "));
        println!(
            "  new users:   {}",
            if p.auto_create {
                "created on first sign-in"
            } else {
                "existing accounts only"
            }
        );
        println!(
            "  updated:     {} by {}",
            fmt_time(p.updated_at),
            p.updated_by.as_deref().unwrap_or("-")
        );
    }
    if let Some(p) = providers.iter().find(|p| p.usable()) {
        println!();
        println!("join link:   {}", sso::join_link(cfg, &p.id));
        println!("share:       {}", sso::start_url(cfg));
    }
    Ok(())
}

async fn sso_command(cfg: &Config, pool: &sqlx::PgPool, cmd: SsoCommand) -> anyhow::Result<()> {
    let actor = intelligence::Actor::Cli;
    let crypto = Crypto::from_master_key(&cfg.master_key);
    match cmd {
        SsoCommand::Show => {}
        SsoCommand::SetGoogle {
            client_id,
            client_secret_stdin,
            domains,
            no_auto_create,
        } => {
            let secret = client_secret_stdin
                .then(|| read_secret_line("client secret"))
                .transpose()?;
            let input = sso::ProviderInput {
                kind: sso::ProviderKind::Google,
                name: None,
                client_id,
                client_secret: secret.as_deref().cloned(),
                issuer: None,
                domains,
                auto_create: Some(!no_auto_create),
                enabled: Some(true),
            };
            sso::put_provider(pool, cfg, &crypto, &actor, "google", input)
                .await
                .map_err(api)?;
            eprintln!("saved provider google");
        }
        SsoCommand::SetOidc {
            id,
            issuer,
            client_id,
            client_secret_stdin,
            name,
            domains,
            no_auto_create,
        } => {
            let secret = client_secret_stdin
                .then(|| read_secret_line("client secret"))
                .transpose()?;
            let input = sso::ProviderInput {
                kind: sso::ProviderKind::Oidc,
                name,
                client_id,
                client_secret: secret.as_deref().cloned(),
                issuer: Some(issuer),
                domains,
                auto_create: Some(!no_auto_create),
                enabled: Some(true),
            };
            sso::put_provider(pool, cfg, &crypto, &actor, &id, input)
                .await
                .map_err(api)?;
            eprintln!("saved provider {id}");
        }
        SsoCommand::Disable { id } => {
            let n = sso::set_providers_enabled(pool, &actor, id.as_deref(), false)
                .await
                .map_err(api)?;
            eprintln!("disabled {n} provider(s)");
        }
        SsoCommand::Enable { id } => {
            let n = sso::set_providers_enabled(pool, &actor, id.as_deref(), true)
                .await
                .map_err(api)?;
            eprintln!("enabled {n} provider(s)");
        }
        SsoCommand::Delete { id } => {
            if !sso::delete_provider(pool, &actor, &id).await.map_err(api)? {
                bail!("no provider {id}");
            }
            eprintln!("deleted provider {id}");
        }
        SsoCommand::PasswordSignin { state } => {
            sso::set_password_signin(pool, &actor, state == "on")
                .await
                .map_err(api)?;
        }
    }
    print_sso(cfg, pool).await
}

async fn integration_command(pool: &sqlx::PgPool, cmd: IntegrationCommand) -> anyhow::Result<()> {
    let actor = intelligence::Actor::Cli;
    match cmd {
        IntegrationCommand::Create {
            name,
            domains,
            no_create_users,
        } => {
            let (view, key) =
                agents::create_integration(pool, &actor, &name, &domains, !no_create_users)
                    .await
                    .map_err(api)?;
            eprintln!(
                "integration {} ({}) — the key below is shown once; store it as a secret:",
                view.id, view.name
            );
            println!("{key}");
        }
        IntegrationCommand::List => {
            let list = agents::list_integrations(pool).await.map_err(api)?;
            println!(
                "{:<36} {:<24} {:<8} {:>7}  {:<20} DOMAINS",
                "ID", "NAME", "STATUS", "DEVICES", "LAST USED"
            );
            for i in &list {
                println!(
                    "{:<36} {:<24} {:<8} {:>7}  {:<20} {}",
                    i.id,
                    i.name,
                    if i.revoked_at.is_some() {
                        "revoked"
                    } else {
                        "active"
                    },
                    i.device_count,
                    i.last_used_at.map_or_else(|| "-".to_owned(), fmt_time),
                    if i.domains.is_empty() {
                        "(single sign-on domains)".to_owned()
                    } else {
                        i.domains.join(",")
                    }
                );
            }
            println!("{} integration(s)", list.len());
        }
        IntegrationCommand::Rotate { id } => {
            let key = agents::rotate_integration(pool, &actor, id)
                .await
                .map_err(api)?;
            eprintln!("integration {id}: new key (shown once; the old key no longer works):");
            println!("{key}");
        }
        IntegrationCommand::Revoke { id } => {
            agents::revoke_integration(pool, &actor, id)
                .await
                .map_err(api)?;
            println!("revoked integration {id} (its agent devices are signed out)");
        }
    }
    Ok(())
}
