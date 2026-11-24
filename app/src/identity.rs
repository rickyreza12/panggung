use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
};
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

pub const SESSION_LIFETIME_SECONDS: i64 = 24 * 60 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, serde::Serialize)]
#[sqlx(type_name = "TEXT", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Admin,
    Member,
    Viewer,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow, serde::Serialize)]
pub struct User {
    pub id: i64,
    pub username: String,
    pub role: Role,
}

// Deliberately not Debug or Serialize: only the caller receives the bearer secret.
pub struct SessionToken {
    token: String,
}

impl SessionToken {
    pub fn as_str(&self) -> &str {
        &self.token
    }
}

#[derive(Debug)]
pub enum IdentityError {
    InvalidInput,
    InvalidCredentials,
    InvalidSession,
    BootstrapClosed,
    Forbidden,
    Conflict,
    Crypto,
    Database(sqlx::Error),
}

impl std::fmt::Display for IdentityError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidInput => "Invalid identity input",
            Self::InvalidCredentials => "Invalid credentials",
            Self::InvalidSession => "Invalid or expired session",
            Self::BootstrapClosed => "Identity bootstrap already completed",
            Self::Forbidden => "Administrator required",
            Self::Conflict => "Identity already exists",
            Self::Crypto => "Credential operation failed",
            Self::Database(_) => "Identity database operation failed",
        })
    }
}

impl std::error::Error for IdentityError {}

impl From<sqlx::Error> for IdentityError {
    fn from(error: sqlx::Error) -> Self {
        if let sqlx::Error::Database(ref database) = error
            && database.is_unique_violation()
        {
            return Self::Conflict;
        }
        Self::Database(error)
    }
}

#[derive(Clone)]
pub struct IdentityStore {
    pool: SqlitePool,
}

impl IdentityStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn bootstrap_admin(
        &self,
        username: &str,
        password: &str,
    ) -> Result<User, IdentityError> {
        validate_new_user(username, password)?;
        let password_hash = hash_password(password.to_owned()).await?;
        let mut transaction = self.pool.begin().await?;
        let user = sqlx::query_as::<_, User>(
            "INSERT INTO identity_users (username, password_hash, role)
             SELECT ?, ?, 'admin' WHERE NOT EXISTS (SELECT 1 FROM identity_users)
             RETURNING id, username, role",
        )
        .bind(username)
        .bind(password_hash)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(IdentityError::BootstrapClosed)?;
        audit(&mut transaction, user.id, user.id, "bootstrap_admin").await?;
        transaction.commit().await?;
        Ok(user)
    }

    // The caller must obtain admin_id from an authenticated principal.
    pub async fn create_user(
        &self,
        admin_id: i64,
        username: &str,
        password: &str,
        role: Role,
    ) -> Result<User, IdentityError> {
        validate_new_user(username, password)?;
        let password_hash = hash_password(password.to_owned()).await?;
        let mut transaction = self.pool.begin().await?;
        let user = sqlx::query_as::<_, User>(
            "INSERT INTO identity_users (username, password_hash, role)
             SELECT ?, ?, ? WHERE EXISTS (SELECT 1 FROM identity_users WHERE id = ? AND role = 'admin')
             RETURNING id, username, role",
        )
        .bind(username)
        .bind(password_hash)
        .bind(role)
        .bind(admin_id)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(IdentityError::Forbidden)?;
        audit(&mut transaction, admin_id, user.id, "create_user").await?;
        transaction.commit().await?;
        Ok(user)
    }

    pub async fn login(
        &self,
        username: &str,
        password: &str,
    ) -> Result<SessionToken, IdentityError> {
        if !valid_username(username) || password.len() > 1024 || password.is_empty() {
            return Err(IdentityError::InvalidCredentials);
        }
        let credentials = sqlx::query_as::<_, (i64, String)>(
            "SELECT id, password_hash FROM identity_users WHERE username = ?",
        )
        .bind(username)
        .fetch_optional(&self.pool)
        .await?;
        let password = password.to_owned();
        let user_id = tokio::task::spawn_blocking(move || {
            let Some((id, hash)) = credentials else {
                // Do comparable work for missing users to reduce username timing leakage.
                hash_password_sync(&password)?;
                return Err(IdentityError::InvalidCredentials);
            };
            let parsed = PasswordHash::new(&hash).map_err(|_| IdentityError::Crypto)?;
            if parsed.algorithm.as_str() != "argon2id" {
                return Err(IdentityError::Crypto);
            }
            Argon2::default()
                .verify_password(password.as_bytes(), &parsed)
                .map_err(|error| match error {
                    argon2::password_hash::Error::Password => IdentityError::InvalidCredentials,
                    _ => IdentityError::Crypto,
                })?;
            Ok(id)
        })
        .await
        .map_err(|_| IdentityError::Crypto)??;

        let token = random_secret()?;
        sqlx::query(
            "INSERT INTO identity_sessions (token_hash, user_id, expires_at)
             VALUES (?, ?, unixepoch() + ?)",
        )
        .bind(token_digest(&token)?)
        .bind(user_id)
        .bind(SESSION_LIFETIME_SECONDS)
        .execute(&self.pool)
        .await?;
        Ok(SessionToken { token })
    }

    pub async fn resolve_session(&self, token: &str) -> Result<User, IdentityError> {
        sqlx::query_as::<_, User>(
            "SELECT u.id, u.username, u.role FROM identity_sessions s
             JOIN identity_users u ON u.id = s.user_id
             WHERE s.token_hash = ? AND s.revoked_at IS NULL AND s.expires_at > unixepoch()",
        )
        .bind(token_digest(token)?)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(IdentityError::InvalidSession)
    }

    pub async fn revoke_session(&self, token: &str) -> Result<(), IdentityError> {
        let result = sqlx::query(
            "UPDATE identity_sessions SET revoked_at = unixepoch()
             WHERE token_hash = ? AND revoked_at IS NULL",
        )
        .bind(token_digest(token)?)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(IdentityError::InvalidSession);
        }
        Ok(())
    }
}

async fn audit(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    actor_id: i64,
    subject_id: i64,
    action: &str,
) -> Result<(), IdentityError> {
    sqlx::query(
        "INSERT INTO identity_audit_events (actor_id, subject_id, action) VALUES (?, ?, ?)",
    )
    .bind(actor_id)
    .bind(subject_id)
    .bind(action)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

fn valid_username(username: &str) -> bool {
    (1..=63).contains(&username.len())
        && username.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
        })
        && username.as_bytes()[0].is_ascii_alphanumeric()
}

fn validate_new_user(username: &str, password: &str) -> Result<(), IdentityError> {
    if !valid_username(username) || !(12..=1024).contains(&password.len()) {
        return Err(IdentityError::InvalidInput);
    }
    Ok(())
}

fn hash_password_sync(password: &str) -> Result<String, IdentityError> {
    let mut salt_bytes = [0_u8; 16];
    OsRng
        .try_fill_bytes(&mut salt_bytes)
        .map_err(|_| IdentityError::Crypto)?;
    let salt = SaltString::encode_b64(&salt_bytes).map_err(|_| IdentityError::Crypto)?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|_| IdentityError::Crypto)
}

async fn hash_password(password: String) -> Result<String, IdentityError> {
    tokio::task::spawn_blocking(move || hash_password_sync(&password))
        .await
        .map_err(|_| IdentityError::Crypto)?
}

fn random_secret() -> Result<String, IdentityError> {
    let mut bytes = [0_u8; 32];
    OsRng
        .try_fill_bytes(&mut bytes)
        .map_err(|_| IdentityError::Crypto)?;
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut token = String::with_capacity(64);
    for byte in bytes {
        token.push(char::from(HEX[usize::from(byte >> 4)]));
        token.push(char::from(HEX[usize::from(byte & 15)]));
    }
    Ok(token)
}

fn token_digest(token: &str) -> Result<Vec<u8>, IdentityError> {
    if token.len() != 64
        || !token
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(IdentityError::InvalidSession);
    }
    Ok(Sha256::digest(token.as_bytes()).to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn concurrent_bootstrap_has_one_winner() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::database::open(&dir.path().join("identity.db"))
            .await
            .unwrap();
        let store = IdentityStore::new(pool.clone());
        let password = random_secret().unwrap();
        let (first, second) = tokio::join!(
            store.bootstrap_admin("first-admin", &password),
            store.bootstrap_admin("second-admin", &password)
        );
        assert!(matches!(
            (&first, &second),
            (Ok(_), Err(IdentityError::BootstrapClosed))
                | (Err(IdentityError::BootstrapClosed), Ok(_))
        ));
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM identity_users WHERE role = 'admin'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 1);
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM identity_audit_events WHERE action = 'bootstrap_admin'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn credentials_roles_and_session_lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.db");
        let pool = crate::database::open(&path).await.unwrap();
        let store = IdentityStore::new(pool.clone());
        let password = random_secret().unwrap();
        let admin = store.bootstrap_admin("admin", &password).await.unwrap();
        let member = store
            .create_user(admin.id, "member", &password, Role::Member)
            .await
            .unwrap();
        assert_eq!(member.role, Role::Member);
        assert!(matches!(
            store
                .create_user(admin.id, "member", &password, Role::Viewer)
                .await,
            Err(IdentityError::Conflict)
        ));
        assert!(matches!(
            store
                .create_user(member.id, "viewer", &password, Role::Viewer)
                .await,
            Err(IdentityError::Forbidden)
        ));
        assert!(matches!(
            store.login("member", &random_secret().unwrap()).await,
            Err(IdentityError::InvalidCredentials)
        ));
        assert!(matches!(
            store.login("missing", &password).await,
            Err(IdentityError::InvalidCredentials)
        ));

        let hashes: Vec<String> =
            sqlx::query_scalar("SELECT password_hash FROM identity_users ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert!(
            hashes
                .iter()
                .all(|hash| hash.starts_with("$argon2id$") && hash != &password)
        );
        assert_ne!(hashes[0], hashes[1]);

        let token = store.login("member", &password).await.unwrap();
        let other = store.login("member", &password).await.unwrap();
        assert!(token.as_str() != other.as_str());
        let stored: Vec<u8> =
            sqlx::query_scalar("SELECT token_hash FROM identity_sessions WHERE token_hash = ?")
                .bind(token_digest(token.as_str()).unwrap())
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(stored.len(), 32);
        assert_ne!(stored, token.as_str().as_bytes());

        pool.close().await;
        let pool = crate::database::open(&path).await.unwrap();
        let store = IdentityStore::new(pool.clone());
        assert_eq!(store.resolve_session(token.as_str()).await.unwrap(), member);
        store.revoke_session(token.as_str()).await.unwrap();
        assert!(matches!(
            store.resolve_session(token.as_str()).await,
            Err(IdentityError::InvalidSession)
        ));
        assert_eq!(store.resolve_session(other.as_str()).await.unwrap(), member);
        sqlx::query("UPDATE identity_sessions SET created_at = unixepoch() - 100, expires_at = unixepoch() WHERE token_hash = ?")
            .bind(token_digest(other.as_str()).unwrap()).execute(&pool).await.unwrap();
        assert!(matches!(
            store.resolve_session(other.as_str()).await,
            Err(IdentityError::InvalidSession)
        ));
        assert!(matches!(
            store.resolve_session("malformed").await,
            Err(IdentityError::InvalidSession)
        ));
    }
}
