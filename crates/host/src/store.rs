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
CREATE TABLE IF NOT EXISTS connector_tool_policy (
    connector_id TEXT NOT NULL REFERENCES connectors(connector_id) ON DELETE CASCADE,
    tool TEXT NOT NULL,
    policy TEXT NOT NULL,
    PRIMARY KEY (connector_id, tool)
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
    })
}
const SESSION_COLS: &str = "session_id, workspace_id, agent, state, state_reason, acp_session_id, title, created_at, updated_at, last_seq";

fn row_workspace(r: &Row) -> rusqlite::Result<Workspace> {
    Ok(Workspace {
        workspace_id: r.get(0)?,
        name: r.get(1)?,
        path: r.get(2)?,
        kind: enum_parse(&r.get::<_, String>(3)?)?,
        network: json_parse(&r.get::<_, String>(4)?)?,
        connectors: json_parse(&r.get::<_, String>(5)?)?,
        created_at: r.get(6)?,
    })
}
const WS_COLS: &str = "workspace_id, name, path, kind, network, connectors, created_at";

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
    })
}
const CONN_COLS: &str = "connector_id, name, url, auth, enabled, status, status_detail, read_policy, write_policy, tools, oauth_meta, created_at";

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
            &format!("INSERT INTO workspaces ({WS_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"),
            params![
                w.workspace_id,
                w.name,
                w.path,
                enum_str(&w.kind),
                serde_json::to_string(&w.network)?,
                serde_json::to_string(&w.connectors)?,
                w.created_at
            ],
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
            &format!("INSERT INTO sessions ({SESSION_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"),
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
                s.last_seq
            ],
        )?;
        Ok(())
    }

    pub fn session_get(&self, id: &str) -> Result<Option<Session>> {
        Ok(self
            .db()
            .query_row(
                &format!("SELECT {SESSION_COLS} FROM sessions WHERE session_id = ?1"),
                params![id],
                row_session,
            )
            .optional()?)
    }

    pub fn session_list(&self, workspace_id: Option<&str>) -> Result<Vec<Session>> {
        let db = self.db();
        let mut st = db.prepare(&format!(
            "SELECT {SESSION_COLS} FROM sessions WHERE (?1 IS NULL OR workspace_id = ?1) ORDER BY created_at"
        ))?;
        let rows = st
            .query_map(params![workspace_id], row_session)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
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
            &format!("SELECT {SESSION_COLS} FROM sessions WHERE session_id = ?1"),
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
    pub fn device_revoke(&self, id: &str) -> Result<bool> {
        Ok(self.db().execute(
            "UPDATE devices SET revoked_at = ?2, public_key = 'revoked:' || device_id, relay_key = 'revoked:' || device_id WHERE device_id = ?1 AND revoked_at IS NULL",
            params![id, now_ms()],
        )? > 0)
    }

    // ---- connectors ----

    pub fn connector_insert(&self, c: &ConnectorRow) -> Result<()> {
        self.db().execute(
            &format!("INSERT INTO connectors ({CONN_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)"),
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
                c.created_at
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
        };
        st.workspace_insert(&w).unwrap();
        let s = Session {
            session_id: "ses_1".into(),
            workspace_id: "ws_1".into(),
            agent: AgentKind::Fake,
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
