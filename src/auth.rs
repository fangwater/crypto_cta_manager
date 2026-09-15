use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Row, postgres::PgPool};
use std::collections::BTreeSet;
use std::fs::File;
use std::io::Read;

pub const SESSION_COOKIE: &str = "cta_session";
const SESSION_TTL_SECS: i64 = 7 * 24 * 60 * 60;
const PASSWORD_ITERATIONS: u32 = 120_000;

#[derive(Clone, Debug, Serialize)]
pub struct UserView {
    pub user_id: i64,
    pub username: String,
    pub role: String,
    pub source_ids: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct AuthUser {
    pub user_id: i64,
    pub username: String,
    pub role: String,
}

impl AuthUser {
    pub fn is_admin(&self) -> bool {
        self.role == "admin"
    }
}

#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Deserialize)]
pub struct SetSourcesRequest {
    pub source_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct SetRoleRequest {
    pub role: String,
}

fn validate_username(username: &str) -> Result<String> {
    let username = username.trim().to_string();
    if username.len() < 3 || username.len() > 64 {
        bail!("username must be between 3 and 64 characters");
    }
    if !username.chars().all(|character| {
        character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.' | '@')
    }) {
        bail!("username contains unsupported characters");
    }
    Ok(username)
}

fn validate_password(password: &str) -> Result<()> {
    if password.len() < 8 {
        bail!("password must contain at least 8 characters");
    }
    if password.len() > 256 {
        bail!("password must not contain more than 256 characters");
    }
    Ok(())
}

fn hash_password(password: &str) -> Result<String> {
    validate_password(password)?;
    let mut salt = [0_u8; 16];
    File::open("/dev/urandom")
        .context("failed to open operating-system random source")?
        .read_exact(&mut salt)
        .context("failed to read password salt")?;
    let derived = pbkdf2_sha256(password.as_bytes(), &salt, PASSWORD_ITERATIONS);
    Ok(format!(
        "pbkdf2-sha256${PASSWORD_ITERATIONS}${}${}",
        URL_SAFE_NO_PAD.encode(salt),
        URL_SAFE_NO_PAD.encode(derived)
    ))
}

fn verify_password(password: &str, encoded_hash: &str) -> bool {
    let Some((algorithm, iterations, salt, expected)) =
        encoded_hash.split_once('$').and_then(|(algorithm, rest)| {
            let (iterations, rest) = rest.split_once('$')?;
            let (salt, expected) = rest.split_once('$')?;
            Some((algorithm, iterations, salt, expected))
        })
    else {
        return false;
    };
    if algorithm != "pbkdf2-sha256" || validate_password(password).is_err() {
        return false;
    }
    let Ok(iterations) = iterations.parse::<u32>() else {
        return false;
    };
    if !(10_000..=1_000_000).contains(&iterations) {
        return false;
    }
    let Ok(salt) = URL_SAFE_NO_PAD.decode(salt) else {
        return false;
    };
    let Ok(expected) = URL_SAFE_NO_PAD.decode(expected) else {
        return false;
    };
    let actual = pbkdf2_sha256(password.as_bytes(), &salt, iterations);
    actual.len() == expected.len()
        && actual
            .iter()
            .zip(expected)
            .fold(0_u8, |difference, (left, right)| {
                difference | (left ^ right)
            })
            == 0
}

fn pbkdf2_sha256(password: &[u8], salt: &[u8], iterations: u32) -> [u8; 32] {
    let mut mac = Hmac::<Sha256>::new_from_slice(password).expect("HMAC accepts every key length");
    let mut input = Vec::with_capacity(salt.len() + 4);
    input.extend_from_slice(salt);
    input.extend_from_slice(&1_u32.to_be_bytes());
    mac.update(&input);
    let mut block = mac.finalize().into_bytes();
    let mut output = block;
    for _ in 1..iterations {
        let mut mac =
            Hmac::<Sha256>::new_from_slice(password).expect("HMAC accepts every key length");
        mac.update(&block);
        block = mac.finalize().into_bytes();
        for (left, right) in output.iter_mut().zip(block.iter()) {
            *left ^= right;
        }
    }
    output.into()
}

pub async fn register(pool: &PgPool, request: RegisterRequest) -> Result<AuthUser> {
    let username = validate_username(&request.username)?;
    let password_hash = hash_password(&request.password)?;
    let mut tx = pool
        .begin()
        .await
        .context("failed to begin registration transaction")?;

    // Serialize the first-user decision so two simultaneous registrations
    // cannot both become administrators.
    sqlx::query("SELECT pg_advisory_xact_lock(738291)")
        .execute(&mut *tx)
        .await
        .context("failed to lock registration")?;
    let already_exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM cta_users)")
        .fetch_one(&mut *tx)
        .await
        .context("failed to inspect existing users")?;
    let role = if already_exists { "user" } else { "admin" };
    let user = sqlx::query(
        "INSERT INTO cta_users (username, password_hash, role) VALUES ($1, $2, $3) RETURNING user_id, username, role",
    )
    .bind(&username)
    .bind(password_hash)
    .bind(role)
    .fetch_one(&mut *tx)
    .await
    .map_err(|error| {
        if error.to_string().contains("cta_users_username_lower_idx") {
            anyhow::anyhow!("username is already registered")
        } else {
            anyhow::anyhow!(error)
        }
    })?;
    tx.commit().await.context("failed to commit registration")?;
    Ok(AuthUser {
        user_id: user.try_get("user_id")?,
        username: user.try_get("username")?,
        role: user.try_get("role")?,
    })
}

pub async fn login(pool: &PgPool, request: LoginRequest) -> Result<AuthUser> {
    let username = request.username.trim();
    let row = sqlx::query(
        "SELECT user_id, username, password_hash, role FROM cta_users WHERE lower(username) = lower($1) AND disabled = false",
    )
    .bind(username)
    .fetch_optional(pool)
    .await
    .context("failed to load login account")?
    .ok_or_else(|| anyhow::anyhow!("invalid username or password"))?;
    let password_hash: String = row.try_get("password_hash")?;
    if !verify_password(&request.password, &password_hash) {
        bail!("invalid username or password");
    }
    Ok(AuthUser {
        user_id: row.try_get("user_id")?,
        username: row.try_get("username")?,
        role: row.try_get("role")?,
    })
}

pub async fn has_users(pool: &PgPool) -> Result<bool> {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM cta_users)")
        .fetch_one(pool)
        .await
        .context("failed to inspect authentication setup")
}

fn new_session_token() -> String {
    let mut bytes = [0_u8; 32];
    File::open("/dev/urandom")
        .expect("operating-system random source must be available")
        .read_exact(&mut bytes)
        .expect("operating-system random source must be readable");
    URL_SAFE_NO_PAD.encode(bytes)
}

fn token_hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

pub async fn create_session(pool: &PgPool, user_id: i64) -> Result<String> {
    let token = new_session_token();
    sqlx::query(
        "INSERT INTO cta_auth_sessions (user_id, token_hash, expires_at) VALUES ($1, $2, now() + ($3 * interval '1 second'))",
    )
    .bind(user_id)
    .bind(token_hash(&token))
    .bind(SESSION_TTL_SECS)
    .execute(pool)
    .await
    .context("failed to create authentication session")?;
    // Expired sessions are disposable and should not accumulate forever.
    let _ = sqlx::query("DELETE FROM cta_auth_sessions WHERE expires_at < now()")
        .execute(pool)
        .await;
    Ok(token)
}

pub async fn load_session(pool: &PgPool, token: &str) -> Result<Option<AuthUser>> {
    if token.is_empty() || token.len() > 128 {
        return Ok(None);
    }
    let row = sqlx::query(
        "SELECT u.user_id, u.username, u.role FROM cta_auth_sessions s JOIN cta_users u ON u.user_id = s.user_id WHERE s.token_hash = $1 AND s.expires_at > now() AND u.disabled = false",
    )
    .bind(token_hash(token))
    .fetch_optional(pool)
    .await
    .context("failed to load authentication session")?;
    let Some(row) = row else {
        return Ok(None);
    };
    sqlx::query("UPDATE cta_auth_sessions SET last_seen_at = now() WHERE token_hash = $1")
        .bind(token_hash(token))
        .execute(pool)
        .await
        .context("failed to update authentication session")?;
    Ok(Some(AuthUser {
        user_id: row.try_get("user_id")?,
        username: row.try_get("username")?,
        role: row.try_get("role")?,
    }))
}

pub async fn delete_session(pool: &PgPool, token: &str) -> Result<()> {
    sqlx::query("DELETE FROM cta_auth_sessions WHERE token_hash = $1")
        .bind(token_hash(token))
        .execute(pool)
        .await
        .context("failed to delete authentication session")?;
    Ok(())
}

pub async fn user_view(pool: &PgPool, user: &AuthUser) -> Result<UserView> {
    let source_ids = sqlx::query_scalar::<_, String>(
        "SELECT source_id FROM cta_user_source_permissions WHERE user_id = $1 ORDER BY source_id",
    )
    .bind(user.user_id)
    .fetch_all(pool)
    .await
    .context("failed to load user source permissions")?;
    Ok(UserView {
        user_id: user.user_id,
        username: user.username.clone(),
        role: user.role.clone(),
        source_ids,
    })
}

pub async fn allowed_source_ids(
    pool: &PgPool,
    user: &AuthUser,
    configured_source_ids: &BTreeSet<String>,
) -> Result<BTreeSet<String>> {
    if user.is_admin() {
        return Ok(configured_source_ids.clone());
    }
    let ids = sqlx::query_scalar::<_, String>(
        "SELECT source_id FROM cta_user_source_permissions WHERE user_id = $1",
    )
    .bind(user.user_id)
    .fetch_all(pool)
    .await
    .context("failed to load authorized source ids")?;
    Ok(ids
        .into_iter()
        .filter(|source_id| configured_source_ids.contains(source_id))
        .collect())
}

pub async fn list_users(pool: &PgPool) -> Result<Vec<UserView>> {
    let rows = sqlx::query(
        "SELECT user_id, username, role FROM cta_users WHERE disabled = false ORDER BY username",
    )
    .fetch_all(pool)
    .await
    .context("failed to list users")?;
    let mut users = Vec::with_capacity(rows.len());
    for row in rows {
        let user = AuthUser {
            user_id: row.try_get("user_id")?,
            username: row.try_get("username")?,
            role: row.try_get("role")?,
        };
        users.push(user_view(pool, &user).await?);
    }
    Ok(users)
}

pub async fn set_sources(
    pool: &PgPool,
    user_id: i64,
    source_ids: &[String],
    configured_source_ids: &BTreeSet<String>,
) -> Result<UserView> {
    let unique = source_ids
        .iter()
        .map(|id| id.trim())
        .collect::<BTreeSet<_>>();
    if unique
        .iter()
        .any(|id| id.is_empty() || !configured_source_ids.contains(*id))
    {
        bail!("source_ids contains an unknown source");
    }
    let mut tx = pool
        .begin()
        .await
        .context("failed to begin permission update")?;
    let user = sqlx::query(
        "SELECT user_id, username, role FROM cta_users WHERE user_id = $1 AND disabled = false",
    )
    .bind(user_id)
    .fetch_optional(&mut *tx)
    .await
    .context("failed to load permission target")?
    .ok_or_else(|| anyhow::anyhow!("user was not found"))?;
    sqlx::query("DELETE FROM cta_user_source_permissions WHERE user_id = $1")
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .context("failed to clear user source permissions")?;
    for source_id in unique {
        sqlx::query("INSERT INTO cta_user_source_permissions (user_id, source_id) VALUES ($1, $2)")
            .bind(user_id)
            .bind(source_id)
            .execute(&mut *tx)
            .await
            .context("failed to save user source permission")?;
    }
    tx.commit()
        .await
        .context("failed to commit permission update")?;
    let user = AuthUser {
        user_id: user.try_get("user_id")?,
        username: user.try_get("username")?,
        role: user.try_get("role")?,
    };
    user_view(pool, &user).await
}

pub async fn set_role(pool: &PgPool, user_id: i64, role: &str) -> Result<UserView> {
    if !matches!(role, "admin" | "user") {
        bail!("role must be admin or user");
    }
    let mut tx = pool.begin().await.context("failed to begin role update")?;
    let current_role: Option<String> = sqlx::query_scalar(
        "SELECT role FROM cta_users WHERE user_id = $1 AND disabled = false FOR UPDATE",
    )
    .bind(user_id)
    .fetch_optional(&mut *tx)
    .await
    .context("failed to load role target")?;
    let Some(current_role) = current_role else {
        bail!("user was not found");
    };
    if current_role == "admin" && role == "user" {
        let admin_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM cta_users WHERE role = 'admin' AND disabled = false",
        )
        .fetch_one(&mut *tx)
        .await
        .context("failed to count administrators")?;
        if admin_count <= 1 {
            bail!("the last administrator cannot be demoted");
        }
    }
    let user = sqlx::query(
        "UPDATE cta_users SET role = $2, updated_at = now() WHERE user_id = $1 RETURNING user_id, username, role",
    )
    .bind(user_id)
    .bind(role)
    .fetch_one(&mut *tx)
    .await
    .context("failed to update user role")?;
    tx.commit().await.context("failed to commit role update")?;
    user_view(
        pool,
        &AuthUser {
            user_id: user.try_get("user_id")?,
            username: user.try_get("username")?,
            role: user.try_get("role")?,
        },
    )
    .await
}

pub fn session_cookie(token: &str) -> String {
    format!(
        "{}={}; Path=/; Max-Age={}; HttpOnly; SameSite=Strict",
        SESSION_COOKIE, token, SESSION_TTL_SECS
    )
}

pub fn clear_session_cookie() -> &'static str {
    "cta_session=; Path=/; Max-Age=0; HttpOnly; SameSite=Strict"
}

pub fn extract_session_cookie(cookie_header: Option<&str>) -> Option<String> {
    cookie_header?
        .split(';')
        .map(str::trim)
        .find_map(|part| part.strip_prefix(&format!("{SESSION_COOKIE}=")))
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_records_verify_and_reject_changes() {
        let encoded = hash_password("correct horse battery staple").unwrap();
        assert!(verify_password("correct horse battery staple", &encoded));
        assert!(!verify_password("wrong horse battery staple", &encoded));
        assert!(!verify_password("short", &encoded));
    }

    #[test]
    fn session_cookie_parser_ignores_other_cookies() {
        assert_eq!(
            extract_session_cookie(Some("theme=dark; cta_session=abc123; flag=yes")),
            Some("abc123".to_string())
        );
        assert_eq!(extract_session_cookie(Some("theme=dark")), None);
    }
}
