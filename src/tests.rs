use crate::{
    api::{self, App},
    config::Config,
    db, mail, migrate, util,
};
use axum::{
    body::Body,
    extract::ConnectInfo,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::{net::SocketAddr, path::Path};
use tower::ServiceExt;

async fn setup() -> (tempfile::TempDir, App) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("test.sqlite3");
    let db = db::open(path.to_str().unwrap()).await.unwrap();
    let config = Config {
        bind: "127.0.0.1:0".into(),
        database: path.to_str().unwrap().into(),
        origins: vec![
            "https://blog.example".into(),
            "https://comments.example".into(),
        ],
        site_url: "https://blog.example".into(),
        password_hash: String::new(),
        cookie_secure: true,
        proxy_mode: crate::proxy::ProxyMode::Nginx,
        smtp_host: String::new(),
        smtp_port: 587,
        smtp_user: String::new(),
        smtp_password: String::new(),
        mail_from: "blog@example.com".into(),
        mail_to: "owner@example.com".into(),
    };
    (temp, App::new(db, config))
}
async fn call(
    app: &App,
    method: &str,
    path: &str,
    body: Value,
    headers: &[(&str, &str)],
) -> (StatusCode, axum::http::HeaderMap, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("Content-Type", "application/json");
    for (key, value) in headers {
        request = request.header(*key, *value);
    }
    let mut request = request.body(Body::from(body.to_string())).unwrap();
    request.extensions_mut().insert(ConnectInfo(
        "127.0.0.1:34567".parse::<SocketAddr>().unwrap(),
    ));
    let response = api::router(app.clone()).oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, headers, body)
}
fn comment(page: &str) -> Value {
    json!({"page":page,"nick":"访客","email":"private@example.com","website":"https://example.com","body":"<script>alert(1)</script>\nhello"})
}
async fn seed_session(app: &App) {
    sqlx::query("INSERT INTO sessions VALUES(?,?,?)")
        .bind(util::hash("secret"))
        .bind(util::hash("csrf"))
        .bind(util::now() + 60)
        .execute(&app.db)
        .await
        .unwrap();
}
#[test]
fn normalization_and_urls() {
    assert_eq!(
        util::page("/posts/%E4%B8%AD%E6%96%87?q=1#x").unwrap(),
        "/posts/中文/"
    );
    assert_eq!(util::page("/posts//a/").unwrap(), "/posts/a/");
    for path in [
        "//evil.test/",
        "https://evil.test/",
        "/%2e%2e/",
        "/%5cevil",
        "/%2fhost",
        "/a%00",
    ] {
        assert!(util::page(path).is_err(), "{path}");
    }
    for link in [
        "javascript:alert(1)",
        "data:text/html,x",
        "https://user:pass@example.com",
    ] {
        assert!(util::website(link).is_err());
    }
}
#[tokio::test]
async fn public_privacy_replies_and_page_isolation() {
    let (_temp, app) = setup().await;
    let (s, _, created) = call(&app, "POST", "/api/comments", comment("/a"), &[]).await;
    assert_eq!(s, StatusCode::CREATED);
    let id = created["id"].as_i64().unwrap();
    let mut reply = comment("/b");
    reply["parent_id"] = json!(id);
    assert_eq!(
        call(&app, "POST", "/api/comments", reply.clone(), &[])
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    reply["page"] = json!("/a/");
    reply["is_admin"] = json!(true);
    assert_eq!(
        call(&app, "POST", "/api/comments", reply, &[]).await.0,
        StatusCode::CREATED
    );
    let (_, _, list) = call(&app, "GET", "/api/comments?page=/a/", Value::Null, &[]).await;
    assert_eq!(list["comments"].as_array().unwrap().len(), 2);
    assert!(list["comments"][0].get("email").is_none());
    assert!(list["comments"][0].get("legacy_id").is_none());
    assert_eq!(list["comments"][1]["is_admin"], false);
    assert_eq!(
        call(&app, "GET", "/api/comments?page=/b/", Value::Null, &[])
            .await
            .2["comments"],
        json!([])
    );
    let jobs: i64 = sqlx::query_scalar("SELECT count(*) FROM mail_jobs")
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(jobs, 2);
}
#[tokio::test]
async fn authentication_csrf_moderation_and_admin_reply() {
    let (_temp, app) = setup().await;
    seed_session(&app).await;
    let (_, _, created) = call(&app, "POST", "/api/comments", comment("/a"), &[]).await;
    let id = created["id"].as_i64().unwrap();
    let path = format!("/api/admin/comments/{id}");
    assert_eq!(
        call(&app, "GET", "/api/admin/comments", Value::Null, &[])
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let cookie = [("cookie", "blog_session=secret")];
    assert_eq!(
        call(&app, "PATCH", &path, json!({"status":"hidden"}), &cookie)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let auth = [("cookie", "blog_session=secret"), ("x-csrf-token", "csrf")];
    assert_eq!(
        call(&app, "PATCH", &path, json!({"status":"hidden"}), &auth)
            .await
            .0,
        StatusCode::OK
    );
    let hidden = call(&app, "GET", "/api/comments?page=/a/", Value::Null, &[])
        .await
        .2;
    assert_eq!(hidden["comments"][0]["unavailable"], true);
    assert!(!hidden.to_string().contains("script"));
    assert_eq!(
        call(&app, "PATCH", &path, json!({"status":"published"}), &auth)
            .await
            .0,
        StatusCode::OK
    );
    let reply_path = format!("{path}/reply");
    assert_eq!(
        call(&app, "POST", &reply_path, json!({"body":"感谢"}), &auth)
            .await
            .0,
        StatusCode::CREATED
    );
    let jobs: i64 = sqlx::query_scalar("SELECT count(*) FROM mail_jobs")
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(jobs, 1);
    let list = call(&app, "GET", "/api/comments?page=/a/", Value::Null, &[])
        .await
        .2;
    assert_eq!(list["comments"][1]["is_admin"], true);
    assert_eq!(
        call(&app, "PATCH", &path, json!({"status":"deleted"}), &auth)
            .await
            .0,
        StatusCode::OK
    );
    let (body, email): (String, String) =
        sqlx::query_as("SELECT body,email FROM comments WHERE id=?")
            .bind(id)
            .fetch_one(&app.db)
            .await
            .unwrap();
    assert_eq!((body, email), (String::new(), String::new()));
    assert_eq!(
        call(&app, "PATCH", &path, json!({"status":"published"}), &auth)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(&app, "POST", "/api/admin/logout", json!({}), &auth)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&app, "GET", "/api/admin/comments", Value::Null, &auth)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
}
#[tokio::test]
async fn origin_honeypot_validation_and_rate_limit() {
    let (_temp, app) = setup().await;
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/comments",
            comment("/a"),
            &[("origin", "https://evil.example")]
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let mut bait = comment("/a");
    bait["company"] = json!("bot");
    assert_eq!(
        call(&app, "POST", "/api/comments", bait, &[]).await.0,
        StatusCode::BAD_REQUEST
    );
    let mut bad = comment("/a");
    bad["website"] = json!("javascript:alert(1)");
    assert_eq!(
        call(&app, "POST", "/api/comments", bad, &[]).await.0,
        StatusCode::BAD_REQUEST
    );
    for _ in 0..3 {
        assert_eq!(
            call(&app, "POST", "/api/comments", comment("/a"), &[])
                .await
                .0,
            StatusCode::CREATED
        );
    }
    assert_eq!(
        call(&app, "POST", "/api/comments", comment("/a"), &[])
            .await
            .0,
        StatusCode::TOO_MANY_REQUESTS
    );
    let (_, headers, _) = call(
        &app,
        "GET",
        "/api/comments?page=/a",
        Value::Null,
        &[("origin", "https://blog.example")],
    )
    .await;
    assert_eq!(
        headers["access-control-allow-origin"],
        "https://blog.example"
    );
    assert!(headers.get("access-control-allow-credentials").is_none());
}
fn legacy(id: &str, pid: &str, url: &str) -> Value {
    json!({"objectId":id,"url":url,"pid":pid,"rid":"","nick":"旧访客","mail":"private@example.com","link":"","comment":"保留正文","status":"approved","createdAt":"2022-01-01T00:00:00Z"})
}
fn rows(value: Value) -> Vec<migrate::Legacy> {
    serde_json::from_value(value).unwrap()
}
#[tokio::test]
async fn migration_is_idempotent_preserves_orphans_and_holds_unknown_pages() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("import.sqlite3");
    let database = path.to_str().unwrap();
    let rows = rows(json!([
        legacy("root", "", "/known/"),
        legacy("reply", "root", "/known/"),
        legacy("orphan", "missing", "/known/"),
        legacy("unknown", "", "https://old.example/a")
    ]));
    let mut mapping = migrate::Mapping::new();
    mapping.insert("/known/".into(), Some("/new/".into()));
    let first = migrate::import(database, &rows, &mapping).await.unwrap();
    assert_eq!(first.inserted, 4);
    assert_eq!(first.held, 1);
    let second = migrate::import(database, &rows, &mapping).await.unwrap();
    assert_eq!(second.inserted, 0);
    assert_eq!(second.unchanged, 4);
    mapping.insert("https://old.example/a".into(), Some("/other/".into()));
    let third = migrate::import(database, &rows, &mapping).await.unwrap();
    assert_eq!(third.remapped, 1);
    let pool = db::open(database).await.unwrap();
    let jobs: i64 = sqlx::query_scalar("SELECT count(*) FROM mail_jobs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(jobs, 0);
    let (parent, note): (Option<i64>, String) =
        sqlx::query_as("SELECT parent_id,migration_note FROM comments WHERE legacy_id='orphan'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(parent.is_none());
    assert!(note.contains("missing"));
    let (parent, time): (Option<i64>, i64) =
        sqlx::query_as("SELECT parent_id,created_at FROM comments WHERE legacy_id='reply'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(parent.is_some());
    assert_eq!(time, 1640995200);
    let report = migrate::preflight(&rows, Some(Path::new("/nonexistent"))).unwrap();
    assert_eq!(report["total"], 4);
    assert!(!report.to_string().contains("private@example.com"));
}
#[tokio::test]
async fn cycles_do_not_hide_imported_comments() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("cycle.sqlite3");
    let rows = rows(json!([legacy("a", "b", "/a"), legacy("b", "a", "/a")]));
    let mapping = [("/a".into(), Some("/a/".into()))].into_iter().collect();
    migrate::import(path.to_str().unwrap(), &rows, &mapping)
        .await
        .unwrap();
    let pool = db::open(path.to_str().unwrap()).await.unwrap();
    let roots: i64 = sqlx::query_scalar("SELECT count(*) FROM comments WHERE parent_id IS NULL")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(roots, 2);
}
#[tokio::test]
async fn mail_failure_restart_retry_and_backup() {
    let (_temp, app) = setup().await;
    call(&app, "POST", "/api/comments", comment("/a"), &[]).await;
    sqlx::query("UPDATE mail_jobs SET attempts=1")
        .execute(&app.db)
        .await
        .unwrap();
    mail::finish(&app, 1, 1, false).await.unwrap();
    let reopened = db::open(&app.config.database).await.unwrap();
    let state: String = sqlx::query_scalar("SELECT state FROM mail_jobs WHERE id=1")
        .fetch_one(&reopened)
        .await
        .unwrap();
    assert_eq!(state, "pending");
    sqlx::query("UPDATE mail_jobs SET attempts=5")
        .execute(&app.db)
        .await
        .unwrap();
    mail::finish(&app, 1, 5, false).await.unwrap();
    let state: String = sqlx::query_scalar("SELECT state FROM mail_jobs WHERE id=1")
        .fetch_one(&reopened)
        .await
        .unwrap();
    assert_eq!(state, "failed");
    mail::finish(&app, 1, 5, true).await.unwrap();
    let backup = _temp.path().join("backup.sqlite3");
    sqlx::query("VACUUM INTO ?")
        .bind(backup.to_str().unwrap())
        .execute(&app.db)
        .await
        .unwrap();
    let copy = db::open(backup.to_str().unwrap()).await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM comments")
        .fetch_one(&copy)
        .await
        .unwrap();
    assert_eq!(count, 1);
}
#[tokio::test]
async fn root_pagination_keeps_replies_with_parent() {
    let (_temp, app) = setup().await;
    for i in 0..11 {
        sqlx::query("INSERT INTO comments(page,nick,body,created_at) VALUES('/a/','n','b',?)")
            .bind(i)
            .execute(&app.db)
            .await
            .unwrap();
    }
    sqlx::query(
        "INSERT INTO comments(page,nick,body,parent_id,created_at) VALUES('/a/','r','reply',1,20)",
    )
    .execute(&app.db)
    .await
    .unwrap();
    let first = call(
        &app,
        "GET",
        "/api/comments?page=/a&cursor=0",
        Value::Null,
        &[],
    )
    .await
    .2;
    assert_eq!(first["has_more"], true);
    assert_eq!(first["comments"].as_array().unwrap().len(), 10);
    let second = call(
        &app,
        "GET",
        "/api/comments?page=/a&cursor=1",
        Value::Null,
        &[],
    )
    .await
    .2;
    assert_eq!(second["has_more"], false);
    assert_eq!(second["comments"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn login_issues_secure_cookie_and_expires() {
    use argon2::{password_hash::SaltString, Argon2, PasswordHasher};
    let (_temp, app) = setup().await;
    let password = format!("{:032x}", rand::random::<u128>());
    let mut config = (*app.config).clone();
    config.password_hash = Argon2::default()
        .hash_password(
            password.as_bytes(),
            &SaltString::generate(&mut rand::rngs::OsRng),
        )
        .unwrap()
        .to_string();
    let app = App::new(app.db.clone(), config);
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/admin/login",
            json!({"password":"wrong"}),
            &[]
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let (status, headers, data) = call(
        &app,
        "POST",
        "/api/admin/login",
        json!({"password":password}),
        &[("origin", "https://comments.example")],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let cookie = headers["set-cookie"].to_str().unwrap();
    assert!(cookie.contains("HttpOnly"));
    assert!(cookie.contains("Secure"));
    assert!(cookie.contains("SameSite=Strict"));
    let cookie = cookie.split(';').next().unwrap();
    let auth = [
        ("cookie", cookie),
        ("x-csrf-token", data["csrf"].as_str().unwrap()),
    ];
    assert_eq!(
        call(&app, "GET", "/api/admin/session", Value::Null, &auth)
            .await
            .0,
        StatusCode::OK
    );
    sqlx::query("UPDATE sessions SET expires_at=0")
        .execute(&app.db)
        .await
        .unwrap();
    assert_eq!(
        call(&app, "GET", "/api/admin/session", Value::Null, &auth)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn smtp_connection_failure_keeps_comment_and_schedules_retry() {
    let (_temp, app) = setup().await;
    call(&app, "POST", "/api/comments", comment("/a"), &[]).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let transport =
        lettre::AsyncSmtpTransport::<lettre::Tokio1Executor>::builder_dangerous("127.0.0.1")
            .port(port)
            .timeout(Some(std::time::Duration::from_secs(1)))
            .build();
    mail::tick(&app, &transport).await.unwrap();
    let (state, attempts, next): (String, i64, i64) =
        sqlx::query_as("SELECT state,attempts,next_attempt FROM mail_jobs")
            .fetch_one(&app.db)
            .await
            .unwrap();
    assert_eq!(state, "pending");
    assert_eq!(attempts, 1);
    assert!(next > util::now());
    let comments: i64 = sqlx::query_scalar("SELECT count(*) FROM comments")
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(comments, 1);
}

#[tokio::test]
async fn concurrent_replies_are_saved_without_sqlite_upgrade_conflicts() {
    let (_temp, app) = setup().await;
    let id = call(&app, "POST", "/api/comments", comment("/a"), &[])
        .await
        .2["id"]
        .as_i64()
        .unwrap();
    let mut tasks = Vec::new();
    for i in 1..=8 {
        let app = app.clone();
        tasks.push(tokio::spawn(async move {
            let mut reply = comment("/a");
            reply["parent_id"] = json!(id);
            let ip = format!("192.0.2.{i}");
            call(&app, "POST", "/api/comments", reply, &[("x-real-ip", &ip)])
                .await
                .0
        }));
    }
    for task in tasks {
        assert_eq!(task.await.unwrap(), StatusCode::CREATED);
    }
    let jobs: i64 = sqlx::query_scalar("SELECT count(*) FROM mail_jobs")
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(jobs, 9);
}

#[tokio::test]
async fn cloudflare_limits_by_visitor_and_rejects_missing_header_before_writing() {
    let (_temp, app) = setup().await;
    let mut config = (*app.config).clone();
    config.proxy_mode = crate::proxy::ProxyMode::Cloudflare;
    let app = App::new(app.db.clone(), config);
    assert_eq!(
        call(&app, "POST", "/api/comments", comment("/a"), &[])
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/admin/login",
            json!({"password":"test"}),
            &[]
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    for i in 1..=5 {
        let fake = format!("192.0.2.{i}");
        let headers = [
            ("cf-connecting-ip", "2001:db8::1"),
            ("x-real-ip", fake.as_str()),
        ];
        assert_eq!(
            call(&app, "POST", "/api/comments", comment("/a"), &headers)
                .await
                .0,
            StatusCode::CREATED
        );
    }
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/comments",
            comment("/a"),
            &[
                ("cf-connecting-ip", "2001:db8::1"),
                ("x-real-ip", "192.0.2.200")
            ]
        )
        .await
        .0,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/comments",
            comment("/a"),
            &[("cf-connecting-ip", "203.0.113.2")]
        )
        .await
        .0,
        StatusCode::CREATED
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM comments")
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(count, 6);
    assert_eq!(
        call(&app, "GET", "/healthz", Value::Null, &[]).await.0,
        StatusCode::OK
    );
}
