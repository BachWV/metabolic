use crate::{api::App, db::Comment, util};
use anyhow::Result;
use lettre::{
    transport::smtp::authentication::Credentials, AsyncSmtpTransport, AsyncTransport, Message,
    Tokio1Executor,
};
use std::time::Duration;

pub fn transport(app: &App) -> Result<Option<AsyncSmtpTransport<Tokio1Executor>>> {
    let c = &app.config;
    if c.smtp_host.is_empty() {
        return Ok(None);
    }
    let _: lettre::message::Mailbox = c.mail_from.parse()?;
    let _: lettre::message::Mailbox = c.mail_to.parse()?;
    let builder = if c.smtp_port == 465 {
        AsyncSmtpTransport::<Tokio1Executor>::relay(&c.smtp_host)?
    } else {
        AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&c.smtp_host)?
    };
    Ok(Some(
        builder
            .port(c.smtp_port)
            .credentials(Credentials::new(
                c.smtp_user.clone(),
                c.smtp_password.clone(),
            ))
            .timeout(Some(Duration::from_secs(15)))
            .build(),
    ))
}
pub async fn finish(app: &App, id: i64, attempts: i64, success: bool) -> Result<()> {
    let state = if success {
        "sent"
    } else if attempts >= 5 {
        "failed"
    } else {
        "pending"
    };
    sqlx::query("UPDATE mail_jobs SET state=?,next_attempt=? WHERE id=?")
        .bind(state)
        .bind(util::now() + 60 * 2i64.pow(attempts.min(5) as u32))
        .bind(id)
        .execute(&app.db)
        .await?;
    if state == "failed" {
        tracing::error!(
            job_id = id,
            "mail exhausted five attempts; use retry-mail after fixing SMTP"
        );
    }
    Ok(())
}
pub async fn tick(app: &App, transport: &AsyncSmtpTransport<Tokio1Executor>) -> Result<()> {
    sqlx::query("UPDATE mail_jobs SET state='failed' WHERE state='pending' AND attempts>=5")
        .execute(&app.db)
        .await?;
    let jobs: Vec<(i64, i64, i64)> = sqlx::query_as("SELECT id,comment_id,attempts FROM mail_jobs WHERE state='pending' AND next_attempt<=? ORDER BY id LIMIT 10")
        .bind(util::now()).fetch_all(&app.db).await?;
    for (id, comment_id, attempts) in jobs {
        let c = sqlx::query_as::<_, Comment>("SELECT * FROM comments WHERE id=?")
            .bind(comment_id)
            .fetch_one(&app.db)
            .await?;
        if c.status != "published" {
            finish(app, id, attempts, true).await?;
            continue;
        }
        // Persist the attempt before sending. A process crash may cause duplicate delivery,
        // but will never silently drop the durable job.
        sqlx::query("UPDATE mail_jobs SET attempts=attempts+1,next_attempt=? WHERE id=?")
            .bind(util::now() + 120)
            .bind(id)
            .execute(&app.db)
            .await?;
        let mut link = url::Url::parse(&app.config.site_url)?;
        link.set_path(&c.page);
        link.set_fragment(Some(&format!("comment-{}", c.id)));
        let message = Message::builder()
            .from(app.config.mail_from.parse()?)
            .to(app.config.mail_to.parse()?)
            .subject("博客收到新评论")
            .body(format!(
                "昵称：{}\n文章：{}\n\n{}\n\n请在评论服务的 /admin 页面管理。",
                c.nick, link, c.body
            ))?;
        let sent = tokio::time::timeout(Duration::from_secs(20), transport.send(message)).await;
        let success = matches!(sent, Ok(Ok(_)));
        if !success {
            tracing::warn!(
                job_id = id,
                "SMTP delivery failed or timed out; retry scheduled"
            );
        }
        finish(app, id, attempts + 1, success).await?;
    }
    Ok(())
}
pub async fn run(app: App, transport: AsyncSmtpTransport<Tokio1Executor>) {
    let mut timer = tokio::time::interval(Duration::from_secs(10));
    loop {
        timer.tick().await;
        if let Err(e) = tick(&app, &transport).await {
            tracing::error!(error=%e, "mail worker failed");
        }
    }
}
