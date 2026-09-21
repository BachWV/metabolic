use crate::{db, util};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::Path,
};

#[derive(Deserialize)]
pub struct Legacy {
    #[serde(rename = "objectId")]
    id: String,
    url: String,
    #[serde(default)]
    pid: String,
    #[serde(default)]
    rid: String,
    nick: String,
    #[serde(default)]
    mail: String,
    #[serde(default)]
    link: String,
    comment: String,
    status: String,
    #[serde(rename = "createdAt")]
    created_at: String,
    #[serde(default, rename = "insertedAt")]
    inserted_at: String,
}
pub type Mapping = BTreeMap<String, Option<String>>;
pub fn read(path: &Path) -> Result<Vec<Legacy>> {
    let rows: Vec<Legacy> = serde_json::from_slice(&std::fs::read(path)?)?;
    let mut ids = HashSet::new();
    for row in &rows {
        if row.id.is_empty() || !ids.insert(&row.id) {
            bail!("duplicate or empty legacy ID");
        }
        timestamp(row)?;
    }
    Ok(rows)
}
fn timestamp(row: &Legacy) -> Result<i64> {
    let raw = if row.inserted_at.is_empty() {
        &row.created_at
    } else {
        &row.inserted_at
    };
    Ok(chrono::DateTime::parse_from_rfc3339(raw)
        .context("invalid legacy timestamp")?
        .timestamp())
}
fn mapped(row: &Legacy, mapping: &Mapping) -> Result<Option<String>> {
    mapping
        .get(&row.url)
        .and_then(Option::as_ref)
        .map(|s| util::page(s))
        .transpose()
}
fn has_page(dir: &Path, page: &str) -> bool {
    std::fs::read_to_string(dir.join(page.trim_start_matches('/')).join("index.html"))
        .is_ok_and(|html| html.contains("id=\"blog-comments\""))
}
pub fn preflight(rows: &[Legacy], public_dir: Option<&Path>) -> Result<serde_json::Value> {
    let ids: HashSet<_> = rows.iter().map(|r| r.id.as_str()).collect();
    let mut mapping = Mapping::new();
    for row in rows {
        // Full URLs and test pages always require human mapping, even if their paths exist.
        let candidate = util::page(&row.url)
            .ok()
            .filter(|p| p != "/test/")
            .filter(|p| public_dir.is_some_and(|d| has_page(d, p)));
        mapping.insert(row.url.clone(), candidate);
    }
    let missing: Vec<_> = rows
        .iter()
        .filter(|r| !r.pid.is_empty() && !ids.contains(r.pid.as_str()))
        .map(|r| &r.id)
        .collect();
    Ok(
        serde_json::json!({"total":rows.len(), "replies": rows.iter().filter(|r| !r.pid.is_empty()).count(), "missing_parent_ids":missing, "mapping":mapping}),
    )
}
#[derive(Serialize)]
pub struct Report {
    pub total: usize,
    pub inserted: usize,
    pub remapped: usize,
    pub unchanged: usize,
    pub held: usize,
}
pub async fn import(database: &str, rows: &[Legacy], mapping: &Mapping) -> Result<Report> {
    // Validate every mapping before touching the database.
    for value in mapping.values().flatten() {
        util::page(value)?;
    }
    let pool = db::open(database).await?;
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let mut report = Report {
        total: rows.len(),
        inserted: 0,
        remapped: 0,
        unchanged: 0,
        held: 0,
    };
    let parents: HashMap<_, _> = rows
        .iter()
        .map(|r| (r.id.as_str(), r.pid.as_str()))
        .collect();
    let mut touched = HashSet::new();
    for row in rows {
        let target = mapped(row, mapping)?;
        let status = if target.is_none() {
            "held"
        } else if row.status == "approved" {
            "published"
        } else {
            "hidden"
        };
        if status == "held" {
            report.held += 1;
        }
        let page = target.unwrap_or_else(|| format!("/__legacy/{}/", util::hash(&row.url)));
        let existing: Option<(i64, String)> =
            sqlx::query_as("SELECT id,status FROM comments WHERE legacy_id=?")
                .bind(&row.id)
                .fetch_optional(&mut *tx)
                .await?;
        if let Some((id, old_status)) = existing {
            if old_status == "held" && status != "held" {
                sqlx::query("UPDATE comments SET page=?,status=?,parent_id=NULL WHERE id=?")
                    .bind(&page)
                    .bind(status)
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
                report.remapped += 1;
                touched.insert(row.id.clone());
            } else {
                report.unchanged += 1;
            }
            continue;
        }
        let website = util::website(&row.link).unwrap_or_default();
        sqlx::query("INSERT INTO comments(page,nick,email,website,body,status,created_at,legacy_id) VALUES(?,?,?,?,?,?,?,?)")
            .bind(page).bind(&row.nick).bind(&row.mail).bind(website).bind(&row.comment).bind(status).bind(timestamp(row)?).bind(&row.id).execute(&mut *tx).await?;
        touched.insert(row.id.clone());
        report.inserted += 1;
    }
    for row in rows.iter().filter(|r| touched.contains(&r.id)) {
        if row.pid.is_empty() {
            continue;
        }
        let mut seen = HashSet::new();
        seen.insert(row.id.as_str());
        let mut cursor = row.pid.as_str();
        let mut cycle = false;
        while !cursor.is_empty() {
            if !seen.insert(cursor) {
                cycle = true;
                break;
            }
            cursor = parents.get(cursor).copied().unwrap_or("");
        }
        let parent: Option<i64> = if cycle {
            None
        } else {
            sqlx::query_scalar("SELECT p.id FROM comments p JOIN comments c ON c.page=p.page WHERE p.legacy_id=? AND c.legacy_id=?")
                .bind(&row.pid).bind(&row.id).fetch_optional(&mut *tx).await?
        };
        let note = if parent.is_some() {
            String::new()
        } else {
            format!(
                "Parent unavailable, cross-page or cyclic; retained as root. pid={} rid={}",
                row.pid, row.rid
            )
        };
        sqlx::query("UPDATE comments SET parent_id=?,migration_note=? WHERE legacy_id=?")
            .bind(parent)
            .bind(note)
            .bind(&row.id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    pool.close().await;
    Ok(report)
}
