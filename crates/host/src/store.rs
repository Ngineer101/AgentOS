//! SQLite storage. The `events` table is the source of truth and is
//! append-only (enforced by triggers); everything else is derived or config.
//!
//! Secrets never go here: see [`crate::secrets`].

use agenthost_proto::methods::Capability;
use agenthost_proto::*;
use anyhow::{anyhow, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde_json::Value;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use crate::title::TitleSource;
use crate::util::now_ms;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS host (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    host_id TEXT NOT NULL,
    public_key TEXT,
    os TEXT NOT NULL,
    version TEXT NOT NULL,
    secret_backend TEXT,
    created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS devices (
    device_id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    public_key TEXT NOT NULL UNIQUE,
    relay_key TEXT NOT NULL,
    capabilities TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    last_seen_at INTEGER,
    revoked_at INTEGER
);
CREATE TABLE IF NOT EXISTS workspaces (
    workspace_id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    path TEXT NOT NULL,
    kind TEXT NOT NULL,
    network TEXT NOT NULL,
    connectors TEXT NOT NULL DEFAULT '[]',
    created_at INTEGER NOT NULL,
    removed_at INTEGER
);
CREATE UNIQUE INDEX IF NOT EXISTS workspaces_path ON workspaces(path) WHERE removed_at IS NULL;
CREATE TABLE IF NOT EXISTS sessions (
    session_id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces(workspace_id),
    agent TEXT NOT NULL,
    state TEXT NOT NULL,
    state_reason TEXT,
    acp_session_id TEXT,
    title TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    last_seq INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS events (
    session_id TEXT NOT NULL REFERENCES sessions(session_id),
    seq INTEGER NOT NULL,
    type TEXT NOT NULL,
    payload TEXT NOT NULL,
    ts INTEGER NOT NULL,
    PRIMARY KEY (session_id, seq)
) WITHOUT ROWID;
CREATE TRIGGER IF NOT EXISTS events_no_update BEFORE UPDATE ON events
BEGIN SELECT RAISE(ABORT, 'events are append-only'); END;
CREATE TRIGGER IF NOT EXISTS events_no_delete BEFORE DELETE ON events
BEGIN SELECT RAISE(ABORT, 'events are append-only'); END;
CREATE TABLE IF NOT EXISTS prompt_ids (
    session_id TEXT NOT NULL,
    prompt_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    PRIMARY KEY (session_id, prompt_id)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS connectors (
    connector_id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    url TEXT NOT NULL,
    auth TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    status TEXT NOT NULL DEFAULT 'unknown',
    status_detail TEXT,
    read_policy TEXT NOT NULL DEFAULT 'auto',
    write_policy TEXT NOT NULL DEFAULT 'ask',
    tools TEXT NOT NULL DEFAULT '[]',
    oauth_meta TEXT,
    created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS agent_profiles (
    profile_id TEXT PRIMARY KEY,
    agent TEXT NOT NULL,
    name TEXT NOT NULL,
    config_dir TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS agent_profiles_name ON agent_profiles(agent, name COLLATE NOCASE);
CREATE UNIQUE INDEX IF NOT EXISTS agent_profiles_dir ON agent_profiles(config_dir);
CREATE TABLE IF NOT EXISTS push_devices (
    device_id TEXT PRIMARY KEY REFERENCES devices(device_id) ON DELETE CASCADE,
    token TEXT NOT NULL,
    topic TEXT NOT NULL,
    environment TEXT NOT NULL,
    content_key BLOB NOT NULL,
    prefs TEXT NOT NULL,
    registered_at INTEGER NOT NULL,
    last_error TEXT
);
CREATE TABLE IF NOT EXISTS connector_tool_policy (
    connector_id TEXT NOT NULL REFERENCES connectors(connector_id) ON DELETE CASCADE,
    tool TEXT NOT NULL,
    policy TEXT NOT NULL,
    PRIMARY KEY (connector_id, tool)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS automations (
    automation_id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    enabled INTEGER NOT NULL,
    workspace_id TEXT NOT NULL,
    agent TEXT NOT NULL,
    profile_id TEXT,
    prompt TEXT NOT NULL,
    trigger TEXT NOT NULL,
    auto_approve TEXT NOT NULL DEFAULT '[]',
    max_minutes INTEGER NOT NULL,
    hook_id TEXT UNIQUE,
    next_run_at INTEGER,
    last_delivery TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS automation_runs (
    run_id TEXT PRIMARY KEY,
    automation_id TEXT NOT NULL REFERENCES automations(automation_id) ON DELETE CASCADE,
    session_id TEXT,
    trigger TEXT NOT NULL,
    scheduled_for INTEGER,
    started_at INTEGER NOT NULL,
    finished_at INTEGER,
    outcome TEXT NOT NULL,
    summary TEXT,
    detail TEXT,
    dedupe_key TEXT
);
CREATE INDEX IF NOT EXISTS automation_runs_by ON automation_runs(automation_id, started_at);
CREATE TABLE IF NOT EXISTS hook_deliveries (
    delivery_id TEXT PRIMARY KEY,
    received_at INTEGER NOT NULL
) WITHOUT ROWID;
"#;

/// A tool as fetched from the server, before policy is applied.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct RawTool {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub kind: ToolKind,
    /// The tool's full MCP definition (input schema etc.), forwarded to agents.
    #[serde(default)]
    pub definition: Value,
}

#[derive(Debug, Clone)]
pub struct DeviceRecord {
    pub device: Device,
    /// Noise static public key (hex).
    pub public_key: String,
    /// Relay authentication public key (hex).
    pub relay_key: String,
}

pub struct Store {
    conn: Mutex<Connection>,
}

/// A device's APNs registration.
#[derive(Debug, Clone, PartialEq)]
pub struct PushRow {
    pub device_id: String,
    /// The device's name (from `devices`).
    pub name: String,
    pub token: String,
    pub topic: String,
    pub environment: PushEnvironment,
    /// 32 bytes the phone chose: notifications' content is sealed with it.
    pub content_key: Vec<u8>,
    pub prefs: PushPrefs,
    pub registered_at: i64,
    pub last_error: Option<String>,
}

impl PushRow {
    pub fn view(&self) -> PushDevice {
        PushDevice {
            device_id: self.device_id.clone(),
            name: self.name.clone(),
            environment: self.environment,
            prefs: self.prefs,
            registered_at: self.registered_at,
            last_error: self.last_error.clone(),
        }
    }
}

const PUSH_COLS: &str = "p.device_id, d.name, p.token, p.topic, p.environment, p.content_key, p.prefs, p.registered_at, p.last_error";

fn row_push(r: &rusqlite::Row) -> rusqlite::Result<PushRow> {
    let env: String = r.get(4)?;
    let prefs: String = r.get(6)?;
    Ok(PushRow {
        device_id: r.get(0)?,
        name: r.get(1)?,
        token: r.get(2)?,
        topic: r.get(3)?,
        environment: PushEnvironment::parse(&env).unwrap_or(PushEnvironment::Production),
        content_key: r.get(5)?,
        prefs: serde_json::from_str(&prefs).unwrap_or_default(),
        registered_at: r.get(7)?,
        last_error: r.get(8)?,
    })
}

fn enum_str<T: serde::Serialize>(v: &T) -> String {
    match serde_json::to_value(v).expect("enum serializes") {
        Value::String(s) => s,
        other => other.to_string(),
    }
}

fn enum_parse<T: serde::de::DeserializeOwned>(s: &str) -> rusqlite::Result<T> {
    serde_json::from_value(Value::String(s.to_string())).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn json_parse<T: serde::de::DeserializeOwned>(s: &str) -> rusqlite::Result<T> {
    serde_json::from_str(s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn row_session(r: &Row) -> rusqlite::Result<Session> {
    Ok(Session {
        session_id: r.get(0)?,
        workspace_id: r.get(1)?,
        agent: enum_parse(&r.get::<_, String>(2)?)?,
        state: enum_parse(&r.get::<_, String>(3)?)?,
        state_reason: r.get(4)?,
        acp_session_id: r.get(5)?,
        title: r.get(6)?,
        created_at: r.get(7)?,
        updated_at: r.get(8)?,
        last_seq: r.get(9)?,
        profile_id: r.get(10)?,
        archived_at: r.get(11)?,
        worktree: match (r.get::<_, Option<String>>(12)?, r.get::<_, Option<String>>(13)?) {
            (Some(path), Some(branch)) => Some(SessionWorktree { path, branch }),
            _ => None,
        },
        config_options: match r.get::<_, Option<String>>(14)? {
            Some(j) => json_parse(&j)?,
            None => vec![],
        },
        side_questions: r.get::<_, Option<i64>>(15)?.unwrap_or(0) != 0,
        automation_id: r.get(16)?,
        pull_request: match (r.get::<_, Option<i64>>(17)?, r.get::<_, Option<String>>(18)?) {
            (Some(n), Some(url)) => Some(SessionPullRequest { number: n as u64, url }),
            _ => None,
        },
    })
}
const SESSION_COLS: &str = "session_id, workspace_id, agent, state, state_reason, acp_session_id, title, created_at, updated_at, last_seq, profile_id, archived_at, worktree_path, worktree_branch, config_options, side_questions, automation_id, pr_number, pr_url";
/// [`SESSION_COLS`] to read: the title shown is the user's, else the host's.
const SESSION_SELECT: &str = "session_id, workspace_id, agent, state, state_reason, acp_session_id, COALESCE(title, auto_title), created_at, updated_at, last_seq, profile_id, archived_at, worktree_path, worktree_branch, config_options, side_questions, automation_id, pr_number, pr_url";

fn row_profile(r: &Row) -> rusqlite::Result<AgentProfile> {
    Ok(AgentProfile {
        profile_id: r.get(0)?,
        agent: enum_parse(&r.get::<_, String>(1)?)?,
        name: r.get(2)?,
        config_dir: r.get(3)?,
        created_at: r.get(4)?,
    })
}
const PROFILE_COLS: &str = "profile_id, agent, name, config_dir, created_at";

/// Columns added after the first release, for databases created before them.
const ADDED_COLUMNS: &[(&str, &str, &str)] = &[
    ("sessions", "profile_id", "TEXT"),
    ("sessions", "archived_at", "INTEGER"),
    ("workspaces", "default_agent", "TEXT"),
    ("workspaces", "default_profile_id", "TEXT"),
    ("sessions", "worktree_path", "TEXT"),
    ("sessions", "worktree_branch", "TEXT"),
    // The workspace's folder inside the repository ("" at its top), which is
    // where in the worktree the session works.
    ("sessions", "worktree_subdir", "TEXT"),
    ("sessions", "config_options", "TEXT"),
    ("sessions", "side_questions", "INTEGER"),
    ("sessions", "pr_number", "INTEGER"),
    ("sessions", "pr_url", "TEXT"),
    ("connectors", "icon", "TEXT"),
    ("sessions", "automation_id", "TEXT"),
    // The host's title for a session the user hasn't named (`title` is the
    // user's), and where it came from (`title::TitleSource`).
    ("sessions", "auto_title", "TEXT"),
    ("sessions", "auto_title_source", "TEXT"),
];

fn row_workspace(r: &Row) -> rusqlite::Result<Workspace> {
    Ok(Workspace {
        workspace_id: r.get(0)?,
        name: r.get(1)?,
        path: r.get(2)?,
        kind: enum_parse(&r.get::<_, String>(3)?)?,
        network: json_parse(&r.get::<_, String>(4)?)?,
        connectors: json_parse(&r.get::<_, String>(5)?)?,
        created_at: r.get(6)?,
        default_agent: match r.get::<_, Option<String>>(7)? {
            Some(a) => Some(enum_parse(&a)?),
            None => None,
        },
        default_profile_id: r.get(8)?,
    })
}
const WS_COLS: &str = "workspace_id, name, path, kind, network, connectors, created_at, default_agent, default_profile_id";

fn row_event(r: &Row) -> rusqlite::Result<Event> {
    Ok(Event {
        session_id: r.get(0)?,
        seq: r.get(1)?,
        kind: enum_parse(&r.get::<_, String>(2)?)?,
        payload: json_parse(&r.get::<_, String>(3)?)?,
        ts: r.get(4)?,
    })
}

fn row_device(r: &Row) -> rusqlite::Result<DeviceRecord> {
    Ok(DeviceRecord {
        device: Device {
            device_id: r.get(0)?,
            name: r.get(1)?,
            capabilities: json_parse(&r.get::<_, String>(4)?)?,
            created_at: r.get(5)?,
            last_seen_at: r.get(6)?,
            revoked_at: r.get(7)?,
        },
        public_key: r.get(2)?,
        relay_key: r.get(3)?,
    })
}
const DEV_COLS: &str =
    "device_id, name, public_key, relay_key, capabilities, created_at, last_seen_at, revoked_at";

/// A connector row before the per-tool policy is joined in.
#[derive(Debug, Clone)]
pub struct ConnectorRow {
    pub connector_id: String,
    pub name: String,
    pub url: String,
    pub auth: ConnectorAuth,
    pub enabled: bool,
    pub status: String,
    pub status_detail: Option<String>,
    pub read_policy: ToolPolicy,
    pub write_policy: ToolPolicy,
    pub tools: Vec<RawTool>,
    pub oauth_meta: Option<Value>,
    pub created_at: i64,
    /// A `data:` URL, set in the background after the connector is added.
    pub icon: Option<String>,
}

impl ConnectorRow {
    pub fn effective_policy(
        &self,
        tool: &RawTool,
        overrides: &[(String, ToolPolicy)],
    ) -> (ToolPolicy, Option<ToolPolicy>) {
        let ov = overrides
            .iter()
            .find(|(t, _)| *t == tool.name)
            .map(|(_, p)| *p);
        let base = match tool.kind {
            ToolKind::Read => self.read_policy,
            ToolKind::Write => self.write_policy,
        };
        (ov.unwrap_or(base), ov)
    }
}

fn row_connector(r: &Row) -> rusqlite::Result<ConnectorRow> {
    Ok(ConnectorRow {
        connector_id: r.get(0)?,
        name: r.get(1)?,
        url: r.get(2)?,
        auth: enum_parse(&r.get::<_, String>(3)?)?,
        enabled: r.get::<_, i64>(4)? != 0,
        status: r.get(5)?,
        status_detail: r.get(6)?,
        read_policy: enum_parse(&r.get::<_, String>(7)?)?,
        write_policy: enum_parse(&r.get::<_, String>(8)?)?,
        tools: json_parse(&r.get::<_, String>(9)?)?,
        oauth_meta: r
            .get::<_, Option<String>>(10)?
            .map(|s| json_parse(&s))
            .transpose()?,
        created_at: r.get(11)?,
        icon: r.get(12)?,
    })
}
const CONN_COLS: &str = "connector_id, name, url, auth, enabled, status, status_detail, read_policy, write_policy, tools, oauth_meta, created_at, icon";

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
        // Owner-only, like the rest of the data directory.
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(SCHEMA)?;
        for (table, column, ty) in ADDED_COLUMNS {
            let has: bool = conn.query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info(?1) WHERE name = ?2",
                params![table, column],
                |r| r.get(0),
            )?;
            if !has {
                conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {ty}"))?;
            }
        }
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn db(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    // ---- host ----

    /// Return the host id, creating the host row on first start.
    pub fn ensure_host(&self, os: &str, version: &str) -> Result<String> {
        let db = self.db();
        if let Some(id) = db
            .query_row("SELECT host_id FROM host WHERE id = 1", [], |r| {
                r.get::<_, String>(0)
            })
            .optional()?
        {
            db.execute(
                "UPDATE host SET os = ?1, version = ?2 WHERE id = 1",
                params![os, version],
            )?;
            return Ok(id);
        }
        let id = crate::util::new_id("host");
        db.execute(
            "INSERT INTO host (id, host_id, os, version, created_at) VALUES (1, ?1, ?2, ?3, ?4)",
            params![id, os, version, now_ms()],
        )?;
        Ok(id)
    }

    pub fn host_get(&self, field: &str) -> Result<Option<String>> {
        let col = match field {
            "public_key" => "public_key",
            "secret_backend" => "secret_backend",
            _ => return Err(anyhow!("unknown host field {field}")),
        };
        Ok(self
            .db()
            .query_row(&format!("SELECT {col} FROM host WHERE id = 1"), [], |r| {
                r.get(0)
            })
            .optional()?
            .flatten())
    }

    pub fn host_set(&self, field: &str, value: &str) -> Result<()> {
        let col = match field {
            "public_key" => "public_key",
            "secret_backend" => "secret_backend",
            _ => return Err(anyhow!("unknown host field {field}")),
        };
        self.db().execute(
            &format!("UPDATE host SET {col} = ?1 WHERE id = 1"),
            params![value],
        )?;
        Ok(())
    }

    // ---- workspaces ----

    pub fn workspace_insert(&self, w: &Workspace) -> Result<()> {
        self.db().execute(
            &format!("INSERT INTO workspaces ({WS_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"),
            params![
                w.workspace_id,
                w.name,
                w.path,
                enum_str(&w.kind),
                serde_json::to_string(&w.network)?,
                serde_json::to_string(&w.connectors)?,
                w.created_at,
                w.default_agent.as_ref().map(enum_str),
                w.default_profile_id
            ],
        )?;
        Ok(())
    }

    /// Insert a built-in workspace once; bring it back if it was removed.
    pub fn workspace_ensure(&self, w: &Workspace) -> Result<()> {
        let db = self.db();
        db.execute(
            &format!("INSERT OR IGNORE INTO workspaces ({WS_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"),
            params![
                w.workspace_id,
                w.name,
                w.path,
                enum_str(&w.kind),
                serde_json::to_string(&w.network)?,
                serde_json::to_string(&w.connectors)?,
                w.created_at,
                w.default_agent.as_ref().map(enum_str),
                w.default_profile_id
            ],
        )?;
        // The folder follows the data folder if that moved.
        db.execute(
            "UPDATE workspaces SET removed_at = NULL, path = ?2, kind = ?3 WHERE workspace_id = ?1",
            params![w.workspace_id, w.path, enum_str(&w.kind)],
        )?;
        Ok(())
    }

    // ---- settings ----

    pub fn setting_get<T: serde::de::DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let v: Option<String> = self
            .db()
            .query_row("SELECT value FROM settings WHERE key = ?1", params![key], |r| r.get(0))
            .optional()?;
        Ok(match v {
            Some(s) => Some(serde_json::from_str(&s)?),
            None => None,
        })
    }

    pub fn setting_set<T: serde::Serialize>(&self, key: &str, value: &T) -> Result<()> {
        self.db().execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, serde_json::to_string(value)?],
        )?;
        Ok(())
    }

    pub fn workspace_update(&self, w: &Workspace) -> Result<()> {
        self.db().execute(
            "UPDATE workspaces SET name = ?2, network = ?3, connectors = ?4 WHERE workspace_id = ?1",
            params![
                w.workspace_id,
                w.name,
                serde_json::to_string(&w.network)?,
                serde_json::to_string(&w.connectors)?
            ],
        )?;
        Ok(())
    }

    pub fn workspace_remove(&self, id: &str) -> Result<bool> {
        Ok(self.db().execute(
            "UPDATE workspaces SET removed_at = ?2 WHERE workspace_id = ?1 AND removed_at IS NULL",
            params![id, now_ms()],
        )? > 0)
    }

    pub fn workspace_get(&self, id: &str) -> Result<Option<Workspace>> {
        Ok(self
            .db()
            .query_row(
                &format!("SELECT {WS_COLS} FROM workspaces WHERE workspace_id = ?1 AND removed_at IS NULL"),
                params![id],
                row_workspace,
            )
            .optional()?)
    }

    pub fn workspace_by_path(&self, path: &str) -> Result<Option<Workspace>> {
        Ok(self
            .db()
            .query_row(
                &format!("SELECT {WS_COLS} FROM workspaces WHERE path = ?1 AND removed_at IS NULL"),
                params![path],
                row_workspace,
            )
            .optional()?)
    }

    pub fn workspace_list(&self) -> Result<Vec<Workspace>> {
        let db = self.db();
        let mut st = db.prepare(&format!(
            "SELECT {WS_COLS} FROM workspaces WHERE removed_at IS NULL ORDER BY created_at"
        ))?;
        let rows = st
            .query_map([], row_workspace)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    // ---- sessions ----

    pub fn session_insert(&self, s: &Session) -> Result<()> {
        self.db().execute(
            &format!("INSERT INTO sessions ({SESSION_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)"),
            params![
                s.session_id,
                s.workspace_id,
                enum_str(&s.agent),
                enum_str(&s.state),
                s.state_reason,
                s.acp_session_id,
                s.title,
                s.created_at,
                s.updated_at,
                s.last_seq,
                s.profile_id,
                s.archived_at,
                s.worktree.as_ref().map(|w| w.path.clone()),
                s.worktree.as_ref().map(|w| w.branch.clone()),
                serde_json::to_string(&s.config_options)?,
                s.side_questions as i64,
                s.automation_id,
                s.pull_request.as_ref().map(|p| p.number as i64),
                s.pull_request.as_ref().map(|p| p.url.clone())
            ],
        )?;
        Ok(())
    }

    pub fn session_get(&self, id: &str) -> Result<Option<Session>> {
        Ok(self
            .db()
            .query_row(
                &format!("SELECT {SESSION_SELECT} FROM sessions WHERE session_id = ?1"),
                params![id],
                row_session,
            )
            .optional()?)
    }

    pub fn session_set_title(&self, id: &str, title: Option<&str>) -> Result<bool> {
        Ok(self.db().execute(
            "UPDATE sessions SET title = ?2 WHERE session_id = ?1",
            params![id, title],
        )? > 0)
    }

    /// Set the host's own title for a session, unless it has one from a
    /// better source (see [`TitleSource`]), or, from a message, any at all. Returns whether the title shown
    /// changed (it doesn't while the user's own title stands).
    pub fn session_set_auto_title(&self, id: &str, title: &str, source: TitleSource) -> Result<bool> {
        let db = self.db();
        let Some((user, current, from)) = db
            .query_row(
                "SELECT title, auto_title, auto_title_source FROM sessions WHERE session_id = ?1",
                params![id],
                |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, Option<String>>(2)?)),
            )
            .optional()?
        else {
            return Ok(false);
        };
        if from.as_deref().and_then(TitleSource::parse).is_some_and(|f| f > source) {
            return Ok(false);
        }
        // Only the first message names a session, not every one.
        if source == TitleSource::Prompt && current.is_some() {
            return Ok(false);
        }
        if current.as_deref() == Some(title) && from.as_deref() == Some(source.as_str()) {
            return Ok(false);
        }
        db.execute(
            "UPDATE sessions SET auto_title = ?2, auto_title_source = ?3 WHERE session_id = ?1",
            params![id, title, source.as_str()],
        )?;
        Ok(user.is_none() && current.as_deref() != Some(title))
    }

    /// Whether a session still wants a better title than it has: the user
    /// hasn't named it and the host has only its first message (or nothing).
    pub fn session_wants_title(&self, id: &str) -> Result<bool> {
        Ok(self
            .db()
            .query_row(
                "SELECT title IS NULL AND (auto_title_source IS NULL OR auto_title_source = ?2) FROM sessions WHERE session_id = ?1",
                params![id, TitleSource::Prompt.as_str()],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(false))
    }

    /// The text of a session's first message, if it has one.
    pub fn first_prompt_text(&self, id: &str) -> Result<Option<String>> {
        let payload: Option<String> = self
            .db()
            .query_row(
                "SELECT payload FROM events WHERE session_id = ?1 AND type = 'prompt' ORDER BY seq LIMIT 1",
                params![id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(payload
            .and_then(|p| serde_json::from_str::<Value>(&p).ok())
            .and_then(|v| v["text"].as_str().map(str::to_string)))
    }

    /// Title, from its first message, every session that has neither the
    /// user's title nor the host's (those from before the host titled them).
    pub fn backfill_prompt_titles(&self) -> Result<usize> {
        let ids: Vec<String> = {
            let db = self.db();
            let mut st = db.prepare(
                "SELECT session_id FROM sessions WHERE title IS NULL AND auto_title IS NULL",
            )?;
            let rows = st.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let mut n = 0;
        for id in ids {
            let Some(t) = self.first_prompt_text(&id)?.as_deref().and_then(crate::title::from_prompt) else {
                continue;
            };
            self.session_set_auto_title(&id, &t, TitleSource::Prompt)?;
            n += 1;
        }
        Ok(n)
    }

    /// Archive (`Some(time)`) or unarchive (`None`) a session.
    pub fn session_set_archived(&self, id: &str, at: Option<i64>) -> Result<bool> {
        Ok(self.db().execute(
            "UPDATE sessions SET archived_at = ?2 WHERE session_id = ?1",
            params![id, at],
        )? > 0)
    }

    pub fn workspace_set_default_agent(
        &self,
        id: &str,
        agent: Option<AgentKind>,
        profile_id: Option<&str>,
    ) -> Result<()> {
        self.db().execute(
            "UPDATE workspaces SET default_agent = ?2, default_profile_id = ?3 WHERE workspace_id = ?1",
            params![id, agent.as_ref().map(enum_str), profile_id],
        )?;
        Ok(())
    }

    /// Every session (archived ones included), or one workspace's.
    pub fn session_list(&self, workspace_id: Option<&str>) -> Result<Vec<Session>> {
        let db = self.db();
        let mut st = db.prepare(&format!(
            "SELECT {SESSION_SELECT} FROM sessions WHERE (?1 IS NULL OR workspace_id = ?1) ORDER BY created_at"
        ))?;
        let rows = st
            .query_map(params![workspace_id], row_session)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Record the worktree a session works in, and where in it.
    pub fn session_set_worktree(&self, id: &str, wt: &SessionWorktree, subdir: &str) -> Result<()> {
        self.db().execute(
            "UPDATE sessions SET worktree_path = ?2, worktree_branch = ?3, worktree_subdir = ?4 WHERE session_id = ?1",
            params![id, wt.path, wt.branch, subdir],
        )?;
        Ok(())
    }

    /// The workspace's folder inside the repository, relative to the worktree's top.
    pub fn session_worktree_subdir(&self, id: &str) -> Result<String> {
        Ok(self
            .db()
            .query_row(
                "SELECT worktree_subdir FROM sessions WHERE session_id = ?1",
                params![id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten()
            .unwrap_or_default())
    }

    pub fn session_set_config_options(&self, id: &str, options: &[SessionConfigOption]) -> Result<()> {
        self.db().execute(
            "UPDATE sessions SET config_options = ?2 WHERE session_id = ?1",
            params![id, serde_json::to_string(options)?],
        )?;
        Ok(())
    }

    pub fn session_set_side_questions(&self, id: &str, on: bool) -> Result<()> {
        self.db().execute(
            "UPDATE sessions SET side_questions = ?2 WHERE session_id = ?1",
            params![id, on as i64],
        )?;
        Ok(())
    }

    /// Record the pull request a session opened.
    pub fn session_set_pull_request(&self, id: &str, pr: &SessionPullRequest) -> Result<bool> {
        Ok(self.db().execute(
            "UPDATE sessions SET pr_number = ?2, pr_url = ?3 WHERE session_id = ?1",
            params![id, pr.number as i64, pr.url],
        )? > 0)
    }

    pub fn session_set_acp_id(&self, id: &str, acp_id: &str) -> Result<()> {
        self.db().execute(
            "UPDATE sessions SET acp_session_id = ?2 WHERE session_id = ?1",
            params![id, acp_id],
        )?;
        Ok(())
    }

    /// Append one event, assigning the next `seq`. Returns the stored event.
    pub fn append_event(&self, session_id: &str, kind: EventType, payload: Value) -> Result<Event> {
        let mut db = self.db();
        let tx = db.transaction()?;
        let ev = append_in_tx(&tx, session_id, kind, payload)?;
        tx.commit()?;
        Ok(ev)
    }

    /// Change a session's state and record it as a `session.state` event,
    /// atomically. Refuses transitions the state machine does not allow.
    pub fn set_state(
        &self,
        session_id: &str,
        state: SessionState,
        reason: Option<&str>,
        extra: Option<Value>,
    ) -> Result<(Session, Event)> {
        let mut db = self.db();
        let tx = db.transaction()?;
        let current: String = tx
            .query_row(
                "SELECT state FROM sessions WHERE session_id = ?1",
                params![session_id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| anyhow!("no session {session_id}"))?;
        let current: SessionState =
            SessionState::parse(&current).ok_or_else(|| anyhow!("bad state {current}"))?;
        if !current.can_transition_to(state) {
            return Err(anyhow!("invalid transition {current} -> {state}"));
        }
        tx.execute(
            "UPDATE sessions SET state = ?2, state_reason = ?3 WHERE session_id = ?1",
            params![session_id, enum_str(&state), reason],
        )?;
        let mut payload = serde_json::json!({ "from": current, "state": state });
        if let Some(r) = reason {
            payload["reason"] = Value::String(r.to_string());
        }
        if let Some(Value::Object(m)) = extra {
            for (k, v) in m {
                payload[k] = v;
            }
        }
        let ev = append_in_tx(&tx, session_id, EventType::SessionState, payload)?;
        let s = tx.query_row(
            &format!("SELECT {SESSION_SELECT} FROM sessions WHERE session_id = ?1"),
            params![session_id],
            row_session,
        )?;
        tx.commit()?;
        Ok((s, ev))
    }

    /// Record a prompt once per `prompt_id`. Returns the event and whether it is new.
    /// The seq of an already accepted prompt id.
    pub fn prompt_seq(&self, session_id: &str, prompt_id: &str) -> Result<Option<i64>> {
        Ok(self
            .db()
            .query_row(
                "SELECT seq FROM prompt_ids WHERE session_id = ?1 AND prompt_id = ?2",
                params![session_id, prompt_id],
                |r| r.get::<_, i64>(0),
            )
            .optional()?)
    }

    pub fn append_prompt(
        &self,
        session_id: &str,
        prompt_id: &str,
        payload: Value,
    ) -> Result<(Option<Event>, i64)> {
        let mut db = self.db();
        let tx = db.transaction()?;
        if let Some(seq) = tx
            .query_row(
                "SELECT seq FROM prompt_ids WHERE session_id = ?1 AND prompt_id = ?2",
                params![session_id, prompt_id],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
        {
            return Ok((None, seq));
        }
        let ev = append_in_tx(&tx, session_id, EventType::Prompt, payload)?;
        tx.execute(
            "INSERT INTO prompt_ids (session_id, prompt_id, seq) VALUES (?1, ?2, ?3)",
            params![session_id, prompt_id, ev.seq],
        )?;
        tx.commit()?;
        let seq = ev.seq;
        Ok((Some(ev), seq))
    }

    pub fn events_after(
        &self,
        session_id: &str,
        after_seq: i64,
        limit: usize,
    ) -> Result<Vec<Event>> {
        let db = self.db();
        let mut st = db.prepare(
            "SELECT session_id, seq, type, payload, ts FROM events WHERE session_id = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3",
        )?;
        let rows = st
            .query_map(params![session_id, after_seq, limit as i64], row_event)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    // ---- agent profiles ----

    pub fn profile_insert(&self, p: &AgentProfile) -> Result<()> {
        self.db().execute(
            &format!("INSERT INTO agent_profiles ({PROFILE_COLS}) VALUES (?1, ?2, ?3, ?4, ?5)"),
            params![p.profile_id, enum_str(&p.agent), p.name, p.config_dir, p.created_at],
        )?;
        Ok(())
    }

    pub fn profile_get(&self, id: &str) -> Result<Option<AgentProfile>> {
        Ok(self
            .db()
            .query_row(
                &format!("SELECT {PROFILE_COLS} FROM agent_profiles WHERE profile_id = ?1"),
                params![id],
                row_profile,
            )
            .optional()?)
    }

    /// Every profile, by agent then name.
    pub fn profile_list(&self) -> Result<Vec<AgentProfile>> {
        let db = self.db();
        let mut st = db.prepare(&format!(
            "SELECT {PROFILE_COLS} FROM agent_profiles ORDER BY agent, name COLLATE NOCASE"
        ))?;
        let rows = st
            .query_map([], row_profile)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn profile_remove(&self, id: &str) -> Result<bool> {
        Ok(self
            .db()
            .execute("DELETE FROM agent_profiles WHERE profile_id = ?1", params![id])?
            > 0)
    }

    // ---- devices ----

    pub fn device_insert(&self, d: &DeviceRecord) -> Result<()> {
        self.db().execute(
            &format!("INSERT INTO devices ({DEV_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"),
            params![
                d.device.device_id,
                d.device.name,
                d.public_key,
                d.relay_key,
                serde_json::to_string(&d.device.capabilities)?,
                d.device.created_at,
                d.device.last_seen_at,
                d.device.revoked_at
            ],
        )?;
        Ok(())
    }

    pub fn device_list(&self, include_revoked: bool) -> Result<Vec<DeviceRecord>> {
        let db = self.db();
        let mut st = db.prepare(&format!(
            "SELECT {DEV_COLS} FROM devices WHERE (?1 OR revoked_at IS NULL) ORDER BY created_at"
        ))?;
        let rows = st
            .query_map(params![include_revoked], row_device)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn device_get(&self, id: &str) -> Result<Option<DeviceRecord>> {
        Ok(self
            .db()
            .query_row(
                &format!("SELECT {DEV_COLS} FROM devices WHERE device_id = ?1"),
                params![id],
                row_device,
            )
            .optional()?)
    }

    pub fn device_by_key(&self, public_key_hex: &str) -> Result<Option<DeviceRecord>> {
        Ok(self
            .db()
            .query_row(
                &format!(
                    "SELECT {DEV_COLS} FROM devices WHERE public_key = ?1 AND revoked_at IS NULL"
                ),
                params![public_key_hex],
                row_device,
            )
            .optional()?)
    }

    pub fn device_rename(&self, id: &str, name: &str) -> Result<bool> {
        Ok(self.db().execute(
            "UPDATE devices SET name = ?2 WHERE device_id = ?1",
            params![id, name],
        )? > 0)
    }

    pub fn device_seen(&self, id: &str) -> Result<()> {
        self.db().execute(
            "UPDATE devices SET last_seen_at = ?2 WHERE device_id = ?1",
            params![id, now_ms()],
        )?;
        Ok(())
    }

    /// Revoke: the key is deleted from the host (replaced by an unusable
    /// marker so the row keeps its history) and the device can never
    /// complete a handshake again.
    /// Its push registration goes too.
    pub fn device_revoke(&self, id: &str) -> Result<bool> {
        let db = self.db();
        let n = db.execute(
            "UPDATE devices SET revoked_at = ?2, public_key = 'revoked:' || device_id, relay_key = 'revoked:' || device_id WHERE device_id = ?1 AND revoked_at IS NULL",
            params![id, now_ms()],
        )?;
        db.execute("DELETE FROM push_devices WHERE device_id = ?1", params![id])?;
        Ok(n > 0)
    }

    // ---- push registrations ----

    /// Register (or replace) a device's APNs token.
    pub fn push_register(
        &self,
        device_id: &str,
        token: &str,
        topic: &str,
        environment: PushEnvironment,
        content_key: &[u8],
        prefs: &PushPrefs,
    ) -> Result<()> {
        self.db().execute(
            "INSERT INTO push_devices (device_id, token, topic, environment, content_key, prefs, registered_at, last_error)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL)
             ON CONFLICT(device_id) DO UPDATE SET token = excluded.token, topic = excluded.topic,
               environment = excluded.environment, content_key = excluded.content_key,
               prefs = excluded.prefs, registered_at = excluded.registered_at, last_error = NULL",
            params![
                device_id,
                token,
                topic,
                environment.as_str(),
                content_key,
                serde_json::to_string(prefs)?,
                now_ms()
            ],
        )?;
        Ok(())
    }

    pub fn push_unregister(&self, device_id: &str) -> Result<bool> {
        Ok(self
            .db()
            .execute("DELETE FROM push_devices WHERE device_id = ?1", params![device_id])?
            > 0)
    }

    pub fn push_set_prefs(&self, device_id: &str, prefs: &PushPrefs) -> Result<bool> {
        Ok(self.db().execute(
            "UPDATE push_devices SET prefs = ?2 WHERE device_id = ?1",
            params![device_id, serde_json::to_string(prefs)?],
        )? > 0)
    }

    /// APNs answered with a token from the other environment (an app built
    /// differently than it thought): remember the one that works.
    pub fn push_set_environment(&self, device_id: &str, env: PushEnvironment) -> Result<()> {
        self.db().execute(
            "UPDATE push_devices SET environment = ?2 WHERE device_id = ?1",
            params![device_id, env.as_str()],
        )?;
        Ok(())
    }

    pub fn push_set_error(&self, device_id: &str, error: Option<&str>) -> Result<()> {
        self.db().execute(
            "UPDATE push_devices SET last_error = ?2 WHERE device_id = ?1",
            params![device_id, error],
        )?;
        Ok(())
    }

    pub fn push_get(&self, device_id: &str) -> Result<Option<PushRow>> {
        Ok(self
            .db()
            .query_row(
                &format!("SELECT {PUSH_COLS} FROM push_devices p JOIN devices d ON d.device_id = p.device_id WHERE p.device_id = ?1 AND d.revoked_at IS NULL"),
                params![device_id],
                row_push,
            )
            .optional()?)
    }

    /// Registrations of devices that are still paired.
    pub fn push_list(&self) -> Result<Vec<PushRow>> {
        let db = self.db();
        let mut st = db.prepare(&format!(
            "SELECT {PUSH_COLS} FROM push_devices p JOIN devices d ON d.device_id = p.device_id WHERE d.revoked_at IS NULL ORDER BY p.registered_at"
        ))?;
        let rows = st
            .query_map([], row_push)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    // ---- connectors ----

    pub fn connector_insert(&self, c: &ConnectorRow) -> Result<()> {
        self.db().execute(
            &format!("INSERT INTO connectors ({CONN_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)"),
            params![
                c.connector_id,
                c.name,
                c.url,
                enum_str(&c.auth),
                c.enabled as i64,
                c.status,
                c.status_detail,
                enum_str(&c.read_policy),
                enum_str(&c.write_policy),
                serde_json::to_string(&c.tools)?,
                c.oauth_meta.as_ref().map(|v| v.to_string()),
                c.created_at,
                c.icon
            ],
        )?;
        Ok(())
    }

    pub fn connector_save(&self, c: &ConnectorRow) -> Result<()> {
        self.db().execute(
            "UPDATE connectors SET name = ?2, url = ?3, auth = ?4, enabled = ?5, status = ?6, status_detail = ?7, read_policy = ?8, write_policy = ?9, tools = ?10, oauth_meta = ?11 WHERE connector_id = ?1",
            params![
                c.connector_id,
                c.name,
                c.url,
                enum_str(&c.auth),
                c.enabled as i64,
                c.status,
                c.status_detail,
                enum_str(&c.read_policy),
                enum_str(&c.write_policy),
                serde_json::to_string(&c.tools)?,
                c.oauth_meta.as_ref().map(|v| v.to_string())
            ],
        )?;
        Ok(())
    }

    /// Store a connector's icon (a `data:` URL). False if it was removed meanwhile.
    pub fn connector_set_icon(&self, id: &str, icon: &str) -> Result<bool> {
        Ok(self.db().execute(
            "UPDATE connectors SET icon = ?2 WHERE connector_id = ?1",
            params![id, icon],
        )? > 0)
    }

    pub fn connector_get(&self, id: &str) -> Result<Option<ConnectorRow>> {
        Ok(self
            .db()
            .query_row(
                &format!("SELECT {CONN_COLS} FROM connectors WHERE connector_id = ?1"),
                params![id],
                row_connector,
            )
            .optional()?)
    }

    pub fn connector_list(&self) -> Result<Vec<ConnectorRow>> {
        let db = self.db();
        let mut st = db.prepare(&format!(
            "SELECT {CONN_COLS} FROM connectors ORDER BY created_at"
        ))?;
        let rows = st
            .query_map([], row_connector)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn connector_remove(&self, id: &str) -> Result<bool> {
        Ok(self.db().execute(
            "DELETE FROM connectors WHERE connector_id = ?1",
            params![id],
        )? > 0)
    }

    pub fn tool_policies(&self, connector_id: &str) -> Result<Vec<(String, ToolPolicy)>> {
        let db = self.db();
        let mut st =
            db.prepare("SELECT tool, policy FROM connector_tool_policy WHERE connector_id = ?1")?;
        let rows = st
            .query_map(params![connector_id], |r| {
                Ok((r.get::<_, String>(0)?, enum_parse(&r.get::<_, String>(1)?)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn set_tool_policy(
        &self,
        connector_id: &str,
        tool: &str,
        policy: Option<ToolPolicy>,
    ) -> Result<()> {
        let db = self.db();
        match policy {
            Some(p) => db.execute(
                "INSERT INTO connector_tool_policy (connector_id, tool, policy) VALUES (?1, ?2, ?3)
                 ON CONFLICT(connector_id, tool) DO UPDATE SET policy = excluded.policy",
                params![connector_id, tool, enum_str(&p)],
            )?,
            None => db.execute(
                "DELETE FROM connector_tool_policy WHERE connector_id = ?1 AND tool = ?2",
                params![connector_id, tool],
            )?,
        };
        Ok(())
    }

    /// The connector as clients see it, with effective tool policy.
    pub fn connector_view(&self, c: &ConnectorRow) -> Result<Connector> {
        let overrides = self.tool_policies(&c.connector_id)?;
        let tools = c
            .tools
            .iter()
            .map(|t| {
                let (policy, policy_override) = c.effective_policy(t, &overrides);
                ConnectorTool {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    kind: t.kind,
                    policy,
                    policy_override,
                }
            })
            .collect();
        Ok(Connector {
            connector_id: c.connector_id.clone(),
            name: c.name.clone(),
            url: c.url.clone(),
            auth: c.auth,
            enabled: c.enabled,
            status: c.status.clone(),
            status_detail: c.status_detail.clone(),
            write_policy: c.write_policy,
            read_policy: c.read_policy,
            tools,
            created_at: c.created_at,
            icon: c.icon.clone(),
        })
    }
}

fn append_in_tx(
    tx: &rusqlite::Transaction,
    session_id: &str,
    kind: EventType,
    payload: Value,
) -> Result<Event> {
    let last: i64 = tx
        .query_row(
            "SELECT last_seq FROM sessions WHERE session_id = ?1",
            params![session_id],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(|| anyhow!("no session {session_id}"))?;
    let seq = last + 1;
    let ts = now_ms();
    tx.execute(
        "INSERT INTO events (session_id, seq, type, payload, ts) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![session_id, seq, kind.as_str(), payload.to_string(), ts],
    )?;
    tx.execute(
        "UPDATE sessions SET last_seq = ?2, updated_at = ?3 WHERE session_id = ?1",
        params![session_id, seq, ts],
    )?;
    Ok(Event {
        session_id: session_id.to_string(),
        seq,
        kind,
        payload,
        ts,
    })
}

/// Capabilities as stored for a device.
pub fn caps_to_json(c: &[Capability]) -> String {
    serde_json::to_string(c).unwrap_or_else(|_| "[]".into())
}

/// An automation as stored, before the host adds what it derives (schedule
/// text, last run, webhook URL).
#[derive(Debug, Clone, PartialEq)]
pub struct AutomationRow {
    pub automation: Automation,
    pub hook_id: Option<String>,
    pub last_delivery: Option<WebhookDelivery>,
}

const AUTO_COLS: &str = "automation_id, name, enabled, workspace_id, agent, profile_id, prompt, trigger, auto_approve, max_minutes, hook_id, next_run_at, last_delivery, created_at, updated_at";

fn row_automation(r: &Row) -> rusqlite::Result<AutomationRow> {
    Ok(AutomationRow {
        automation: Automation {
            automation_id: r.get(0)?,
            name: r.get(1)?,
            enabled: r.get::<_, i64>(2)? != 0,
            workspace_id: r.get(3)?,
            agent: enum_parse(&r.get::<_, String>(4)?)?,
            profile_id: r.get(5)?,
            prompt: r.get(6)?,
            trigger: json_parse(&r.get::<_, String>(7)?)?,
            auto_approve: json_parse(&r.get::<_, String>(8)?)?,
            max_minutes: r.get(9)?,
            next_run_at: r.get(11)?,
            created_at: r.get(13)?,
            updated_at: r.get(14)?,
            schedule_text: None,
            last_run: None,
            webhook: None,
        },
        hook_id: r.get(10)?,
        last_delivery: r.get::<_, Option<String>>(12)?.map(|s| json_parse(&s)).transpose()?,
    })
}

const RUN_COLS: &str = "run_id, automation_id, session_id, trigger, scheduled_for, started_at, finished_at, outcome, summary, detail";

fn row_run(r: &Row) -> rusqlite::Result<AutomationRun> {
    Ok(AutomationRun {
        run_id: r.get(0)?,
        automation_id: r.get(1)?,
        session_id: r.get(2)?,
        trigger: enum_parse(&r.get::<_, String>(3)?)?,
        scheduled_for: r.get(4)?,
        started_at: r.get(5)?,
        finished_at: r.get(6)?,
        outcome: enum_parse(&r.get::<_, String>(7)?)?,
        summary: r.get(8)?,
        detail: r.get(9)?,
    })
}

impl Store {
    // ---- automations ----

    pub fn automation_save(&self, row: &AutomationRow) -> Result<()> {
        let a = &row.automation;
        self.db().execute(
            &format!(
                "INSERT INTO automations ({AUTO_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
                 ON CONFLICT(automation_id) DO UPDATE SET name = excluded.name, enabled = excluded.enabled,
                 workspace_id = excluded.workspace_id, agent = excluded.agent, profile_id = excluded.profile_id,
                 prompt = excluded.prompt, trigger = excluded.trigger, auto_approve = excluded.auto_approve,
                 max_minutes = excluded.max_minutes, hook_id = excluded.hook_id, next_run_at = excluded.next_run_at,
                 last_delivery = excluded.last_delivery, updated_at = excluded.updated_at"
            ),
            params![
                a.automation_id,
                a.name,
                a.enabled as i64,
                a.workspace_id,
                enum_str(&a.agent),
                a.profile_id,
                a.prompt,
                serde_json::to_string(&a.trigger)?,
                serde_json::to_string(&a.auto_approve)?,
                a.max_minutes,
                row.hook_id,
                a.next_run_at,
                row.last_delivery.as_ref().map(serde_json::to_string).transpose()?,
                a.created_at,
                a.updated_at
            ],
        )?;
        Ok(())
    }

    pub fn automation_get(&self, id: &str) -> Result<Option<AutomationRow>> {
        Ok(self
            .db()
            .query_row(&format!("SELECT {AUTO_COLS} FROM automations WHERE automation_id = ?1"), params![id], row_automation)
            .optional()?)
    }

    pub fn automation_by_hook(&self, hook_id: &str) -> Result<Option<AutomationRow>> {
        Ok(self
            .db()
            .query_row(&format!("SELECT {AUTO_COLS} FROM automations WHERE hook_id = ?1"), params![hook_id], row_automation)
            .optional()?)
    }

    pub fn automation_list(&self) -> Result<Vec<AutomationRow>> {
        let db = self.db();
        let mut st = db.prepare(&format!("SELECT {AUTO_COLS} FROM automations ORDER BY created_at"))?;
        let rows = st.query_map([], row_automation)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn automation_set_next_run(&self, id: &str, at: Option<i64>) -> Result<()> {
        self.db().execute("UPDATE automations SET next_run_at = ?2 WHERE automation_id = ?1", params![id, at])?;
        Ok(())
    }

    pub fn automation_set_last_delivery(&self, id: &str, d: &WebhookDelivery) -> Result<()> {
        self.db().execute(
            "UPDATE automations SET last_delivery = ?2 WHERE automation_id = ?1",
            params![id, serde_json::to_string(d)?],
        )?;
        Ok(())
    }

    /// Delete an automation and its run history (its sessions stay).
    pub fn automation_remove(&self, id: &str) -> Result<bool> {
        Ok(self.db().execute("DELETE FROM automations WHERE automation_id = ?1", params![id])? > 0)
    }

    pub fn session_set_automation(&self, session_id: &str, automation_id: &str) -> Result<()> {
        self.db().execute(
            "UPDATE sessions SET automation_id = ?2 WHERE session_id = ?1",
            params![session_id, automation_id],
        )?;
        Ok(())
    }

    // ---- automation runs ----

    pub fn run_insert(&self, r: &AutomationRun, dedupe_key: Option<&str>) -> Result<()> {
        self.db().execute(
            &format!("INSERT INTO automation_runs ({RUN_COLS}, dedupe_key) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)"),
            params![
                r.run_id,
                r.automation_id,
                r.session_id,
                enum_str(&r.trigger),
                r.scheduled_for,
                r.started_at,
                r.finished_at,
                enum_str(&r.outcome),
                r.summary,
                r.detail,
                dedupe_key
            ],
        )?;
        Ok(())
    }

    pub fn run_set_session(&self, run_id: &str, session_id: &str) -> Result<()> {
        self.db().execute("UPDATE automation_runs SET session_id = ?2 WHERE run_id = ?1", params![run_id, session_id])?;
        Ok(())
    }

    /// Record how a run ended. Only a running run changes.
    pub fn run_finish(&self, run_id: &str, outcome: RunOutcome, summary: Option<&str>, detail: Option<&str>) -> Result<bool> {
        Ok(self.db().execute(
            "UPDATE automation_runs SET outcome = ?2, finished_at = ?3, summary = COALESCE(?4, summary), detail = COALESCE(?5, detail)
             WHERE run_id = ?1 AND outcome = 'running'",
            params![run_id, enum_str(&outcome), now_ms(), summary, detail],
        )? > 0)
    }

    pub fn run_get(&self, run_id: &str) -> Result<Option<AutomationRun>> {
        Ok(self
            .db()
            .query_row(&format!("SELECT {RUN_COLS} FROM automation_runs WHERE run_id = ?1"), params![run_id], row_run)
            .optional()?)
    }

    /// Newest first.
    pub fn run_list(&self, automation_id: &str, before: Option<i64>, limit: usize) -> Result<Vec<AutomationRun>> {
        let db = self.db();
        let mut st = db.prepare(&format!(
            "SELECT {RUN_COLS} FROM automation_runs WHERE automation_id = ?1 AND started_at < ?2
             ORDER BY started_at DESC, rowid DESC LIMIT ?3"
        ))?;
        let rows = st
            .query_map(params![automation_id, before.unwrap_or(i64::MAX), limit as i64], row_run)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn runs_running(&self) -> Result<Vec<AutomationRun>> {
        let db = self.db();
        let mut st = db.prepare(&format!("SELECT {RUN_COLS} FROM automation_runs WHERE outcome = 'running'"))?;
        let rows = st.query_map([], row_run)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Whether a run with this dedupe key started at or after `since`.
    pub fn run_dedupe_hit(&self, automation_id: &str, key: &str, since: i64) -> Result<bool> {
        Ok(self.db().query_row(
            "SELECT COUNT(*) > 0 FROM automation_runs WHERE automation_id = ?1 AND dedupe_key = ?2 AND started_at >= ?3 AND outcome != 'skipped'",
            params![automation_id, key, since],
            |r| r.get(0),
        )?)
    }

    /// Remember a webhook delivery id; false if it was seen already. Ids
    /// older than a week are forgotten (the relay keeps deliveries a day).
    pub fn hook_delivery_first(&self, delivery_id: &str) -> Result<bool> {
        let db = self.db();
        let now = now_ms();
        db.execute("DELETE FROM hook_deliveries WHERE received_at < ?1", params![now - 7 * 86_400_000])?;
        Ok(db.execute(
            "INSERT OR IGNORE INTO hook_deliveries (delivery_id, received_at) VALUES (?1, ?2)",
            params![delivery_id, now],
        )? > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn setup() -> (Store, String) {
        let st = Store::open_in_memory().unwrap();
        st.ensure_host("linux", "0").unwrap();
        let w = Workspace {
            workspace_id: "ws_1".into(),
            name: "w".into(),
            path: "/tmp/w".into(),
            kind: WorkspaceKind::Folder,
            network: NetworkProfile::default(),
            connectors: vec![],
            created_at: 0,
            default_agent: None,
            default_profile_id: None,
        };
        st.workspace_insert(&w).unwrap();
        let s = Session {
            session_id: "ses_1".into(),
            workspace_id: "ws_1".into(),
            agent: AgentKind::Fake,
            profile_id: None,
            archived_at: None,
            worktree: None,
            config_options: vec![],
            side_questions: false,
            pull_request: None,
            automation_id: None,
            state: SessionState::Created,
            state_reason: None,
            acp_session_id: None,
            title: None,
            created_at: 0,
            updated_at: 0,
            last_seq: 0,
        };
        st.session_insert(&s).unwrap();
        (st, "ses_1".into())
    }

    #[test]
    fn old_databases_gain_the_profile_column() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.db");
        {
            // A sessions table from before agent profiles.
            let c = Connection::open(&path).unwrap();
            c.execute_batch(
                "CREATE TABLE sessions (session_id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, agent TEXT NOT NULL, state TEXT NOT NULL, state_reason TEXT, acp_session_id TEXT, title TEXT, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, last_seq INTEGER NOT NULL DEFAULT 0);
                 INSERT INTO sessions VALUES ('ses_old', 'ws_1', 'claude', 'ended', NULL, NULL, NULL, 1, 2, 0);",
            )
            .unwrap();
        }
        let st = Store::open(&path).unwrap();
        let s = st.session_get("ses_old").unwrap().unwrap();
        assert_eq!((s.agent, s.profile_id), (AgentKind::Claude, None));
        // And opening again is a no-op.
        drop(st);
        Store::open(&path).unwrap();
    }

    #[test]
    fn a_session_keeps_its_pull_request() {
        let (st, sid) = setup();
        assert_eq!(st.session_get(&sid).unwrap().unwrap().pull_request, None);
        let pr = SessionPullRequest { number: 48483, url: "https://github.com/o/r/pull/48483".into() };
        assert!(st.session_set_pull_request(&sid, &pr).unwrap());
        assert_eq!(st.session_get(&sid).unwrap().unwrap().pull_request, Some(pr));
        assert!(!st.session_set_pull_request("ses_none", &SessionPullRequest { number: 1, url: "u".into() }).unwrap());
    }

    #[test]
    fn the_users_title_beats_the_hosts_and_better_sources_win() {
        let (st, sid) = setup();
        let title = |st: &Store| st.session_get(&sid).unwrap().unwrap().title;
        assert!(st.session_wants_title(&sid).unwrap());
        assert!(st.session_set_auto_title(&sid, "Fix the bug", TitleSource::Prompt).unwrap());
        assert_eq!(title(&st).as_deref(), Some("Fix the bug"));
        assert!(st.session_wants_title(&sid).unwrap(), "a first message is not a good title");
        assert!(!st.session_set_auto_title(&sid, "Second message", TitleSource::Prompt).unwrap(), "only the first");
        assert!(st.session_set_auto_title(&sid, "Flaky resume test", TitleSource::Asked).unwrap());
        assert!(!st.session_wants_title(&sid).unwrap());
        assert!(!st.session_set_auto_title(&sid, "Later message", TitleSource::Prompt).unwrap());
        assert!(st.session_set_auto_title(&sid, "Agent's name", TitleSource::Agent).unwrap());
        assert!(!st.session_set_auto_title(&sid, "Asked again", TitleSource::Asked).unwrap());
        assert!(st.session_set_auto_title(&sid, "Agent renamed it", TitleSource::Agent).unwrap());
        assert_eq!(title(&st).as_deref(), Some("Agent renamed it"));
        // The user's title shows over it, and clearing it brings it back.
        st.session_set_title(&sid, Some("Mine")).unwrap();
        assert_eq!(title(&st).as_deref(), Some("Mine"));
        assert!(!st.session_set_auto_title(&sid, "Agent again", TitleSource::Agent).unwrap(), "not shown");
        assert_eq!(title(&st).as_deref(), Some("Mine"));
        st.session_set_title(&sid, None).unwrap();
        assert_eq!(title(&st).as_deref(), Some("Agent again"));
        assert_eq!(st.session_list(None).unwrap()[0].title.as_deref(), Some("Agent again"));
    }

    #[test]
    fn older_sessions_are_titled_from_their_first_message() {
        let (st, sid) = setup();
        assert_eq!(st.backfill_prompt_titles().unwrap(), 0, "no messages yet");
        st.append_prompt(&sid, "p1", json!({"text": "make the tests pass. then push"})).unwrap();
        st.append_prompt(&sid, "p2", json!({"text": "and the docs"})).unwrap();
        assert_eq!(st.first_prompt_text(&sid).unwrap().as_deref(), Some("make the tests pass. then push"));
        assert_eq!(st.backfill_prompt_titles().unwrap(), 1);
        assert_eq!(st.session_get(&sid).unwrap().unwrap().title.as_deref(), Some("Make the tests pass"));
        assert_eq!(st.backfill_prompt_titles().unwrap(), 0, "once");
    }

    #[test]
    fn events_are_ordered_and_append_only() {
        let (st, sid) = setup();
        let a = st
            .append_event(&sid, EventType::AgentMessage, json!({"text":"a"}))
            .unwrap();
        let b = st
            .append_event(&sid, EventType::AgentMessage, json!({"text":"b"}))
            .unwrap();
        assert_eq!((a.seq, b.seq), (1, 2));
        let all = st.events_after(&sid, 0, 100).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(
            st.events_after(&sid, 1, 100).unwrap()[0].payload["text"],
            "b"
        );
        let db = st.db();
        assert!(db.execute("UPDATE events SET payload = '{}'", []).is_err());
        assert!(db.execute("DELETE FROM events", []).is_err());
    }

    #[test]
    fn state_transitions_are_checked_and_logged() {
        let (st, sid) = setup();
        assert!(st
            .set_state(&sid, SessionState::Running, None, None)
            .is_err());
        st.set_state(&sid, SessionState::Starting, None, None)
            .unwrap();
        let (s, ev) = st
            .set_state(&sid, SessionState::Failed, Some("boom"), None)
            .unwrap();
        assert_eq!(s.state, SessionState::Failed);
        assert_eq!(ev.payload["reason"], "boom");
        assert!(st.set_state(&sid, SessionState::Idle, None, None).is_err());
    }

    #[test]
    fn prompts_are_idempotent() {
        let (st, sid) = setup();
        let (e1, s1) = st.append_prompt(&sid, "p1", json!({"text":"hi"})).unwrap();
        let (e2, s2) = st.append_prompt(&sid, "p1", json!({"text":"hi"})).unwrap();
        assert!(e1.is_some() && e2.is_none());
        assert_eq!(s1, s2);
        assert_eq!(st.events_after(&sid, 0, 10).unwrap().len(), 1);
    }
}
