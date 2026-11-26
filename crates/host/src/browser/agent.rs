//! What agents do with the browser, through the gateway's
//! `agenthost__browser_*` tools. Each session works in a tab of its own in
//! its workspace's browser (plus the popups that tab opens); acting on the
//! page answers with what the page shows afterwards.

use super::{keys, record, snapshot, Browsers, Instance, Recording};
use agenthost_proto::*;
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long an action waits for a page it started loading.
const LOAD_WAIT: Duration = Duration::from_secs(15);
/// Most characters of an evaluated result.
const EVAL_LIMIT: usize = 20_000;
/// The longest `browser_wait_for` waits.
const MAX_WAIT_SECS: f64 = 30.0;

/// A tool's answer: text, and maybe a picture (base64 JPEG).
#[derive(Debug, Default)]
pub struct AgentOut {
    pub text: String,
    pub image: Option<String>,
}

impl AgentOut {
    fn text(t: impl Into<String>) -> AgentOut {
        AgentOut { text: t.into(), image: None }
    }
}

/// Why a navigation failed, in words an agent can act on.
fn explain_net_error(err: &str, url: &str) -> String {
    let host = url.split("://").nth(1).unwrap_or(url).split(['/', '?', '#']).next().unwrap_or("");
    let bare = host.rsplit_once(':').map_or(host, |(h, _)| h);
    let local = bare == "localhost" || bare.ends_with(".localhost") || bare.starts_with("127.") || bare == "[::1]" || bare == "0.0.0.0";
    if local {
        return format!(
            "{url} did not open ({err}). The browser runs on the user's computer, outside your sandbox, and can't reach addresses on it \
             (localhost) yet, so a server you started in your sandbox is out of its reach. Test a deployed address instead, or ask the user."
        );
    }
    if err.contains("TUNNEL_CONNECTION_FAILED") || err.contains("PROXY") {
        return format!(
            "{url} did not open: this workspace's network settings don't allow {host}. Ask the user to add it to the workspace's allowed hosts \
             (Settings → Workspaces → Network) if it should be reachable."
        );
    }
    format!("{url} did not open: {err}")
}

impl Browsers {
    /// The session's tab, opened in its workspace's browser if it has none.
    async fn session_tab(&self, session_id: &str, workspace_id: &str) -> Result<(Arc<Instance>, String)> {
        let inst = self.ensure(workspace_id).await.map_err(|e| anyhow!("{}", e.message))?;
        let known = self.session_tabs.lock().unwrap().get(session_id).cloned();
        if let Some(id) = known {
            if inst.tab_session(&id).is_some() {
                inst.touch();
                mark_used(&inst, &id);
                return Ok((inst, id));
            }
        }
        let (w, h) = BROWSER_DEFAULT_SIZE;
        let id = self.new_tab(&inst, Some(session_id), w, h).await?;
        self.session_tabs.lock().unwrap().insert(session_id.to_string(), id.clone());
        // Its tab closed while recording: the recording carries on here.
        self.follow_recording(&inst, session_id, &id).await;
        Ok((inst, id))
    }

    /// Wait for what an action set off: a page load if one starts soon,
    /// else a moment for the page to react.
    async fn settle(&self, inst: &Instance, tab_id: &str, loads_before: u64) {
        tokio::time::sleep(Duration::from_millis(150)).await;
        let Some(mut rx) = inst.st.lock().unwrap().tabs.iter().find(|t| t.id == tab_id).map(|t| t.loads.subscribe()) else { return };
        let loading = || inst.st.lock().unwrap().tabs.iter().find(|t| t.id == tab_id).is_some_and(|t| t.loading);
        if *rx.borrow() == loads_before && !loading() {
            tokio::time::sleep(Duration::from_millis(250)).await;
            if !loading() {
                return;
            }
        }
        let deadline = Instant::now() + LOAD_WAIT;
        while loading() && Instant::now() < deadline {
            let _ = tokio::time::timeout(Duration::from_millis(500), rx.changed()).await;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    fn loads(&self, inst: &Instance, tab_id: &str) -> u64 {
        inst.st.lock().unwrap().tabs.iter().find(|t| t.id == tab_id).map(|t| *t.loads.borrow()).unwrap_or(0)
    }

    /// The page as text: title, address and outline, and a word about new
    /// console errors.
    async fn describe(&self, inst: &Instance, tab_id: &str) -> Result<String> {
        // The title and address as they are now: change events may still be on their way.
        if let Ok(info) = inst.cdp.call("Target.getTargetInfo", json!({"targetId": tab_id}), None).await {
            let t = &info["targetInfo"];
            let mut st = inst.st.lock().unwrap();
            if let Some(tab) = st.tabs.iter_mut().find(|x| x.id == tab_id) {
                if let (Some(url), Some(title)) = (t["url"].as_str(), t["title"].as_str()) {
                    (tab.url, tab.title) = (url.to_string(), title.to_string());
                }
            }
        }
        let tree = inst.call(tab_id, "Accessibility.getFullAXTree", json!({})).await?;
        let (outline, cut) = snapshot::outline(&tree);
        let (title, url, loading, errors) = {
            let st = inst.st.lock().unwrap();
            let t = st.tabs.iter().find(|t| t.id == tab_id).ok_or_else(|| anyhow!("the tab closed"))?;
            let errors = t.log.iter().filter(|(n, l)| *n > t.log_read && (l.starts_with("[uncaught]") || l.starts_with("[error]") || l.starts_with("[console.error]") || l.starts_with("[network]"))).count();
            (t.title.clone(), t.url.clone(), t.loading, errors)
        };
        let mut s = format!("Tab {tab_id}\nTitle: {title}\nURL: {url}\n");
        if loading {
            s.push_str("(still loading)\n");
        }
        s.push('\n');
        s.push_str(&outline);
        if cut {
            s.push_str("[snapshot cut short: the page is long; scroll or use browser_evaluate for the rest]\n");
        }
        if errors > 0 {
            s.push_str(&format!("\n{errors} new console error{} — see browser_console.\n", if errors == 1 { "" } else { "s" }));
        }
        Ok(s)
    }

    /// The element a ref names, as a remote object id.
    async fn element(&self, inst: &Instance, tab_id: &str, r: &str) -> Result<String> {
        let id = snapshot::parse_ref(r).ok_or_else(|| anyhow!("{r:?} is not a ref; give one from the snapshot, like e123"))?;
        let v = inst
            .call(tab_id, "DOM.resolveNode", json!({"backendNodeId": id}))
            .await
            .map_err(|_| anyhow!("there's no element {r} on this page any more; take a new snapshot (browser_snapshot)"))?;
        v["object"]["objectId"].as_str().map(str::to_string).ok_or_else(|| anyhow!("{r} can't be used (not an element)"))
    }

    async fn on_element(&self, inst: &Instance, tab_id: &str, object: &str, function: &str, args: Value) -> Result<Value> {
        let r = inst
            .call(tab_id, "Runtime.callFunctionOn", json!({"objectId": object, "functionDeclaration": function, "arguments": args, "returnByValue": true, "awaitPromise": true}))
            .await?;
        if let Some(e) = r.get("exceptionDetails") {
            return Err(anyhow!("{}", e["exception"]["description"].as_str().or(e["text"].as_str()).unwrap_or("the page threw an error")));
        }
        Ok(r["result"]["value"].clone())
    }

    async fn press_keys(&self, inst: &Instance, tab_id: &str, key: &str, mods: &BrowserMods) -> Result<()> {
        for e in keys::press(key, None, mods) {
            inst.call(tab_id, "Input.dispatchKeyEvent", e).await?;
        }
        Ok(())
    }

    /// `browser_navigate`: an address, or back / forward / reload.
    pub async fn agent_navigate(&self, session_id: &str, workspace_id: &str, url: Option<&str>, action: Option<&str>) -> Result<AgentOut> {
        let (inst, tab) = self.session_tab(session_id, workspace_id).await?;
        let before = self.loads(&inst, &tab);
        match (url.filter(|u| !u.trim().is_empty()), action) {
            (Some(u), _) => {
                let url = browser_url(u).map_err(|e| anyhow!("{e}"))?;
                let r = inst.call(&tab, "Page.navigate", json!({"url": url})).await?;
                if let Some(err) = r["errorText"].as_str().filter(|e| !e.is_empty()) {
                    return Err(anyhow!("{}", explain_net_error(err, &url)));
                }
            }
            (None, Some(a)) => {
                let a = match a {
                    "back" => BrowserAction::Back,
                    "forward" => BrowserAction::Forward,
                    "reload" => BrowserAction::Reload,
                    other => return Err(anyhow!("unknown action {other:?}: back, forward or reload")),
                };
                super::go(&inst, &tab, a).await?;
            }
            (None, None) => return Err(anyhow!("give a url (or an action: back, forward, reload)")),
        }
        self.settle(&inst, &tab, before).await;
        Ok(AgentOut::text(self.describe(&inst, &tab).await?))
    }

    pub async fn agent_snapshot(&self, session_id: &str, workspace_id: &str) -> Result<AgentOut> {
        let (inst, tab) = self.session_tab(session_id, workspace_id).await?;
        Ok(AgentOut::text(self.describe(&inst, &tab).await?))
    }

    pub async fn agent_click(&self, session_id: &str, workspace_id: &str, r: &str, double: bool, button: BrowserButton) -> Result<AgentOut> {
        let (inst, tab) = self.session_tab(session_id, workspace_id).await?;
        let obj = self.element(&inst, &tab, r).await?;
        self.on_element(&inst, &tab, &obj, "function(){ this.scrollIntoView({block: 'center', inline: 'center', behavior: 'instant'}); }", json!([])).await?;
        let q = inst.call(&tab, "DOM.getContentQuads", json!({"objectId": obj})).await.map_err(|_| anyhow!("{r} isn't on screen (it has no box); it may be hidden"))?;
        let quad = q["quads"].as_array().and_then(|a| a.first()).and_then(Value::as_array).ok_or_else(|| anyhow!("{r} isn't on screen (it has no box); it may be hidden"))?;
        let pts: Vec<f64> = quad.iter().filter_map(Value::as_f64).collect();
        if pts.len() < 8 {
            return Err(anyhow!("{r} has no box on screen"));
        }
        let (x, y) = ((pts[0] + pts[2] + pts[4] + pts[6]) / 4.0, (pts[1] + pts[3] + pts[5] + pts[7]) / 4.0);
        let name = super::button_name(button);
        let bit = super::button_bit(button);
        let before = self.loads(&inst, &tab);
        inst.call(&tab, "Input.dispatchMouseEvent", json!({"type": "mouseMoved", "x": x, "y": y})).await?;
        for n in 1..=if double { 2 } else { 1 } {
            inst.call(&tab, "Input.dispatchMouseEvent", json!({"type": "mousePressed", "x": x, "y": y, "button": name, "buttons": bit, "clickCount": n})).await?;
            inst.call(&tab, "Input.dispatchMouseEvent", json!({"type": "mouseReleased", "x": x, "y": y, "button": name, "buttons": 0, "clickCount": n})).await?;
        }
        self.settle(&inst, &tab, before).await;
        Ok(AgentOut::text(self.describe(&inst, &tab).await?))
    }

    /// `browser_type`: put text in a field (replacing what's there unless
    /// `append`), and press Enter after if `submit`.
    pub async fn agent_type(&self, session_id: &str, workspace_id: &str, r: &str, text: &str, submit: bool, append: bool) -> Result<AgentOut> {
        let (inst, tab) = self.session_tab(session_id, workspace_id).await?;
        let obj = self.element(&inst, &tab, r).await?;
        let focused = self
            .on_element(
                &inst,
                &tab,
                &obj,
                "function(append){
                    this.scrollIntoView({block: 'center', behavior: 'instant'});
                    this.focus();
                    const inside = document.activeElement === this || this.contains(document.activeElement);
                    if (!append) {
                        try {
                            if (this.tagName === 'INPUT' || this.tagName === 'TEXTAREA') { this.select(); }
                            else if (this.isContentEditable) { const r = document.createRange(); r.selectNodeContents(this); const s = getSelection(); s.removeAllRanges(); s.addRange(r); }
                        } catch (e) {}
                    } else if (this.tagName === 'INPUT' || this.tagName === 'TEXTAREA') {
                        try { const n = this.value.length; this.setSelectionRange(n, n); } catch (e) {}
                    }
                    return inside;
                }",
                json!([{"value": append}]),
            )
            .await?;
        if focused != json!(true) {
            return Err(anyhow!("{r} can't be typed in (it doesn't take the keyboard)"));
        }
        let before = self.loads(&inst, &tab);
        if text.is_empty() {
            if !append {
                self.press_keys(&inst, &tab, "Backspace", &BrowserMods::default()).await?;
            }
        } else {
            inst.call(&tab, "Input.insertText", json!({"text": text})).await?;
        }
        if submit {
            self.press_keys(&inst, &tab, "Enter", &BrowserMods::default()).await?;
        }
        self.settle(&inst, &tab, before).await;
        Ok(AgentOut::text(self.describe(&inst, &tab).await?))
    }

    pub async fn agent_press(&self, session_id: &str, workspace_id: &str, combo: &str) -> Result<AgentOut> {
        let (key, mods) = keys::parse_combo(combo).map_err(|e| anyhow!("{e}"))?;
        let (inst, tab) = self.session_tab(session_id, workspace_id).await?;
        let before = self.loads(&inst, &tab);
        self.press_keys(&inst, &tab, &key, &mods).await?;
        self.settle(&inst, &tab, before).await;
        Ok(AgentOut::text(self.describe(&inst, &tab).await?))
    }

    pub async fn agent_select(&self, session_id: &str, workspace_id: &str, r: &str, values: &[String]) -> Result<AgentOut> {
        let (inst, tab) = self.session_tab(session_id, workspace_id).await?;
        let obj = self.element(&inst, &tab, r).await?;
        let res = self
            .on_element(
                &inst,
                &tab,
                &obj,
                "function(want){
                    if (this.tagName !== 'SELECT') return 'not a select element';
                    let n = 0;
                    for (const o of this.options) {
                        const on = want.includes(o.value) || want.includes(o.label) || want.includes(o.text.trim());
                        if (this.multiple) { o.selected = on; if (on) n++; }
                        else if (on && n === 0) { this.value = o.value; n++; }
                    }
                    if (!n) return 'no option matched; options: ' + Array.from(this.options).map(o => JSON.stringify(o.label)).join(', ');
                    this.dispatchEvent(new Event('input', {bubbles: true}));
                    this.dispatchEvent(new Event('change', {bubbles: true}));
                    return 'ok';
                }",
                json!([{"value": values}]),
            )
            .await?;
        if res != json!("ok") {
            return Err(anyhow!("{}", res.as_str().unwrap_or("could not select")));
        }
        let before = self.loads(&inst, &tab);
        self.settle(&inst, &tab, before).await;
        Ok(AgentOut::text(self.describe(&inst, &tab).await?))
    }

    pub async fn agent_screenshot(&self, session_id: &str, workspace_id: &str, full_page: bool) -> Result<AgentOut> {
        let (inst, tab) = self.session_tab(session_id, workspace_id).await?;
        let mut p = json!({"format": "jpeg", "quality": 80});
        if full_page {
            let m = inst.call(&tab, "Page.getLayoutMetrics", json!({})).await?;
            let size = &m["cssContentSize"];
            let (w, h) = (size["width"].as_f64().unwrap_or(1280.0), size["height"].as_f64().unwrap_or(800.0).min(16_000.0));
            p["captureBeyondViewport"] = json!(true);
            p["clip"] = json!({"x": 0, "y": 0, "width": w, "height": h, "scale": 1});
        }
        let r = inst.call(&tab, "Page.captureScreenshot", p).await?;
        let data = r["data"].as_str().ok_or_else(|| anyhow!("no picture came back"))?.to_string();
        let url = inst.describe_tab(&tab).map(|t| t.url).unwrap_or_default();
        Ok(AgentOut { text: format!("A screenshot of {url}{}.", if full_page { " (the whole page)" } else { "" }), image: Some(data) })
    }

    /// `browser_console`: what the page logged since the last time asked.
    pub async fn agent_console(&self, session_id: &str, workspace_id: &str) -> Result<AgentOut> {
        let (inst, tab) = self.session_tab(session_id, workspace_id).await?;
        let mut st = inst.st.lock().unwrap();
        let t = st.tabs.iter_mut().find(|t| t.id == tab).ok_or_else(|| anyhow!("the tab closed"))?;
        let lines: Vec<String> = t.log.iter().filter(|(n, _)| *n > t.log_read).map(|(_, l)| l.clone()).collect();
        t.log_read = t.log_seq;
        Ok(AgentOut::text(if lines.is_empty() { "Nothing new in the console.".to_string() } else { lines.join("\n") }))
    }

    pub async fn agent_evaluate(&self, session_id: &str, workspace_id: &str, expression: &str) -> Result<AgentOut> {
        let (inst, tab) = self.session_tab(session_id, workspace_id).await?;
        let r = inst
            .call(&tab, "Runtime.evaluate", json!({"expression": expression, "returnByValue": true, "awaitPromise": true, "userGesture": true, "timeout": 10_000}))
            .await?;
        if let Some(e) = r.get("exceptionDetails") {
            return Err(anyhow!("it threw: {}", e["exception"]["description"].as_str().or(e["text"].as_str()).unwrap_or("an error")));
        }
        let res = &r["result"];
        let text = match res.get("value") {
            Some(Value::String(s)) => s.clone(),
            Some(v) => serde_json::to_string_pretty(v).unwrap_or_default(),
            None => res["description"].as_str().or(res["type"].as_str()).unwrap_or("undefined").to_string(),
        };
        let (text, cut) = crate::util::truncate(&text, EVAL_LIMIT);
        Ok(AgentOut::text(if cut { format!("{text}\n[cut short]") } else { text }))
    }

    /// `browser_wait_for`: until text shows (or goes), or some seconds.
    pub async fn agent_wait(&self, session_id: &str, workspace_id: &str, text: Option<&str>, gone: Option<&str>, seconds: Option<f64>) -> Result<AgentOut> {
        let (inst, tab) = self.session_tab(session_id, workspace_id).await?;
        let limit = Duration::from_secs_f64(seconds.unwrap_or(if text.is_some() || gone.is_some() { 10.0 } else { 2.0 }).clamp(0.1, MAX_WAIT_SECS));
        let deadline = Instant::now() + limit;
        if text.is_none() && gone.is_none() {
            tokio::time::sleep(limit).await;
            return Ok(AgentOut::text(self.describe(&inst, &tab).await?));
        }
        loop {
            let r = inst.call(&tab, "Runtime.evaluate", json!({"expression": "document.body ? document.body.innerText : ''", "returnByValue": true})).await?;
            let body = r["result"]["value"].as_str().unwrap_or("");
            let shown = text.is_none_or(|t| body.contains(t));
            let went = gone.is_none_or(|g| !body.contains(g));
            if shown && went {
                return Ok(AgentOut::text(self.describe(&inst, &tab).await?));
            }
            if Instant::now() >= deadline {
                let what = match (text, gone) {
                    (Some(t), _) if !shown => format!("{t:?} did not appear"),
                    (_, Some(g)) => format!("{g:?} did not go away"),
                    _ => "the page did not change".into(),
                };
                return Err(anyhow!("{what} within {:.0}s", limit.as_secs_f64()));
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    /// `browser_tabs`: this session's tabs; switch to one or close one.
    pub async fn agent_tabs(&self, session_id: &str, workspace_id: &str, select: Option<&str>, close: Option<&str>) -> Result<AgentOut> {
        let (inst, current) = self.session_tab(session_id, workspace_id).await?;
        let mine = |id: &str| inst.st.lock().unwrap().tabs.iter().any(|t| t.id == id && t.session_id.as_deref() == Some(session_id));
        if let Some(id) = close {
            if !mine(id) {
                return Err(anyhow!("{id} is not one of this session's tabs"));
            }
            self.close_tab(id).await.map_err(|e| anyhow!("{}", e.message))?;
        }
        if let Some(id) = select {
            if !mine(id) {
                return Err(anyhow!("{id} is not one of this session's tabs"));
            }
            self.session_tabs.lock().unwrap().insert(session_id.to_string(), id.to_string());
            self.follow_recording(&inst, session_id, id).await;
        }
        let current = self.session_tabs.lock().unwrap().get(session_id).cloned().unwrap_or(current);
        let st = inst.st.lock().unwrap();
        let lines: Vec<String> = st
            .tabs
            .iter()
            .filter(|t| t.session_id.as_deref() == Some(session_id))
            .map(|t| format!("{} {} — {} ({})", if t.id == current { "*" } else { " " }, t.id, if t.title.is_empty() { "untitled" } else { &t.title }, t.url))
            .collect();
        Ok(AgentOut::text(if lines.is_empty() { "This session has no tabs.".into() } else { format!("This session's tabs (* is the one the tools act on):\n{}", lines.join("\n")) }))
    }
}

// ---- Recording ----

impl Browsers {
    /// `browser_record_start`: record the session's tab until
    /// `browser_record_stop` (it follows `browser_tabs` to another tab).
    pub async fn agent_record_start(&self, session_id: &str, workspace_id: &str) -> Result<AgentOut> {
        if self.recordings.lock().unwrap().contains_key(session_id) {
            return Err(anyhow!("already recording: call browser_record_stop to send it first"));
        }
        let (inst, tab) = self.session_tab(session_id, workspace_id).await?;
        let first = inst.st.lock().unwrap().tabs.iter().find(|t| t.id == tab && t.casting).and_then(|t| t.last_frame.as_ref().map(|f| f.data.clone()));
        let recorder = record::Recorder::start(first)?;
        let sink = recorder.sink.clone();
        {
            let mut recs = self.recordings.lock().unwrap();
            if recs.contains_key(session_id) {
                return Err(anyhow!("already recording: call browser_record_stop to send it first"));
            }
            recs.insert(session_id.to_string(), Recording { recorder, workspace_id: workspace_id.to_string(), tab_id: tab.clone() });
        }
        if let Err(e) = self.record_into(&inst, &tab, sink).await {
            self.recordings.lock().unwrap().remove(session_id);
            return Err(e);
        }
        self.ctx.audit.log("browser.record_start", json!({"session_id": session_id, "workspace_id": workspace_id}));
        Ok(AgentOut::text(format!(
            "Recording your browser tab (at most {} minutes). Do what you want to show, then call browser_record_stop to send the video to the user.",
            record::MAX_LENGTH.as_secs() / 60
        )))
    }

    /// `browser_record_stop`: the recording, finished.
    pub async fn agent_record_stop(&self, session_id: &str) -> Result<record::Finished> {
        let rec = self.recordings.lock().unwrap().remove(session_id).ok_or_else(|| anyhow!("not recording: call browser_record_start first"))?;
        if let Some(inst) = self.instance(&rec.workspace_id) {
            unrecord(&inst, &rec.tab_id);
        }
        let f = tokio::task::spawn_blocking(move || rec.recorder.finish()).await.map_err(|_| anyhow!("the recorder crashed"))??;
        self.ctx.audit.log("browser.record_stop", json!({"session_id": session_id, "duration_ms": f.duration_ms, "bytes": f.mp4.len()}));
        Ok(f)
    }

    /// Throw away a session's recording (the session ended).
    pub fn discard_recording(&self, session_id: &str) {
        let Some(rec) = self.recordings.lock().unwrap().remove(session_id) else { return };
        if let Some(inst) = self.instance(&rec.workspace_id) {
            unrecord(&inst, &rec.tab_id);
        }
    }

    /// The session's tools moved to another tab: so does its recording.
    async fn follow_recording(&self, inst: &Instance, session_id: &str, tab_id: &str) {
        let sink = {
            let mut recs = self.recordings.lock().unwrap();
            let Some(r) = recs.get_mut(session_id) else { return };
            if r.tab_id == tab_id {
                return;
            }
            let old = std::mem::replace(&mut r.tab_id, tab_id.to_string());
            unrecord(inst, &old);
            r.recorder.sink.clone()
        };
        if let Err(e) = self.record_into(inst, tab_id, sink).await {
            tracing::debug!("browser: the recording could not follow to {tab_id}: {e:#}");
        }
    }

    /// Send a tab's frames to a recording, starting its screencast if
    /// nobody watches it.
    async fn record_into(&self, inst: &Instance, tab_id: &str, sink: Arc<record::Sink>) -> Result<()> {
        let start = {
            let mut st = inst.st.lock().unwrap();
            let t = st.tabs.iter_mut().find(|t| t.id == tab_id).ok_or_else(|| anyhow!("the tab closed"))?;
            if t.casting {
                if let Some(f) = &t.last_frame {
                    sink.offer(f.data.clone());
                }
            }
            t.rec = Some(sink);
            let start = !t.casting;
            t.casting = true;
            start.then_some((t.width, t.height))
        };
        if let Some((w, h)) = start {
            inst.call(tab_id, "Page.startScreencast", super::screencast_params(w, h)).await?;
        }
        Ok(())
    }
}

/// Stop sending a tab's frames to a recording, and its screencast if
/// nobody watches it.
fn unrecord(inst: &Instance, tab_id: &str) {
    let mut st = inst.st.lock().unwrap();
    let Some(t) = st.tabs.iter_mut().find(|t| t.id == tab_id) else { return };
    t.rec = None;
    t.viewers.retain(|v| !v.gone.load(std::sync::atomic::Ordering::Relaxed));
    if t.viewers.is_empty() && t.casting {
        t.casting = false;
        inst.cdp.send("Page.stopScreencast", json!({}), Some(&t.cdp));
    }
}

fn mark_used(inst: &Instance, tab_id: &str) {
    if let Some(t) = inst.st.lock().unwrap().tabs.iter_mut().find(|t| t.id == tab_id) {
        t.used = Instant::now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_say_what_to_do() {
        assert!(explain_net_error("net::ERR_TUNNEL_CONNECTION_FAILED", "https://staging.example.com/x").contains("allowed hosts"));
        assert!(explain_net_error("net::ERR_TUNNEL_CONNECTION_FAILED", "http://localhost:3000").contains("localhost"));
        assert!(explain_net_error("net::ERR_NAME_NOT_RESOLVED", "https://nope.invalid").contains("ERR_NAME_NOT_RESOLVED"));
    }
}
