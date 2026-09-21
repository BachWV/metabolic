use anyhow::Result;
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
    SqlitePool,
};
use std::time::Duration;

pub async fn open(path: &str) -> Result<SqlitePool> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(5));
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(options)
        .await?;
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&pool)
        .await?;
    anyhow::ensure!(version <= 1, "database schema is newer than this binary");
    sqlx::raw_sql(include_str!("../schema.sql"))
        .execute(&pool)
        .await?;
    Ok(pool)
}

#[derive(Debug, Clone, serde::Serialize, sqlx::FromRow)]
pub struct Comment {
    pub id: i64,
    pub page: String,
    pub parent_id: Option<i64>,
    pub nick: String,
    pub email: String,
    pub website: String,
    pub body: String,
    pub status: String,
    pub is_admin: bool,
    pub created_at: i64,
    pub legacy_id: Option<String>,
    pub migration_note: String,
}
