CREATE TABLE IF NOT EXISTS comments (
    id INTEGER PRIMARY KEY,
    page TEXT NOT NULL,
    parent_id INTEGER REFERENCES comments(id),
    nick TEXT NOT NULL,
    email TEXT NOT NULL DEFAULT '',
    website TEXT NOT NULL DEFAULT '',
    body TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'published' CHECK(status IN ('published','hidden','deleted','held')),
    is_admin INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    legacy_id TEXT UNIQUE,
    migration_note TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS comments_page_parent ON comments(page,parent_id,created_at,id);
CREATE TABLE IF NOT EXISTS sessions (
    token_hash TEXT PRIMARY KEY,
    csrf_hash TEXT NOT NULL,
    expires_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS mail_jobs (
    id INTEGER PRIMARY KEY,
    comment_id INTEGER NOT NULL UNIQUE REFERENCES comments(id),
    attempts INTEGER NOT NULL DEFAULT 0,
    next_attempt INTEGER NOT NULL,
    state TEXT NOT NULL DEFAULT 'pending' CHECK(state IN ('pending','sent','failed'))
);
CREATE INDEX IF NOT EXISTS mail_jobs_due ON mail_jobs(state,next_attempt);
PRAGMA user_version = 1;
