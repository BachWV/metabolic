use crate::{config::Config, db::Comment, util};
use argon2::{Argon2, PasswordHash, PasswordVerifier};
use axum::{
    extract::{ConnectInfo, DefaultBodyLimit, Path, Query, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, patch, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::SqlitePool;
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
};
use tower_http::cors::CorsLayer;

#[derive(Clone)]
pub struct App {
    pub db: SqlitePool,
    pub config: Arc<Config>,
    limits: Arc<Mutex<HashMap<String, (i64, u32)>>>,
    auth_slots: Arc<tokio::sync::Semaphore>,
}
impl App {
    pub fn new(db: SqlitePool, config: Config) -> Self {
        Self {
            db,
            config: Arc::new(config),
            limits: Arc::new(Mutex::new(HashMap::new())),
            auth_slots: Arc::new(tokio::sync::Semaphore::new(2)),
        }
    }
    fn limit(
        &self,
        peer: SocketAddr,
        headers: &HeaderMap,
        kind: &str,
        max: u32,
        window: i64,
    ) -> Result<(), ApiError> {
        let ip = self
            .config
            .proxy_mode
            .client_ip(peer, headers)
            .map_err(|message| ApiError(StatusCode::BAD_REQUEST, message))?;
        let mut limits = self.limits.lock().unwrap();
        let now = util::now();
        limits.retain(|_, (start, _)| now - *start < 600);
        let key = format!("{kind}:{ip}");
        if limits.len() >= 10000 && !limits.contains_key(&key) {
            return Err(ApiError(StatusCode::TOO_MANY_REQUESTS, "请稍后重试"));
        }
        let entry = limits.entry(key).or_insert((now, 0));
        if now - entry.0 >= window {
            *entry = (now, 0);
        }
        if entry.1 >= max {
            return Err(ApiError(
                StatusCode::TOO_MANY_REQUESTS,
                "请求过于频繁，请稍后重试",
            ));
        }
        entry.1 += 1;
        Ok(())
    }
}
pub struct ApiError(pub StatusCode, pub &'static str);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}
impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        tracing::error!(error = %e, "database operation failed");
        Self(StatusCode::INTERNAL_SERVER_ERROR, "服务暂时不可用")
    }
}
fn bad(msg: &'static str) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg)
}
fn denied() -> ApiError {
    ApiError(StatusCode::UNAUTHORIZED, "请先登录")
}
pub fn router(app: App) -> Router {
    let origins: Vec<HeaderValue> = app
        .config
        .origins
        .iter()
        .map(|o| o.parse().unwrap())
        .collect();
    Router::new()
        .route("/healthz", get(health))
        .route("/api/comments", get(list).post(create))
        .route("/api/admin/login", post(login))
        .route("/api/admin/logout", post(logout))
        .route("/api/admin/session", get(session))
        .route("/api/admin/comments", get(admin_list))
        .route("/api/admin/comments/{id}", patch(moderate))
        .route("/api/admin/comments/{id}/reply", post(admin_reply))
        .route("/admin", get(|| async { Html(include_str!("../web/admin.html")) }))
        .route("/admin.js", get(|| async { ([(header::CONTENT_TYPE, "text/javascript; charset=utf-8")], include_str!("../web/admin.js")) }))
        .layer(CorsLayer::new().allow_origin(origins).allow_methods([Method::GET, Method::POST, Method::PATCH])
            .allow_headers([header::CONTENT_TYPE, axum::http::HeaderName::from_static("x-csrf-token")]))
        .layer(DefaultBodyLimit::max(24 * 1024))
        .layer(axum::middleware::from_fn(|req: axum::extract::Request, next: axum::middleware::Next| async move {
            let mut response = next.run(req).await;
            let h = response.headers_mut();
            h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
            h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("default-src 'none'; script-src 'self'; style-src 'unsafe-inline'; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'"));
            h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
            response
        }))
        .with_state(app)
}
async fn health(State(app): State<App>) -> Result<Json<serde_json::Value>, ApiError> {
    sqlx::query("SELECT 1").execute(&app.db).await?;
    Ok(Json(json!({"ok": true})))
}
fn check_origin(app: &App, headers: &HeaderMap) -> Result<(), ApiError> {
    if let Some(origin) = headers.get(header::ORIGIN) {
        if !app
            .config
            .origins
            .iter()
            .any(|o| origin.as_bytes() == o.as_bytes())
        {
            return Err(ApiError(StatusCode::FORBIDDEN, "来源未授权"));
        }
    }
    Ok(())
}
#[derive(Deserialize)]
pub struct ListQuery {
    pub page: String,
    pub cursor: Option<u32>,
}
#[derive(Serialize)]
struct PublicComment {
    id: i64,
    parent_id: Option<i64>,
    nick: String,
    website: String,
    body: String,
    is_admin: bool,
    created_at: i64,
    unavailable: bool,
}
impl From<Comment> for PublicComment {
    fn from(c: Comment) -> Self {
        let visible = c.status == "published";
        Self {
            id: c.id,
            parent_id: c.parent_id,
            created_at: c.created_at,
            unavailable: !visible,
            nick: if visible { c.nick } else { String::new() },
            website: if visible { c.website } else { String::new() },
            body: if visible {
                c.body
            } else {
                "此评论已隐藏或删除".into()
            },
            is_admin: visible && c.is_admin,
        }
    }
}
async fn list(
    State(app): State<App>,
    Query(q): Query<ListQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let page = util::page(&q.page).map_err(|_| bad("文章路径无效"))?;
    let cursor = q.cursor.unwrap_or(0).min(100000);
    let total: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM comments WHERE page=? AND parent_id IS NULL AND status!='held'",
    )
    .bind(&page)
    .fetch_one(&app.db)
    .await?;
    let rows = sqlx::query_as::<_, Comment>("WITH RECURSIVE roots AS (SELECT id FROM comments WHERE page=? AND parent_id IS NULL AND status!='held' ORDER BY created_at DESC,id DESC LIMIT 10 OFFSET ?), tree(id) AS (SELECT id FROM roots UNION SELECT c.id FROM comments c JOIN tree t ON c.parent_id=t.id WHERE c.page=? AND c.status!='held') SELECT c.* FROM comments c JOIN tree ON c.id=tree.id ORDER BY c.created_at,c.id")
        .bind(&page).bind(i64::from(cursor) * 10).bind(&page).fetch_all(&app.db).await?;
    let comments: Vec<_> = rows.into_iter().map(PublicComment::from).collect();
    Ok(Json(
        json!({"comments": comments, "cursor": cursor, "has_more": (i64::from(cursor)+1)*10 < total}),
    ))
}
#[derive(Deserialize)]
pub struct NewComment {
    pub page: String,
    pub parent_id: Option<i64>,
    pub nick: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub website: String,
    pub body: String,
    #[serde(default)]
    pub company: String,
}
fn validate(input: &mut NewComment) -> Result<(), ApiError> {
    input.page = util::page(&input.page).map_err(|_| bad("文章路径无效"))?;
    input.nick = input.nick.trim().to_owned();
    input.body = input.body.trim().to_owned();
    input.email = input.email.trim().to_owned();
    if !input.company.is_empty()
        || input.nick.is_empty()
        || input.nick.chars().count() > 60
        || input.nick.chars().any(char::is_control)
        || input.body.is_empty()
        || input.body.chars().count() > 5000
        || input
            .body
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\r' && c != '\t')
    {
        return Err(bad("请填写昵称与评论（最多 5000 字）"));
    }
    if !input.email.is_empty()
        && (input.email.len() > 254 || input.email.parse::<lettre::Address>().is_err())
    {
        return Err(bad("邮箱格式无效"));
    }
    input.website =
        util::website(input.website.trim()).map_err(|_| bad("网址必须为 HTTP(S) 地址"))?;
    Ok(())
}
async fn insert(app: &App, mut input: NewComment, admin: bool) -> Result<i64, ApiError> {
    validate(&mut input)?;
    let mut tx = app.db.begin_with("BEGIN IMMEDIATE").await?;
    if let Some(parent) = input.parent_id {
        let row: Option<(String, String)> =
            sqlx::query_as("SELECT page,status FROM comments WHERE id=?")
                .bind(parent)
                .fetch_optional(&mut *tx)
                .await?;
        if !matches!(row, Some((ref p, ref s)) if p == &input.page && s == "published") {
            return Err(bad("回复对象不存在或已隐藏"));
        }
    }
    let id = sqlx::query("INSERT INTO comments(page,parent_id,nick,email,website,body,is_admin,created_at) VALUES(?,?,?,?,?,?,?,?)")
        .bind(input.page).bind(input.parent_id).bind(if admin { "博主".to_owned() } else { input.nick })
        .bind(input.email).bind(input.website).bind(input.body).bind(admin).bind(util::now()).execute(&mut *tx).await?.last_insert_rowid();
    if !admin {
        sqlx::query("INSERT INTO mail_jobs(comment_id,next_attempt) VALUES(?,?)")
            .bind(id)
            .bind(util::now())
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(id)
}
async fn create(
    State(app): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(input): Json<NewComment>,
) -> Result<impl IntoResponse, ApiError> {
    check_origin(&app, &headers)?;
    app.limit(peer, &headers, "comment", 5, 60)?;
    let id = insert(&app, input, false).await?;
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}
fn cookie_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|p| p.trim().strip_prefix("blog_session="))
}
async fn auth(app: &App, headers: &HeaderMap, csrf: bool) -> Result<String, ApiError> {
    let token = cookie_token(headers).ok_or_else(denied)?;
    let token_hash = util::hash(token);
    let row: Option<String> =
        sqlx::query_scalar("SELECT csrf_hash FROM sessions WHERE token_hash=? AND expires_at>?")
            .bind(&token_hash)
            .bind(util::now())
            .fetch_optional(&app.db)
            .await?;
    let expected = row.ok_or_else(denied)?;
    if csrf {
        check_origin(app, headers)?;
        let supplied = headers
            .get("x-csrf-token")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if util::hash(supplied) != expected {
            return Err(ApiError(StatusCode::FORBIDDEN, "安全令牌无效，请重新登录"));
        }
    }
    Ok(token_hash)
}
fn session_cookie(config: &Config, value: &str, age: i64) -> String {
    format!(
        "blog_session={value}; Path=/; HttpOnly; SameSite=Strict; Max-Age={age}{}",
        if config.cookie_secure { "; Secure" } else { "" }
    )
}
#[derive(Deserialize)]
struct Login {
    password: String,
}
async fn login(
    State(app): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(input): Json<Login>,
) -> Result<impl IntoResponse, ApiError> {
    check_origin(&app, &headers)?;
    app.limit(peer, &headers, "login", 5, 300)?;
    if input.password.len() > 1024 {
        return Err(denied());
    }
    let permit = app
        .auth_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError(StatusCode::TOO_MANY_REQUESTS, "登录繁忙，请稍后重试"))?;
    let hash = app.config.password_hash.clone();
    let valid = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        PasswordHash::new(&hash).is_ok_and(|hash| {
            Argon2::default()
                .verify_password(input.password.as_bytes(), &hash)
                .is_ok()
        })
    })
    .await
    .unwrap_or(false);
    if !valid {
        return Err(denied());
    }
    let token = util::token();
    let csrf = util::token();
    sqlx::query("DELETE FROM sessions WHERE expires_at<=?")
        .bind(util::now())
        .execute(&app.db)
        .await?;
    sqlx::query("INSERT INTO sessions VALUES(?,?,?)")
        .bind(util::hash(&token))
        .bind(util::hash(&csrf))
        .bind(util::now() + 43200)
        .execute(&app.db)
        .await?;
    Ok((
        [(
            header::SET_COOKIE,
            session_cookie(&app.config, &token, 43200),
        )],
        Json(json!({"csrf": csrf})),
    ))
}
async fn session(
    State(app): State<App>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    auth(&app, &headers, false).await?;
    Ok(Json(json!({"ok": true})))
}
async fn logout(State(app): State<App>, headers: HeaderMap) -> Result<impl IntoResponse, ApiError> {
    let token = auth(&app, &headers, true).await?;
    sqlx::query("DELETE FROM sessions WHERE token_hash=?")
        .bind(token)
        .execute(&app.db)
        .await?;
    Ok((
        [(header::SET_COOKIE, session_cookie(&app.config, "", 0))],
        Json(json!({"ok": true})),
    ))
}
#[derive(Deserialize)]
struct AdminQuery {
    offset: Option<u32>,
}
async fn admin_list(
    State(app): State<App>,
    headers: HeaderMap,
    Query(q): Query<AdminQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    auth(&app, &headers, false).await?;
    let rows = sqlx::query_as::<_, Comment>(
        "SELECT * FROM comments ORDER BY created_at DESC,id DESC LIMIT 51 OFFSET ?",
    )
    .bind(q.offset.unwrap_or(0))
    .fetch_all(&app.db)
    .await?;
    let more = rows.len() > 50;
    let rows: Vec<_> = rows.into_iter().take(50).collect();
    let failed: i64 = sqlx::query_scalar("SELECT count(*) FROM mail_jobs WHERE state='failed'")
        .fetch_one(&app.db)
        .await?;
    Ok(Json(
        json!({"comments": rows, "has_more": more, "failed_mail_jobs": failed}),
    ))
}
#[derive(Deserialize)]
struct Moderate {
    status: String,
}
async fn moderate(
    State(app): State<App>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(input): Json<Moderate>,
) -> Result<Json<serde_json::Value>, ApiError> {
    auth(&app, &headers, true).await?;
    if !matches!(input.status.as_str(), "hidden" | "published" | "deleted") {
        return Err(bad("无效状态"));
    }
    let result = sqlx::query("UPDATE comments SET status=?, body=CASE WHEN ?='deleted' THEN '' ELSE body END, email=CASE WHEN ?='deleted' THEN '' ELSE email END, website=CASE WHEN ?='deleted' THEN '' ELSE website END, nick=CASE WHEN ?='deleted' THEN '' ELSE nick END WHERE id=? AND status NOT IN ('held','deleted')")
        .bind(&input.status).bind(&input.status).bind(&input.status).bind(&input.status).bind(&input.status).bind(id).execute(&app.db).await?;
    if result.rows_affected() == 0 {
        return Err(bad("评论不存在、已删除或需先完成迁移映射"));
    }
    Ok(Json(json!({"ok": true})))
}
#[derive(Deserialize)]
struct Reply {
    body: String,
}
async fn admin_reply(
    State(app): State<App>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(input): Json<Reply>,
) -> Result<impl IntoResponse, ApiError> {
    auth(&app, &headers, true).await?;
    let page: Option<String> = sqlx::query_scalar("SELECT page FROM comments WHERE id=?")
        .bind(id)
        .fetch_optional(&app.db)
        .await?;
    let page = page.ok_or_else(|| bad("评论不存在"))?;
    let id = insert(
        &app,
        NewComment {
            page,
            parent_id: Some(id),
            nick: "博主".into(),
            body: input.body,
            email: String::new(),
            website: String::new(),
            company: String::new(),
        },
        true,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}
