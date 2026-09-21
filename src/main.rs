mod api;
mod config;
mod db;
mod mail;
mod migrate;
mod proxy;
#[cfg(test)]
mod tests;
mod util;

use anyhow::Result;
use argon2::{password_hash::SaltString, Argon2, PasswordHasher};
use clap::{Parser, Subcommand};
use std::{net::SocketAddr, path::PathBuf};

#[derive(Parser)]
#[command(about = "Small self-hosted blog comments service")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Serve {
        #[arg(long)]
        config: PathBuf,
    },
    /// Prompt for a new admin password and print its Argon2id hash.
    HashPassword,
    /// Print a privacy-safe report with a mapping object; does not modify the DB.
    Preflight {
        input: PathBuf,
        #[arg(long)]
        public_dir: Option<PathBuf>,
    },
    /// Import or resolve previously held comments. Mapping JSON is URL -> path or null.
    Import {
        input: PathBuf,
        #[arg(long)]
        mapping: PathBuf,
        #[arg(long)]
        config: PathBuf,
    },
    /// Make a consistent standalone SQLite backup. Destination must not exist.
    Backup {
        output: PathBuf,
        #[arg(long)]
        config: PathBuf,
    },
    RetryMail {
        #[arg(long)]
        config: PathBuf,
    },
}
#[tokio::main(worker_threads = 2)]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();
    match Cli::parse().command {
        Command::HashPassword => {
            let password =
                rpassword::prompt_password("New admin password (at least 12 characters): ")?;
            anyhow::ensure!(
                password.chars().count() >= 12,
                "password must contain at least 12 characters"
            );
            let confirm = rpassword::prompt_password("Repeat password: ")?;
            anyhow::ensure!(password == confirm, "passwords do not match");
            let salt = SaltString::generate(&mut rand::rngs::OsRng);
            let hash = Argon2::default()
                .hash_password(password.as_bytes(), &salt)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("{hash}");
        }
        Command::Preflight { input, public_dir } => {
            let rows = migrate::read(&input)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&migrate::preflight(&rows, public_dir.as_deref())?)?
            );
        }
        Command::Import {
            input,
            mapping,
            config,
        } => {
            let database = config::Config::from_file(&config)?.database;
            let rows = migrate::read(&input)?;
            let mapping: migrate::Mapping = serde_json::from_slice(&std::fs::read(mapping)?)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&migrate::import(&database, &rows, &mapping).await?)?
            );
        }
        Command::Backup { output, config } => {
            let database = config::Config::from_file(&config)?.database;
            anyhow::ensure!(!output.exists(), "backup destination already exists");
            anyhow::ensure!(
                std::path::Path::new(&database).is_file(),
                "source database not found"
            );
            let pool = db::open(&database).await?;
            sqlx::query("VACUUM INTO ?")
                .bind(
                    output
                        .to_str()
                        .ok_or_else(|| anyhow::anyhow!("invalid output path"))?,
                )
                .execute(&pool)
                .await?;
            pool.close().await;
            println!("Backup complete");
        }
        Command::RetryMail { config } => {
            let database = config::Config::from_file(&config)?.database;
            anyhow::ensure!(
                std::path::Path::new(&database).is_file(),
                "source database not found"
            );
            let pool = db::open(&database).await?;
            let result = sqlx::query("UPDATE mail_jobs SET attempts=0,state='pending',next_attempt=? WHERE state='failed'").bind(util::now()).execute(&pool).await?;
            println!("Reset {} failed jobs", result.rows_affected());
        }
        Command::Serve { config } => {
            let config = config::Config::from_file(&config)?;
            let listener = tokio::net::TcpListener::bind(&config.bind).await?;
            let db = db::open(&config.database).await?;
            let app = api::App::new(db, config);
            let transport = mail::transport(&app)?;
            let worker = transport.map(|t| tokio::spawn(mail::run(app.clone(), t)));
            if worker.is_none() {
                tracing::warn!(
                    "smtp_host absent: notifications remain queued until SMTP is configured"
                );
            }
            tracing::info!(address=%listener.local_addr()?, "comments service listening");
            axum::serve(
                listener,
                api::router(app).into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(shutdown())
            .await?;
            if let Some(worker) = worker {
                worker.abort();
            }
        }
    }
    Ok(())
}
async fn shutdown() {
    #[cfg(unix)]
    {
        let mut term =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod cli_tests {
    use super::*;
    #[test]
    fn database_commands_require_explicit_yaml_and_reject_database_overrides() {
        for args in [
            vec!["serve"],
            vec!["import", "input.json", "--mapping", "mapping.json"],
            vec!["backup", "backup.sqlite3"],
            vec!["retry-mail"],
        ] {
            let mut missing = vec!["metabolic"];
            missing.extend(args.clone());
            assert!(Cli::try_parse_from(&missing).is_err());
            missing.extend(["--config", "private/config.yaml"]);
            assert!(Cli::try_parse_from(&missing).is_ok());
            missing.extend(["--database", "other.sqlite3"]);
            assert!(Cli::try_parse_from(&missing).is_err());
        }
        assert!(Cli::try_parse_from(["metabolic", "hash-password"]).is_ok());
        assert!(Cli::try_parse_from(["metabolic", "preflight", "input.json"]).is_ok());
    }
}
