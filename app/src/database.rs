use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};
use std::{path::Path, time::Duration};

pub async fn open(path: &Path) -> Result<SqlitePool, Box<dyn std::error::Error>> {
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(path)
                .create_if_missing(true)
                .foreign_keys(true)
                .journal_mode(SqliteJournalMode::Wal)
                .busy_timeout(Duration::from_secs(5)),
        )
        .await?;
    sqlx::migrate!().run(&pool).await?;
    Ok(pool)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn migrations_and_data_survive_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog.db");
        let pool = open(&path).await.unwrap();
        sqlx::query("INSERT INTO components (name, description, owner) VALUES (?, ?, ?)")
            .bind("service")
            .bind("test")
            .bind("team")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
        let pool = open(&path).await.unwrap();
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM components")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1);
        pool.close().await;
    }
}
