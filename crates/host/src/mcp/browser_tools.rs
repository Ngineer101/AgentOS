//! `agenthost__browser_*`: the workspace's browser, for agents.
//!
//! The browser is signed in to what the user signed in to in this
//! workspace, so the first call of a session asks the user (unless they let
//! the browser run without asking, or an automation allows it); after that
//! the session uses it freely. Every call is in the session's transcript.

use super::*;
use crate::browser::AgentOut;

/// What every browser tool says about the browser.
const ABOUT: &str = "The browser is a real Chromium on the user's computer (outside your sandbox), one per workspace, \
     signed in to the sites the user signed in to in it; the user can watch it in the app's Browser panel. \
     You get a tab of your own. It reaches the hosts this workspace's network settings allow, and nothing on the user's computer \
     (no localhost) yet. Never type passwords or sign in yourself: if a page needs a sign-in, ask the user to sign in in the Browser panel, \
     then carry on.";

fn def(name: &str, title: &str, description: &str, props: Value, required: &[&str], read_only: bool) -> Value {
    json!({
        "name": format!("{BROWSER_TOOL_PREFIX}{name}"),
        "description": format!("[AgentOS] {description}"),
        "inputSchema": {"type": "object", "properties": props, "required": required},
        "annotations": {"readOnlyHint": read_only, "destructiveHint": false, "openWorldHint": true, "title": title}
    })
}

pub(super) fn browser_tool_defs() -> Vec<Value> {
    let r = json!({"type": "string", "description": "The element's ref from the latest snapshot, e.g. e123."});
    vec![
        def(
            "navigate",
            "Open a page in the browser",
            &format!("Open a web address in your browser tab (or go back, forward, reload), wait for it to load, and return the page as an outline of its elements, each with a ref for the other tools. {ABOUT}"),
            json!({
                "url": {"type": "string", "description": "An http(s) address; a bare host gets https://."},
                "action": {"type": "string", "enum": ["back", "forward", "reload"], "description": "Instead of a url."}
            }),
            &[],
            false,
        ),
        def("snapshot", "Read the page", "The page in your browser tab as an outline of its elements (accessibility tree), each with a ref (e123) for browser_click, browser_type and browser_select_option. Take a new one when the page has changed.", json!({}), &[], true),
        def(
            "click",
            "Click in the page",
            "Click an element of the page (by its ref from the snapshot) as a person would, wait for what it sets off, and return the page.",
            json!({"ref": r, "double_click": {"type": "boolean"}, "button": {"type": "string", "enum": ["left", "right", "middle"]}}),
            &["ref"],
            false,
        ),
        def(
            "type",
            "Type in the page",
            "Type text into a field (by its ref), replacing what is in it unless `append`, and press Enter after if `submit`. Returns the page.",
            json!({"ref": r, "text": {"type": "string"}, "submit": {"type": "boolean", "description": "Press Enter after typing."}, "append": {"type": "boolean", "description": "Add to the field's text instead of replacing it."}}),
            &["ref", "text"],
            false,
        ),
        def(
            "press_key",
            "Press a key",
            "Press a key in the page, with modifiers if any: Enter, Escape, Tab, ArrowDown, Backspace, a, Control+a, Shift+Tab, Meta+Enter. Returns the page.",
            json!({"key": {"type": "string"}}),
            &["key"],
            false,
        ),
        def(
            "select_option",
            "Choose in a list",
            "Choose options of a <select> (by its ref), by value or label. Returns the page.",
            json!({"ref": r, "values": {"type": "array", "items": {"type": "string"}, "minItems": 1}}),
            &["ref", "values"],
            false,
        ),
        def("screenshot", "Look at the page", "A picture of your browser tab, to check how it looks. Prefer browser_snapshot to find things and act on them.", json!({"full_page": {"type": "boolean", "description": "The whole page, not only what fits in the window."}}), &[], true),
        def("console", "Read the console", "What the page logged since you last asked: console messages, uncaught errors, failed requests, refused hosts and dialogs (which are accepted for you).", json!({}), &[], true),
        def(
            "evaluate",
            "Run JavaScript in the page",
            "Run a JavaScript expression in the page and return its value (as JSON). Promises are awaited. Use it to check state the outline doesn't show.",
            json!({"expression": {"type": "string", "description": "e.g. document.querySelectorAll('li').length, or (async () => { … })()"}}),
            &["expression"],
            false,
        ),
        def(
            "wait_for",
            "Wait for the page",
            "Wait until text appears on the page (or goes away), up to `seconds` (default 10, at most 30); with neither, just wait. Returns the page.",
            json!({"text": {"type": "string"}, "text_gone": {"type": "string"}, "seconds": {"type": "number"}}),
            &[],
            true,
        ),
        def(
            "tabs",
            "Your browser tabs",
            "List your tabs (yours and the popups they opened, such as a sign-in window), switch the other tools to one (`select`), or close one.",
            json!({"select": {"type": "string", "description": "A tab id from the list."}, "close": {"type": "string", "description": "A tab id from the list."}}),
            &[],
            false,
        ),
    ]
}

impl Gateway {
    /// Whether this session may use the browser: asked once per session.
    async fn browser_allowed(self: &Arc<Self>, session_id: &str, workspace_id: &str, tool: &str, args: &Value) -> bool {
        if self.browser_ok.lock().unwrap().contains(session_id) {
            return true;
        }
        let ok = self.builtin_policy("browser") == ToolPolicy::Auto || self.automation_allows(session_id, "browser").is_some() || {
            let ws = self.ctx.store.workspace_get(workspace_id).ok().flatten().map(|w| w.name).unwrap_or_else(|| "this workspace".into());
            let summary = format!("Use the {ws} browser for this session. It is signed in to the sites you signed in to there; the agent can act on them as you. Asks once per session.");
            let shown = json!({"tool": format!("{BROWSER_TOOL_PREFIX}{tool}"), "arguments": args});
            self.await_approval(session_id, BUILTIN_CONNECTOR_ID, "AgentOS", "browser", &shown, Some(summary)).await == ApprovalDecision::Approve
        };
        if ok {
            self.browser_ok.lock().unwrap().insert(session_id.to_string());
        }
        ok
    }

    pub(super) async fn browser_tool(self: &Arc<Self>, session_id: &str, workspace_id: &str, tool: &str, args: Value) -> Value {
        let call_id = new_id("mcp");
        let exposed = format!("{BROWSER_TOOL_PREFIX}{tool}");
        let read_only = browser_tool_defs().iter().find(|d| d["name"] == exposed).map(|d| d["annotations"]["readOnlyHint"] == true);
        let _ = self.ctx.append(
            session_id,
            EventType::ToolCall,
            json!({
                "tool_call_id": call_id, "name": tool, "exposed_name": exposed, "arguments": args, "source": "mcp",
                "connector_id": BUILTIN_CONNECTOR_ID, "connector_name": "AgentOS",
                "tool_kind": if read_only == Some(true) { ToolKind::Read } else { ToolKind::Write },
                "policy": self.builtin_policy("browser")
            }),
        );
        let finish = |status: &str, output: &str| {
            let (out, truncated) = truncate(output, crate::session::TOOL_OUTPUT_LIMIT);
            let _ = self.ctx.append(
                session_id,
                EventType::ToolResult,
                json!({"tool_call_id": call_id, "name": tool, "status": status, "output": out, "truncated": truncated, "source": "mcp", "connector_id": BUILTIN_CONNECTOR_ID}),
            );
        };
        if read_only.is_none() {
            let e = format!("unknown tool {exposed}");
            finish("failed", &e);
            return text_result(&format!("AgentOS: {e}"), true);
        }
        if self.rate_limited(session_id) {
            finish("failed", "rate limit reached");
            return text_result("AgentOS: too many calls this minute; try again shortly", true);
        }
        if !self.browser_allowed(session_id, workspace_id, tool, &args).await {
            finish("denied", "the user did not let this session use the browser");
            return text_result("The user did not let this session use the browser. Don't try again in this session; ask the user if you need it.", true);
        }
        let s = |k: &str| args[k].as_str().map(str::to_string);
        let b = &self.browsers;
        let need_ref = || s("ref").ok_or_else(|| anyhow!("give the element's `ref` from the snapshot"));
        let res: Result<AgentOut> = match tool {
            "navigate" => b.agent_navigate(session_id, workspace_id, s("url").as_deref(), s("action").as_deref()).await,
            "snapshot" => b.agent_snapshot(session_id, workspace_id).await,
            "click" => match need_ref() {
                Ok(r) => {
                    let button = match args["button"].as_str() {
                        Some("right") => BrowserButton::Right,
                        Some("middle") => BrowserButton::Middle,
                        _ => BrowserButton::Left,
                    };
                    b.agent_click(session_id, workspace_id, &r, args["double_click"].as_bool().unwrap_or(false), button).await
                }
                Err(e) => Err(e),
            },
            "type" => match (need_ref(), s("text")) {
                (Ok(r), Some(t)) => b.agent_type(session_id, workspace_id, &r, &t, args["submit"].as_bool().unwrap_or(false), args["append"].as_bool().unwrap_or(false)).await,
                (Err(e), _) => Err(e),
                (_, None) => Err(anyhow!("give the `text` to type")),
            },
            "press_key" => match s("key") {
                Some(k) => b.agent_press(session_id, workspace_id, &k).await,
                None => Err(anyhow!("give a `key`, e.g. Enter")),
            },
            "select_option" => {
                let values: Vec<String> = args["values"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();
                match need_ref() {
                    Ok(_) if values.is_empty() => Err(anyhow!("give the `values` to choose")),
                    Ok(r) => b.agent_select(session_id, workspace_id, &r, &values).await,
                    Err(e) => Err(e),
                }
            }
            "screenshot" => b.agent_screenshot(session_id, workspace_id, args["full_page"].as_bool().unwrap_or(false)).await,
            "console" => b.agent_console(session_id, workspace_id).await,
            "evaluate" => match s("expression") {
                Some(e) => b.agent_evaluate(session_id, workspace_id, &e).await,
                None => Err(anyhow!("give an `expression`")),
            },
            "wait_for" => b.agent_wait(session_id, workspace_id, s("text").as_deref(), s("text_gone").as_deref(), args["seconds"].as_f64()).await,
            "tabs" => b.agent_tabs(session_id, workspace_id, s("select").as_deref(), s("close").as_deref()).await,
            _ => Err(anyhow!("unknown tool {exposed}")),
        };
        match res {
            Ok(out) => {
                finish("completed", &out.text);
                let mut content = vec![json!({"type": "text", "text": out.text})];
                if let Some(img) = out.image {
                    content.push(json!({"type": "image", "data": img, "mimeType": "image/jpeg"}));
                }
                json!({"content": content, "isError": false})
            }
            Err(e) => {
                let msg = format!("{e:#}");
                finish("failed", &msg);
                text_result(&msg, true)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_is_named_and_described() {
        let defs = browser_tool_defs();
        assert_eq!(defs.len(), 11);
        for d in &defs {
            let name = d["name"].as_str().unwrap();
            assert!(name.starts_with(BROWSER_TOOL_PREFIX), "{name}");
            assert!(d["description"].as_str().unwrap().starts_with("[AgentOS]"), "{name}");
            assert_eq!(d["inputSchema"]["type"], "object", "{name}");
        }
        // Agents are told not to sign in themselves.
        assert!(defs[0]["description"].as_str().unwrap().contains("Never type passwords"));
    }
}
