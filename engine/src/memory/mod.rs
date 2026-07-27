use crate::config::{AppConfig, KnowledgeBasePluginConfig, MemoryConfig};
use crate::paths::MossPaths;
use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::PathBuf;

#[derive(Clone)]
pub struct MemoryStore {
    config: MemoryConfig,
    kb_config: KnowledgeBasePluginConfig,
    data_db: PathBuf,
    state_db: PathBuf,
    skills_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub struct EvictedTurn {
    pub source_id: String,
    pub timestamp: String,
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct AssociationContext {
    pub facts: Vec<MemoryHit>,
    pub episodes: Vec<MemoryHit>,
}

#[derive(Debug, Clone)]
pub struct MemoryHit {
    pub id: i64,
    pub content: String,
    pub score: f32,
    pub timestamp: String,
    pub source: String,
}

type EvictedRow = (i64, String, String, String);

/// Name of the FTS5 virtual table that indexes `evicted_turns.content`.
///
/// It is an "external content" table (the real text lives only in
/// `evicted_turns`; this table just stores the index), kept in sync via
/// SQL triggers so that inserts coming through *any* connection — including
/// the `ATTACH DATABASE` path used by `ConversationDb::archive_and_delete_visible_turns`
/// to archive turns directly — stay indexed without any Rust-side plumbing.
const EVICTED_FTS_TABLE: &str = "evicted_turns_fts";

/// Upper bound on how many keyword-matching candidate rows are pulled out of
/// SQLite before scoring/snippeting in Rust. This is a safety valve applied
/// *after* SQL-side keyword filtering (FTS MATCH or LIKE) — never a recency
/// window applied before filtering, which was the original defect
/// (`ORDER BY id DESC LIMIT 1000` silently hid any older archived turn).
const EVICTED_SEARCH_CANDIDATE_CAP: i64 = 5000;

impl MemoryStore {
    pub fn new(config: &AppConfig, paths: &MossPaths) -> Self {
        let data_dir = config.active_persona_memory_data_dir(paths).join("memory");
        let state_dir = config.active_persona_memory_state_dir(paths).join("memory");
        Self {
            config: config.memory_config().clone(),
            kb_config: config.plugins.knowledge_base.clone(),
            data_db: data_dir.join("memory.db"),
            state_db: state_dir.join("evicted_context.db"),
            skills_dir: config.active_persona_skills_dir(paths),
        }
    }

    pub fn init(&self) -> Result<()> {
        if let Some(parent) = self.data_db.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if let Some(parent) = self.state_db.parent() {
            std::fs::create_dir_all(parent)?;
        }
        init_data_db(&self.data_conn()?)?;
        init_state_db(&self.state_conn()?)?;
        self.decay_memories()?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn remember_evicted_turns(&self, turns: &[EvictedTurn]) -> Result<()> {
        if !self.config.enabled || !self.config.evicted_context_enabled || turns.is_empty() {
            return Ok(());
        }
        self.init()?;
        let mut conn = self.state_conn()?;
        let tx = conn.transaction()?;
        for turn in turns {
            tx.execute(
                "INSERT OR IGNORE INTO evicted_turns (source_id, timestamp, role, content, created_at)
                  VALUES (?1, ?2, ?3, ?4, ?5)",
                params![turn.source_id, turn.timestamp, turn.role, turn.content, now()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn prepare_evicted_context_db(&self) -> Result<Option<PathBuf>> {
        if !self.config.enabled || !self.config.evicted_context_enabled {
            return Ok(None);
        }
        self.init()?;
        Ok(Some(self.state_db.clone()))
    }

    pub fn clear_evicted_context(&self) -> Result<()> {
        self.init()?;
        self.state_conn()?
            .execute("DELETE FROM evicted_turns", [])?;
        Ok(())
    }

    pub fn clear_pending_events(&self) -> Result<()> {
        self.init()?;
        let data = self.data_conn()?;
        data.execute("DELETE FROM pending_events", [])?;
        data.execute(
            "DELETE FROM sqlite_sequence WHERE name = 'pending_events'",
            [],
        )?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn search_evicted_context(&self, query: &str, limit: usize) -> Result<Value> {
        self.init()?;
        self.search_evicted_context_existing(query, limit)
    }

    pub fn search_evicted_context_readonly(&self, query: &str, limit: usize) -> Result<Value> {
        if !self.state_db.is_file() {
            return Ok(json!({ "ok": true, "query": query, "results": [] }));
        }
        self.search_evicted_context_existing(query, limit)
    }

    fn search_evicted_context_existing(&self, query: &str, limit: usize) -> Result<Value> {
        let tokens = query_tokens(query);
        if tokens.is_empty() {
            return Ok(json!({ "ok": true, "query": query, "results": [] }));
        }
        let conn = self.state_conn()?;
        // Idempotent migration: creates the FTS index (and backfills it from
        // whatever rows already exist) the first time this DB is opened
        // after upgrading, then becomes a cheap existence check on every
        // later call.
        let fts_ready = ensure_evicted_fts(&conn).unwrap_or(false);
        let candidates = fetch_evicted_candidates(&conn, &tokens, fts_ready)?;
        let mut hits = Vec::new();
        for (id, timestamp, role, content) in candidates {
            let score = score_text(&content, &tokens);
            if score <= 0.0 {
                continue;
            }
            hits.push(json!({
                "id": id,
                "timestamp": timestamp,
                "role": role,
                "score": score,
                "snippet": snippet(&content, &tokens, self.kb_config.snippet_context_chars),
            }));
        }
        sort_json_hits(&mut hits);
        hits.truncate(limit.clamp(1, 50));
        Ok(json!({ "ok": true, "query": query, "results": hits }))
    }

    pub fn remember_fact(&self, content: &str, source: &str) -> Result<i64> {
        if !self.config.enabled || content.trim().is_empty() {
            return Ok(0);
        }
        self.init()?;
        let conn = self.data_conn()?;
        conn.execute(
            "INSERT INTO facts (content, source, status, confidence, recall_count, created_at, updated_at) VALUES (?1, ?2, 'active', 1.0, 0, ?3, ?3)",
            params![content.trim(), source.trim(), now()],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn remember_pending_event(
        &self,
        user_message: &str,
        assistant_message: &str,
    ) -> Result<()> {
        if !self.config.enabled || !self.config.auto_diary_enabled {
            return Ok(());
        }
        self.init()?;
        self.data_conn()?.execute(
            "INSERT INTO pending_events (user_message, assistant_message, created_at) VALUES (?1, ?2, ?3)",
            params![user_message.trim(), assistant_message.trim(), now()],
        )?;
        Ok(())
    }

    pub fn process_after_turn(&self, user_message: &str, assistant_message: &str) -> Result<()> {
        self.remember_pending_event(user_message, assistant_message)?;
        self.flush_pending_events()?;
        Ok(())
    }

    pub fn stats(&self) -> Result<Value> {
        self.init()?;
        self.prune_missing_skill_records()?;
        let data = self.data_conn()?;
        let state = self.state_conn()?;
        Ok(json!({
            "ok": true,
            "data_db": self.data_db.display().to_string(),
            "state_db": self.state_db.display().to_string(),
            "skills_dir": self.skills_dir.display().to_string(),
            "facts": count_rows(&data, "facts")?,
            "episodes": count_rows(&data, "episodes")?,
            "unprocessed_pending_events": count_where(&data, "pending_events", "processed_at IS NULL")?,
            "total_pending_events": count_rows(&data, "pending_events")?,
            "skill_records": count_rows(&data, "skill_records")?,
            "skill_dirs": count_skill_dirs(&self.skills_dir)?,
            "evicted_turns": count_rows(&state, "evicted_turns")?,
        }))
    }

    pub fn reset_all(&self, include_skills: bool) -> Result<()> {
        self.init()?;
        let data = self.data_conn()?;
        data.execute("DELETE FROM facts", [])?;
        data.execute("DELETE FROM episodes", [])?;
        data.execute("DELETE FROM pending_events", [])?;
        data.execute("DELETE FROM skill_records", [])?;
        data.execute(
            "DELETE FROM sqlite_sequence WHERE name IN ('facts', 'episodes', 'pending_events', 'skill_records')",
            [],
        )?;
        self.clear_evicted_context()?;
        if include_skills {
            self.remove_auto_skills()?;
        }
        Ok(())
    }

    fn remove_auto_skills(&self) -> Result<()> {
        if !self.skills_dir.exists() {
            return Ok(());
        }
        for entry in std::fs::read_dir(&self.skills_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let skill_file = entry.path().join("SKILL.md");
            let raw = std::fs::read_to_string(&skill_file).unwrap_or_default();
            if raw.contains("Auto-learned method from assistant conversation")
                || raw.contains("Auto-learned method from Moss conversation")
                || raw.contains("generated_by: moss")
            {
                std::fs::remove_dir_all(entry.path())?;
            }
        }
        Ok(())
    }

    fn flush_pending_events(&self) -> Result<()> {
        if !self.config.enabled || !self.config.auto_diary_enabled {
            return Ok(());
        }
        self.init()?;
        let conn = self.data_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, user_message, assistant_message, created_at FROM pending_events WHERE processed_at IS NULL ORDER BY id LIMIT 20",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        for row in rows {
            let (id, user, assistant, created_at) = row?;
            let content = format!(
                "{}，我被要求：{}；结果：{}",
                created_at,
                truncate_chars(&compact_line(&user), 260),
                truncate_chars(&compact_line(&assistant), 520)
            );
            conn.execute(
                "INSERT INTO episodes (content, source, status, recall_count, created_at, updated_at) VALUES (?1, 'episode', 'active', 0, ?2, ?2)",
                params![content, created_at],
            )?;
            conn.execute(
                "UPDATE pending_events SET processed_at=?1 WHERE id=?2",
                params![now(), id],
            )?;
        }
        Ok(())
    }

    fn prune_missing_skill_records(&self) -> Result<()> {
        let conn = self.data_conn()?;
        let mut stmt = conn.prepare("SELECT id, path FROM skill_records")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut missing = Vec::new();
        for row in rows {
            let (id, path) = row?;
            if !PathBuf::from(path).exists() {
                missing.push(id);
            }
        }
        drop(stmt);
        for id in missing {
            conn.execute("DELETE FROM skill_records WHERE id=?1", params![id])?;
        }
        Ok(())
    }

    pub fn recall_memories(
        &self,
        query: &str,
        limit: usize,
        include_forgotten: bool,
    ) -> Result<Value> {
        self.init()?;
        self.recall_memories_existing(query, limit, include_forgotten)
    }

    pub fn recall_memories_readonly(
        &self,
        query: &str,
        limit: usize,
        include_forgotten: bool,
    ) -> Result<Value> {
        if !self.data_db.is_file() {
            return Ok(json!({ "ok": true, "query": query, "facts": [], "episodes": [] }));
        }
        self.recall_memories_existing(query, limit, include_forgotten)
    }

    fn recall_memories_existing(
        &self,
        query: &str,
        limit: usize,
        include_forgotten: bool,
    ) -> Result<Value> {
        let facts = self.search_facts(query, limit, include_forgotten)?;
        let episodes = self.search_episodes(query, limit, include_forgotten)?;
        Ok(json!({
            "ok": true,
            "query": query,
            "facts": facts.iter().map(memory_hit_json).collect::<Vec<_>>(),
            "episodes": episodes.iter().map(memory_hit_json).collect::<Vec<_>>(),
        }))
    }

    #[allow(dead_code)]
    pub fn recall_past_events(&self, query: &str, limit: usize) -> Result<Value> {
        self.init()?;
        self.recall_past_events_existing(query, limit)
    }

    pub fn recall_past_events_readonly(&self, query: &str, limit: usize) -> Result<Value> {
        if !self.data_db.is_file() {
            return Ok(json!({ "ok": true, "query": query, "episodes": [] }));
        }
        self.recall_past_events_existing(query, limit)
    }

    fn recall_past_events_existing(&self, query: &str, limit: usize) -> Result<Value> {
        let episodes = self.search_episodes(query, limit, true)?;
        Ok(json!({
            "ok": true,
            "query": query,
            "episodes": episodes.iter().map(memory_hit_json).collect::<Vec<_>>(),
        }))
    }

    pub fn association(&self, query: &str) -> Result<Option<AssociationContext>> {
        if !self.config.enabled || !self.config.association_enabled {
            return Ok(None);
        }
        self.init()?;
        let facts = self.search_facts(query, self.config.association_facts, false)?;
        let episodes = self.search_episodes(query, self.config.association_episodes, false)?;
        for hit in facts.iter().chain(episodes.iter()) {
            self.reinforce(hit.id, &hit.source)?;
        }
        if facts.is_empty() && episodes.is_empty() {
            return Ok(None);
        }
        Ok(Some(AssociationContext { facts, episodes }))
    }

    pub fn format_association(&self, association: &AssociationContext) -> String {
        let mut output = String::new();
        output.push_str("<associative-memory>\n");
        output.push_str("以下是根据当前用户输入联想到的旧记忆，可能相关也可能不相关；必要时使用，不要强行引用。\n");
        if !association.facts.is_empty() {
            output.push_str("\n曾经记住的相关知识点：\n");
            for hit in &association.facts {
                output.push_str("- ");
                output.push_str(&compact_line(&hit.content));
                output.push('\n');
            }
        }
        if !association.episodes.is_empty() {
            output.push_str("\n曾经发生的事情：\n");
            for hit in &association.episodes {
                output.push_str("- ");
                output.push_str(&compact_line(&hit.content));
                output.push('\n');
            }
        }
        output.push_str("</associative-memory>");
        truncate_chars(&output, self.config.association_max_chars)
    }

    fn search_facts(
        &self,
        query: &str,
        limit: usize,
        include_forgotten: bool,
    ) -> Result<Vec<MemoryHit>> {
        self.search_table("facts", query, limit, include_forgotten)
    }

    fn search_episodes(
        &self,
        query: &str,
        limit: usize,
        include_forgotten: bool,
    ) -> Result<Vec<MemoryHit>> {
        self.search_table("episodes", query, limit, include_forgotten)
    }

    fn search_table(
        &self,
        table: &str,
        query: &str,
        limit: usize,
        include_forgotten: bool,
    ) -> Result<Vec<MemoryHit>> {
        let tokens = query_tokens(query);
        let sql = format!(
            "SELECT id, content, source, status, created_at FROM {table} ORDER BY updated_at DESC LIMIT 1000"
        );
        let conn = self.data_conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        let mut hits = Vec::new();
        for row in rows {
            let (id, content, source, status, timestamp) = row?;
            if !include_forgotten && status == "forgotten" {
                continue;
            }
            let score = score_text(&content, &tokens);
            if score <= 0.0 {
                continue;
            }
            hits.push(MemoryHit {
                id,
                content,
                score,
                timestamp,
                source,
            });
        }
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        hits.truncate(limit.clamp(1, 50));
        Ok(hits)
    }

    fn reinforce(&self, id: i64, source: &str) -> Result<()> {
        let table = if source == "episode" {
            "episodes"
        } else {
            "facts"
        };
        let sql = format!(
            "UPDATE {table} SET recall_count=recall_count+1, strength=MIN(1.0, strength+?1), last_recalled_at=?2, updated_at=?2, status='active' WHERE id=?3"
        );
        self.data_conn()?.execute(
            &sql,
            params![self.config.forgetting_review_boost, now(), id],
        )?;
        Ok(())
    }

    fn decay_memories(&self) -> Result<()> {
        if !self.config.enabled || !self.config.forgetting_enabled {
            return Ok(());
        }
        let conn = self.data_conn()?;
        decay_table(&conn, "facts", &self.config)?;
        decay_table(&conn, "episodes", &self.config)?;
        Ok(())
    }

    fn data_conn(&self) -> Result<Connection> {
        Ok(Connection::open(&self.data_db)?)
    }

    fn state_conn(&self) -> Result<Connection> {
        Ok(Connection::open(&self.state_db)?)
    }
}

fn init_data_db(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS facts (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            content TEXT NOT NULL,
            source TEXT NOT NULL DEFAULT '',
            status TEXT NOT NULL DEFAULT 'active',
            confidence REAL NOT NULL DEFAULT 1.0,
            strength REAL NOT NULL DEFAULT 1.0,
            recall_count INTEGER NOT NULL DEFAULT 0,
            last_recalled_at TEXT,
            last_decay_at TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS episodes (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            content TEXT NOT NULL,
            source TEXT NOT NULL DEFAULT 'episode',
            status TEXT NOT NULL DEFAULT 'active',
            strength REAL NOT NULL DEFAULT 1.0,
            recall_count INTEGER NOT NULL DEFAULT 0,
            last_recalled_at TEXT,
            last_decay_at TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS pending_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            user_message TEXT NOT NULL,
            assistant_message TEXT NOT NULL,
            created_at TEXT NOT NULL,
            processed_at TEXT
        );
        CREATE TABLE IF NOT EXISTS skill_records (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL UNIQUE,
            path TEXT NOT NULL,
            summary TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );",
    )?;
    add_column_if_missing(conn, "facts", "strength", "REAL NOT NULL DEFAULT 1.0")?;
    add_column_if_missing(conn, "facts", "last_decay_at", "TEXT")?;
    add_column_if_missing(conn, "episodes", "strength", "REAL NOT NULL DEFAULT 1.0")?;
    add_column_if_missing(conn, "episodes", "last_decay_at", "TEXT")?;
    Ok(())
}

fn init_state_db(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS evicted_turns (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            source_id TEXT,
            timestamp TEXT NOT NULL,
            role TEXT NOT NULL,
            content TEXT NOT NULL,
            created_at TEXT NOT NULL
        );",
    )?;
    add_column_if_missing(conn, "evicted_turns", "source_id", "TEXT")?;
    conn.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_evicted_turns_source_id
         ON evicted_turns(source_id) WHERE source_id IS NOT NULL",
        [],
    )?;
    // Best-effort: search still works (via the LIKE fallback) if this fails.
    let _ = ensure_evicted_fts(conn)?;
    Ok(())
}

/// Ensures the trigram FTS5 index over `evicted_turns.content` exists,
/// creating and backfilling it on first use (covering both brand-new DBs and
/// pre-upgrade DBs that only have the base table). Returns `Ok(true)` if the
/// index is ready to search, or `Ok(false)` if this SQLite build lacks FTS5
/// or the `trigram` tokenizer, in which case callers should fall back to a
/// plain `LIKE` scan.
///
/// Idempotent and cheap to call repeatedly: once the table exists this is a
/// single `sqlite_master` lookup.
fn ensure_evicted_fts(conn: &Connection) -> Result<bool> {
    if table_exists(conn, EVICTED_FTS_TABLE)? {
        return Ok(true);
    }
    let created = conn
        .execute(
            &format!(
                "CREATE VIRTUAL TABLE IF NOT EXISTS {EVICTED_FTS_TABLE} USING fts5(
                    content,
                    content='evicted_turns',
                    content_rowid='id',
                    tokenize='trigram'
                )"
            ),
            [],
        )
        .is_ok();
    if !created {
        return Ok(false);
    }
    create_evicted_fts_triggers(conn)?;
    // Backfill: rebuilds the entire index from the current contents of
    // `evicted_turns`. This is what makes the migration idempotent and safe
    // to run against a DB that already has rows archived before this index
    // (or before this process) existed.
    conn.execute(
        &format!("INSERT INTO {EVICTED_FTS_TABLE}({EVICTED_FTS_TABLE}) VALUES('rebuild')"),
        [],
    )?;
    Ok(true)
}

/// Keeps `evicted_turns_fts` in sync on every insert/update/delete of
/// `evicted_turns`, regardless of which connection performs the write.
///
/// This matters because turns are archived via `ConversationDb::archive_and_delete_visible_turns`,
/// which `ATTACH DATABASE`s this file from the *conversation* DB connection
/// and inserts into `evicted_turns` through that alias — a codepath that
/// never goes through any `MemoryStore` method. Triggers live with the
/// table itself, so they still fire in that scenario.
fn create_evicted_fts_triggers(conn: &Connection) -> Result<()> {
    conn.execute_batch(&format!(
        "CREATE TRIGGER IF NOT EXISTS evicted_turns_fts_ai AFTER INSERT ON evicted_turns BEGIN
            INSERT INTO {table}(rowid, content) VALUES (new.id, new.content);
         END;
         CREATE TRIGGER IF NOT EXISTS evicted_turns_fts_ad AFTER DELETE ON evicted_turns BEGIN
            INSERT INTO {table}({table}, rowid, content) VALUES('delete', old.id, old.content);
         END;
         CREATE TRIGGER IF NOT EXISTS evicted_turns_fts_au AFTER UPDATE ON evicted_turns BEGIN
            INSERT INTO {table}({table}, rowid, content) VALUES('delete', old.id, old.content);
            INSERT INTO {table}(rowid, content) VALUES (new.id, new.content);
         END;",
        table = EVICTED_FTS_TABLE
    ))?;
    Ok(())
}

fn table_exists(conn: &Connection, name: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        params![name],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// Fetches candidate rows for a keyword search. When the trigram FTS index
/// is available it is used to narrow the whole archive down to matching
/// rows via an indexed lookup; otherwise falls back to a bounded `LIKE`
/// scan of the whole table. Either way, nothing is excluded based on
/// recency/row-position — only on whether the content matches.
fn fetch_evicted_candidates(
    conn: &Connection,
    tokens: &[String],
    fts_ready: bool,
) -> Result<Vec<EvictedRow>> {
    if fts_ready {
        fetch_evicted_candidates_via_fts(conn, tokens)
    } else {
        fetch_evicted_candidates_via_like(conn, tokens, EVICTED_SEARCH_CANDIDATE_CAP)
    }
}

/// The trigram tokenizer indexes overlapping 3-character windows, so it can
/// only match tokens with at least 3 characters (this is a fundamental
/// property of trigrams, not a bug). Longer tokens are matched via the FTS
/// index; shorter tokens (common for 2-character CJK words) fall back to a
/// bounded `LIKE` scan. Candidate ids from both are merged before fetching
/// full rows for Rust-side scoring/snippeting.
fn fetch_evicted_candidates_via_fts(
    conn: &Connection,
    tokens: &[String],
) -> Result<Vec<EvictedRow>> {
    let (long_tokens, short_tokens): (Vec<&String>, Vec<&String>) =
        tokens.iter().partition(|token| token.chars().count() >= 3);

    let mut ids: HashSet<i64> = HashSet::new();

    if !long_tokens.is_empty() {
        // Each token is matched as a quoted phrase, which makes FTS5's
        // trigram tokenizer perform a substring search (per SQLite docs)
        // rather than requiring the token to be a standalone "word" — there
        // is no word segmentation for CJK text to rely on anyway.
        let match_query = long_tokens
            .iter()
            .map(|token| format!("\"{}\"", escape_fts_phrase(token)))
            .collect::<Vec<_>>()
            .join(" OR ");
        let mut stmt = conn.prepare(&format!(
            "SELECT rowid FROM {EVICTED_FTS_TABLE} WHERE {EVICTED_FTS_TABLE} MATCH ?1
             LIMIT {EVICTED_SEARCH_CANDIDATE_CAP}"
        ))?;
        let rows = stmt.query_map(params![match_query], |row| row.get::<_, i64>(0))?;
        for row in rows {
            ids.insert(row?);
        }
    }

    if !short_tokens.is_empty() {
        let clause = short_tokens
            .iter()
            .map(|_| "content LIKE ? ESCAPE '\\'")
            .collect::<Vec<_>>()
            .join(" OR ");
        let sql = format!(
            "SELECT id FROM evicted_turns WHERE {clause} LIMIT {EVICTED_SEARCH_CANDIDATE_CAP}"
        );
        let like_params: Vec<String> = short_tokens
            .iter()
            .map(|token| format!("%{}%", escape_like(token)))
            .collect();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(like_params.iter()), |row| {
            row.get::<_, i64>(0)
        })?;
        for row in rows {
            ids.insert(row?);
        }
    }

    if ids.is_empty() {
        return Ok(Vec::new());
    }
    fetch_evicted_rows_by_ids(conn, &ids)
}

fn fetch_evicted_rows_by_ids(conn: &Connection, ids: &HashSet<i64>) -> Result<Vec<EvictedRow>> {
    let id_list: Vec<i64> = ids.iter().copied().collect();
    let mut result = Vec::with_capacity(id_list.len());
    for chunk in id_list.chunks(500) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT id, timestamp, role, content FROM evicted_turns WHERE id IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        for row in rows {
            result.push(row?);
        }
    }
    Ok(result)
}

/// Used when FTS5 or the `trigram` tokenizer is unavailable on this SQLite
/// build. Filters with SQL `LIKE` first (so the whole table is *searched*,
/// not skipped), then applies `cap` — the row cap is applied after keyword
/// filtering, never before it, which is the fix for the original defect.
fn fetch_evicted_candidates_via_like(
    conn: &Connection,
    tokens: &[String],
    cap: i64,
) -> Result<Vec<EvictedRow>> {
    let clause = tokens
        .iter()
        .map(|_| "content LIKE ? ESCAPE '\\'")
        .collect::<Vec<_>>()
        .join(" OR ");
    let sql = format!(
        "SELECT id, timestamp, role, content FROM evicted_turns WHERE {clause} LIMIT {cap}"
    );
    let like_params: Vec<String> = tokens
        .iter()
        .map(|token| format!("%{}%", escape_like(token)))
        .collect();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(like_params.iter()), |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn escape_fts_phrase(token: &str) -> String {
    token.replace('"', "\"\"")
}

fn escape_like(token: &str) -> String {
    token
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for row in rows {
        if row? == column {
            return Ok(());
        }
    }
    conn.execute(
        &format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"),
        [],
    )?;
    Ok(())
}

fn decay_table(conn: &Connection, table: &str, config: &MemoryConfig) -> Result<()> {
    let now = Utc::now();
    let mut stmt = conn.prepare(&format!(
        "SELECT id, strength, COALESCE(last_recalled_at, updated_at, created_at), last_decay_at FROM {table} WHERE status='active'"
    ))?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, f64>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    })?;
    let mut updates = Vec::new();
    for row in rows {
        let (id, strength, recalled_at, last_decay_at) = row?;
        let anchor = last_decay_at.as_deref().unwrap_or(&recalled_at);
        let Ok(anchor) = DateTime::parse_from_rfc3339(anchor) else {
            continue;
        };
        let days = (now - anchor.with_timezone(&Utc)).num_seconds().max(0) as f64 / 86_400.0;
        if days < 0.25 {
            continue;
        }
        let half_life = config.forgetting_half_life_days.max(0.1);
        let new_strength = strength * 2f64.powf(-days / half_life);
        let status = if new_strength < config.forgetting_min_strength {
            "forgotten"
        } else {
            "active"
        };
        updates.push((id, new_strength, status.to_string()));
    }
    drop(stmt);
    for (id, strength, status) in updates {
        conn.execute(
            &format!("UPDATE {table} SET strength=?1, status=?2, last_decay_at=?3 WHERE id=?4"),
            params![strength, status, now.to_rfc3339(), id],
        )?;
    }
    Ok(())
}

fn memory_hit_json(hit: &MemoryHit) -> Value {
    json!({
        "id": hit.id,
        "timestamp": hit.timestamp,
        "score": hit.score,
        "source": hit.source,
        "content": hit.content,
    })
}

fn sort_json_hits(hits: &mut [Value]) {
    hits.sort_by(|a, b| {
        b.get("score")
            .and_then(Value::as_f64)
            .unwrap_or_default()
            .partial_cmp(&a.get("score").and_then(Value::as_f64).unwrap_or_default())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

fn score_text(text: &str, tokens: &[String]) -> f32 {
    if tokens.is_empty() {
        return 0.0;
    }
    let lower = text.to_ascii_lowercase();
    let mut score = 0.0;
    let mut matched = HashSet::new();
    for token in tokens {
        if lower.contains(token) {
            score += 10.0;
            matched.insert(token);
        }
    }
    score + matched.len() as f32 / tokens.len() as f32 * 20.0
}

fn query_tokens(query: &str) -> Vec<String> {
    query
        .split(|ch: char| ch.is_whitespace() || ch.is_ascii_punctuation())
        .map(str::trim)
        .filter(|token| token.chars().count() >= 2)
        .map(|token| token.to_ascii_lowercase())
        .collect()
}

fn snippet(text: &str, tokens: &[String], max_chars: usize) -> String {
    let lower = text.to_ascii_lowercase();
    let start = tokens
        .iter()
        .filter_map(|token| lower.find(token))
        .min()
        .unwrap_or(0);
    let start = text[..start.min(text.len())]
        .char_indices()
        .rev()
        .nth(max_chars / 4)
        .map(|(index, _)| index)
        .unwrap_or(0);
    truncate_chars(&text[start..], max_chars)
}

fn compact_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    format!(
        "{}...",
        text.chars()
            .take(max_chars.saturating_sub(3))
            .collect::<String>()
    )
}

fn count_rows(conn: &Connection, table: &str) -> Result<i64> {
    let sql = format!("SELECT COUNT(*) FROM {table}");
    Ok(conn.query_row(&sql, [], |row| row.get(0))?)
}

fn count_where(conn: &Connection, table: &str, condition: &str) -> Result<i64> {
    let sql = format!("SELECT COUNT(*) FROM {table} WHERE {condition}");
    Ok(conn.query_row(&sql, [], |row| row.get(0))?)
}

fn count_skill_dirs(skills_dir: &PathBuf) -> Result<usize> {
    if !skills_dir.exists() {
        return Ok(0);
    }
    let mut count = 0usize;
    for entry in std::fs::read_dir(skills_dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() && entry.path().join("SKILL.md").is_file() {
            count += 1;
        }
    }
    Ok(count)
}

fn now() -> String {
    Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use crate::paths::MossPaths;

    fn test_paths(temp: &tempfile::TempDir) -> MossPaths {
        MossPaths {
            config_dir: temp.path().join("config"),
            config_file: temp.path().join("config/config.jsonc"),
            skills_dir: temp.path().join("config/skills"),
            data_dir: temp.path().join("data"),
            cache_dir: temp.path().join("cache"),
            state_dir: temp.path().join("state"),
            pictures_dir: temp.path().join("pictures"),
            fish_hook_file: temp.path().join("fish/moss.fish"),
            bash_hook_file: temp.path().join("shell/bash-hook.sh"),
            zsh_hook_file: temp.path().join("shell/zsh-hook.zsh"),
            scripts_dir: temp.path().join("config/scripts"),
            system_scripts_dir: PathBuf::new(),
        }
    }

    #[test]
    fn remembers_and_recalls_fact() {
        let temp = tempfile::tempdir().unwrap();
        let config = AppConfig::default();
        let paths = test_paths(&temp);
        let store = MemoryStore::new(&config, &paths);
        store
            .remember_fact("Niri 输入法需要 XMODIFIERS", "test")
            .unwrap();
        let result = store.recall_memories("Niri XMODIFIERS", 5, false).unwrap();
        assert!(result.to_string().contains("XMODIFIERS"));
    }

    #[test]
    fn reset_all_clears_facts_and_episodes() {
        let temp = tempfile::tempdir().unwrap();
        let config = AppConfig::default();
        let paths = test_paths(&temp);
        let store = MemoryStore::new(&config, &paths);
        store
            .remember_fact("Niri 输入法需要 XMODIFIERS", "test")
            .unwrap();
        store.remember_pending_event("你好", "在呢").unwrap();
        store.flush_pending_events().unwrap();

        let before = store.recall_memories("你好 XMODIFIERS", 5, false).unwrap();
        assert!(!before["facts"].as_array().unwrap().is_empty());
        assert!(!before["episodes"].as_array().unwrap().is_empty());

        store.reset_all(false).unwrap();

        let after = store.recall_memories("你好 XMODIFIERS", 5, false).unwrap();
        assert!(after["facts"].as_array().unwrap().is_empty());
        assert!(after["episodes"].as_array().unwrap().is_empty());
    }

    #[test]
    fn evicted_context_can_be_cleared() {
        let temp = tempfile::tempdir().unwrap();
        let config = AppConfig::default();
        let paths = test_paths(&temp);
        let store = MemoryStore::new(&config, &paths);
        store
            .remember_evicted_turns(&[EvictedTurn {
                source_id: "turn-1:user".to_string(),
                timestamp: "now".to_string(),
                role: "user".to_string(),
                content: "旧上下文 输入法".to_string(),
            }])
            .unwrap();
        store
            .remember_evicted_turns(&[EvictedTurn {
                source_id: "turn-1:user".to_string(),
                timestamp: "now".to_string(),
                role: "user".to_string(),
                content: "旧上下文 输入法".to_string(),
            }])
            .unwrap();
        assert_eq!(
            store.search_evicted_context("输入法", 5).unwrap()["results"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(store
            .search_evicted_context("输入法", 5)
            .unwrap()
            .to_string()
            .contains("旧上下文"));
        store.clear_evicted_context().unwrap();
        assert!(!store
            .search_evicted_context("输入法", 5)
            .unwrap()
            .to_string()
            .contains("旧上下文"));
    }

    #[test]
    fn evicted_context_trigram_fts_is_available_in_this_sqlite_build() {
        // rusqlite's `bundled` feature compiles SQLite with `-DSQLITE_ENABLE_FTS5`
        // unconditionally, and the vendored SQLite version is well above the
        // 3.34.0 minimum for the `trigram` tokenizer, so this should always
        // succeed. If it ever doesn't, `ensure_evicted_fts` still degrades
        // gracefully to a LIKE-based scan (see the other tests below), but we
        // want to know loudly if the preferred path stops being available.
        let temp = tempfile::tempdir().unwrap();
        let config = AppConfig::default();
        let paths = test_paths(&temp);
        let store = MemoryStore::new(&config, &paths);
        store.init().unwrap();
        let conn = Connection::open(&store.state_db).unwrap();
        assert!(
            table_exists(&conn, EVICTED_FTS_TABLE).unwrap(),
            "expected the trigram FTS5 index to be created on this build"
        );
    }

    #[test]
    fn evicted_context_search_covers_rows_beyond_old_thousand_row_window() {
        let temp = tempfile::tempdir().unwrap();
        let config = AppConfig::default();
        let paths = test_paths(&temp);
        let store = MemoryStore::new(&config, &paths);

        // Archive the target turn first, so it gets the lowest `id`. The old
        // buggy query was `ORDER BY id DESC LIMIT 1000`, which would drop the
        // oldest rows entirely once more than 1000 newer turns were archived
        // afterward — regardless of whether they matched the search.
        store
            .remember_evicted_turns(&[EvictedTurn {
                source_id: "target-turn".to_string(),
                timestamp: "now".to_string(),
                role: "user".to_string(),
                content: "很久以前归档的重要线索 archivalneedle".to_string(),
            }])
            .unwrap();

        let fillers: Vec<EvictedTurn> = (0..1200)
            .map(|i| EvictedTurn {
                source_id: format!("filler-{i}"),
                timestamp: "now".to_string(),
                role: "user".to_string(),
                content: format!("无关的填充内容 filler number {i}"),
            })
            .collect();
        store.remember_evicted_turns(&fillers).unwrap();

        let result = store.search_evicted_context("archivalneedle", 5).unwrap();
        let results = result["results"].as_array().unwrap();
        assert!(
            !results.is_empty(),
            "a turn archived before 1200 newer turns must still be retrievable, got {result}"
        );
    }

    #[test]
    fn evicted_context_search_finds_chinese_keywords() {
        let temp = tempfile::tempdir().unwrap();
        let config = AppConfig::default();
        let paths = test_paths(&temp);
        let store = MemoryStore::new(&config, &paths);

        store
            .remember_evicted_turns(&[EvictedTurn {
                source_id: "cjk-turn".to_string(),
                timestamp: "now".to_string(),
                role: "assistant".to_string(),
                content: "这段旧对话讨论了人工智能安全性与对齐问题".to_string(),
            }])
            .unwrap();

        // unicode61 (FTS5's default tokenizer) does not segment CJK text at
        // all, so it would treat the whole run of characters as one token
        // and fail to match a substring like this. The trigram tokenizer
        // has no such requirement since it isn't based on word boundaries.
        let result = store.search_evicted_context("人工智能", 5).unwrap();
        assert!(
            result.to_string().contains("这段旧对话"),
            "Chinese keyword search should retrieve archived Chinese content, got {result}"
        );

        let result2 = store.search_evicted_context("对齐问题", 5).unwrap();
        assert!(!result2["results"].as_array().unwrap().is_empty());
    }

    #[test]
    fn evicted_context_backfills_fts_for_pre_existing_rows_without_index() {
        let temp = tempfile::tempdir().unwrap();
        let config = AppConfig::default();
        let paths = test_paths(&temp);
        let store = MemoryStore::new(&config, &paths);

        // Simulate a DB written by an engine build that predates the FTS
        // index: only the base table exists, populated directly, with no
        // `evicted_turns_fts` virtual table and no sync triggers.
        std::fs::create_dir_all(store.state_db.parent().unwrap()).unwrap();
        {
            let legacy_conn = Connection::open(&store.state_db).unwrap();
            legacy_conn
                .execute_batch(
                    "CREATE TABLE evicted_turns (
                        id INTEGER PRIMARY KEY AUTOINCREMENT,
                        source_id TEXT,
                        timestamp TEXT NOT NULL,
                        role TEXT NOT NULL,
                        content TEXT NOT NULL,
                        created_at TEXT NOT NULL
                    );",
                )
                .unwrap();
            legacy_conn
                .execute(
                    "INSERT INTO evicted_turns (source_id, timestamp, role, content, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        "legacy-turn",
                        "now",
                        "user",
                        "预升级归档内容 preupgrademarker",
                        "now"
                    ],
                )
                .unwrap();
            assert!(!table_exists(&legacy_conn, EVICTED_FTS_TABLE).unwrap());
        }

        // The readonly path (used by the actual tool call) must migrate and
        // backfill on first open, without requiring an explicit `init()`.
        let result = store
            .search_evicted_context_readonly("preupgrademarker", 5)
            .unwrap();
        assert!(
            !result["results"].as_array().unwrap().is_empty(),
            "pre-existing row must be found once the FTS index is backfilled, got {result}"
        );

        let conn = Connection::open(&store.state_db).unwrap();
        assert!(table_exists(&conn, EVICTED_FTS_TABLE).unwrap());
    }

    #[test]
    fn evicted_context_search_results_stay_bounded() {
        let temp = tempfile::tempdir().unwrap();
        let config = AppConfig::default();
        let paths = test_paths(&temp);
        let store = MemoryStore::new(&config, &paths);

        let turns: Vec<EvictedTurn> = (0..200)
            .map(|i| EvictedTurn {
                source_id: format!("bounded-{i}"),
                timestamp: "now".to_string(),
                role: "user".to_string(),
                content: format!("boundedkeyword occurrence number {i}"),
            })
            .collect();
        store.remember_evicted_turns(&turns).unwrap();

        let small_limit = store.search_evicted_context("boundedkeyword", 5).unwrap();
        assert_eq!(small_limit["results"].as_array().unwrap().len(), 5);

        // `max_results` is clamped to 50 regardless of how many rows match
        // and regardless of how the caller requested to be more.
        let large_limit = store.search_evicted_context("boundedkeyword", 500).unwrap();
        assert_eq!(large_limit["results"].as_array().unwrap().len(), 50);
    }
}
