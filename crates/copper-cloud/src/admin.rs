//! `copper-cloud admin …` — user and signup administration straight against the database.

use std::io::{BufRead as _, IsTerminal as _, Write as _};

use anyhow::{bail, Context as _};
use copper_cloud_core::auth;
use copper_cloud_core::config::Config;
use copper_cloud_core::crypto::Crypto;
use copper_cloud_core::db;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::cli::AdminCommand;

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
    }
    Ok(())
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
