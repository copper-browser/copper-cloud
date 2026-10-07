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

use crate::cli::{AdminCommand, HistoryCommand, HistoryDeleteArgs, IntelligenceCommand};

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
            let user = auth::insert_user(&mut conn, &crypto, &email, &display_name, &hash)
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
    }
    Ok(())
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
    let row: Option<(Uuid, String)> = match Uuid::parse_str(args.user.trim()) {
        Ok(id) => {
            sqlx::query_as("SELECT id, email FROM users WHERE id = $1")
                .bind(id)
                .fetch_optional(pool)
                .await?
        }
        Err(_) => {
            sqlx::query_as("SELECT id, email FROM users WHERE lower(email) = lower($1)")
                .bind(args.user.trim())
                .fetch_optional(pool)
                .await?
        }
    };
    let (user_id, email) = row.with_context(|| format!("no user {}", args.user))?;
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
