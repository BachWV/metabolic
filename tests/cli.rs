use argon2::{password_hash::SaltString, Argon2, PasswordHasher};
use std::{
    path::Path,
    process::{Command, Output},
};

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_metabolic"))
        .current_dir(dir)
        .env("DATABASE_PATH", dir.join("must-not-exist.sqlite3"))
        .env("SITE_URL", "invalid")
        .env("ADMIN_PASSWORD_HASH", "invalid")
        .env("PROXY_MODE", "invalid")
        .env("RUST_LOG", "off")
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn yaml_drives_import_backup_and_retry_despite_environment_overrides() {
    let temp = tempfile::tempdir().unwrap();
    let private = temp.path().join("private");
    std::fs::create_dir(&private).unwrap();
    let hash = Argon2::default()
        .hash_password(
            &rand::random::<[u8; 32]>(),
            &SaltString::generate(&mut rand::rngs::OsRng),
        )
        .unwrap()
        .to_string();
    let yaml = include_str!("../examples/config.example.yaml")
        .replace("<GENERATE_LOCALLY_WITH_HASH_PASSWORD>", &hash);
    std::fs::write(private.join("config.yaml"), yaml).unwrap();
    std::fs::write(temp.path().join("input.json"), "[]").unwrap();
    std::fs::write(temp.path().join("mapping.json"), "{}").unwrap();
    for args in [
        vec![
            "import",
            "input.json",
            "--mapping",
            "mapping.json",
            "--config",
            "private/config.yaml",
        ],
        vec![
            "backup",
            "backup.sqlite3",
            "--config",
            "private/config.yaml",
        ],
        vec!["retry-mail", "--config", "private/config.yaml"],
    ] {
        let output = run(temp.path(), &args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert!(private.join("comments.sqlite3").is_file());
    assert!(temp.path().join("backup.sqlite3").is_file());
    assert!(!temp.path().join("comments.sqlite3").exists());
    assert!(!temp.path().join("must-not-exist.sqlite3").exists());
    assert!(!run(
        temp.path(),
        &[
            "backup",
            "backup.sqlite3",
            "--config",
            "private/config.yaml"
        ]
    )
    .status
    .success());
    assert!(!run(temp.path(), &["retry-mail"]).status.success());
    assert!(!run(temp.path(), &["serve"]).status.success());
    std::fs::remove_file(private.join("comments.sqlite3")).unwrap();
    assert!(!run(
        temp.path(),
        &["retry-mail", "--config", "private/config.yaml"]
    )
    .status
    .success());
    assert!(!private.join("comments.sqlite3").exists());
}
