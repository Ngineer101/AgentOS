//! What an agent reads of a page: its accessibility tree as an indented
//! outline, one element per line, each with a ref (`e123`, the element's
//! backend DOM node id) that the click and type tools take.
//!
//! ```text
//! - heading "Sign in" [level=1] [ref=e14]
//! - textbox "Email" [focused] [ref=e20]
//! - button "Next" [ref=e22]
//! ```

use serde_json::Value;
use std::collections::HashMap;

/// Most characters of outline an agent gets; the rest is cut.
pub const SNAPSHOT_LIMIT: usize = 40_000;

/// Roles that only group things: drawn by their children, not themselves,
/// unless they have a name.
const SILENT: &[&str] = &["generic", "none", "presentation", "LineBreak", "InlineTextBox", "RootWebArea", "WebArea", "LayoutTable", "LayoutTableRow", "LayoutTableCell", "group"];

/// Properties worth showing, in this order.
const SHOWN: &[&str] = &["focused", "checked", "pressed", "selected", "expanded", "disabled", "required", "readonly", "level", "invalid", "url"];

struct Node<'a> {
    role: &'a str,
    name: &'a str,
    value: Option<String>,
    props: Vec<String>,
    backend: Option<u64>,
    children: Vec<&'a str>,
    ignored: bool,
}

fn text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn node(v: &Value) -> Option<(&str, Node<'_>)> {
    let id = v["nodeId"].as_str()?;
    let role = v["role"]["value"].as_str().unwrap_or("");
    let name = v["name"]["value"].as_str().unwrap_or("").trim();
    let value = text(&v["value"]["value"]).filter(|s| !s.is_empty());
    let mut props = vec![];
    if let Some(list) = v["properties"].as_array() {
        for want in SHOWN {
            let Some(p) = list.iter().find(|p| p["name"] == *want) else { continue };
            let val = &p["value"]["value"];
            match (*want, val) {
                // A link's address says where it goes.
                ("url", Value::String(u)) if role == "link" => props.push(format!("url={}", short(u, 120))),
                ("url", _) => {}
                // Checkboxes and toggles say when they're off, too (their state is
                // a "tristate" string: "true", "false" or "mixed").
                (_, Value::Bool(false)) if matches!(*want, "checked" | "pressed" | "expanded") => props.push(format!("{want}=false")),
                (_, Value::String(s)) if s == "false" && matches!(*want, "checked" | "pressed" | "expanded") => props.push(format!("{want}=false")),
                (_, Value::Bool(true)) => props.push(want.to_string()),
                (_, Value::Bool(false)) => {}
                (_, Value::String(s)) if s == "false" => {}
                (_, Value::String(s)) if s == "true" => props.push(want.to_string()),
                (_, v) => {
                    if let Some(s) = text(v) {
                        props.push(format!("{want}={s}"));
                    }
                }
            }
        }
    }
    let children = v["childIds"].as_array().map(|c| c.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    Some((id, Node { role, name, value, props, backend: v["backendDOMNodeId"].as_u64(), children, ignored: v["ignored"].as_bool().unwrap_or(false) }))
}

fn short(s: &str, max: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= max {
        return s;
    }
    let cut: String = s.chars().take(max).collect();
    format!("{cut}…")
}

/// The outline of an `Accessibility.getFullAXTree` result, and whether it
/// was cut short.
pub fn outline(tree: &Value) -> (String, bool) {
    let nodes: Vec<&Value> = tree["nodes"].as_array().map(|a| a.iter().collect()).unwrap_or_default();
    let mut by_id: HashMap<&str, Node> = HashMap::new();
    let mut first = None;
    for v in &nodes {
        if let Some((id, n)) = node(v) {
            if first.is_none() && v.get("parentId").is_none() {
                first = Some(id);
            }
            by_id.insert(id, n);
        }
    }
    let mut out = String::new();
    let mut cut = false;
    if let Some(root) = first {
        walk(&by_id, root, 0, "", &mut out, &mut cut, 0);
    }
    if out.is_empty() {
        out.push_str("(the page has no content yet)\n");
    }
    (out, cut)
}

fn walk(nodes: &HashMap<&str, Node>, id: &str, depth: usize, parent_name: &str, out: &mut String, cut: &mut bool, guard: usize) {
    if *cut || guard > 400 {
        return;
    }
    let Some(n) = nodes.get(id) else { return };
    let silent = n.ignored || (SILENT.contains(&n.role) && n.name.is_empty()) || n.role == "InlineTextBox";
    let text_node = n.role == "StaticText";
    let mut next_depth = depth;
    let mut name_here = parent_name;
    // A field's text is its value, already on its line.
    let field = matches!(n.role, "textbox" | "searchbox" | "spinbutton");
    if !silent {
        if text_node {
            // A link's or button's own text says nothing its name didn't.
            if !n.name.is_empty() && n.name != parent_name {
                line(out, depth, &format!("text: {}", short(n.name, 300)));
            }
        } else {
            let mut l = n.role.to_string();
            if !n.name.is_empty() {
                l.push_str(&format!(" \"{}\"", short(n.name, 200).replace('"', "\\\"")));
            }
            if let Some(v) = &n.value {
                if v != n.name {
                    l.push_str(&format!(": {}", short(v, 200)));
                }
            }
            for p in &n.props {
                l.push_str(&format!(" [{p}]"));
            }
            if let Some(b) = n.backend {
                l.push_str(&format!(" [ref=e{b}]"));
            }
            line(out, depth, &l);
            next_depth = depth + 1;
            name_here = n.name;
            if field {
                return;
            }
        }
        if out.len() > SNAPSHOT_LIMIT {
            *cut = true;
            return;
        }
    }
    for c in &n.children {
        walk(nodes, c, next_depth, name_here, out, cut, guard + 1);
    }
}

fn line(out: &mut String, depth: usize, s: &str) {
    for _ in 0..depth.min(40) {
        out.push_str("  ");
    }
    out.push_str("- ");
    out.push_str(s);
    out.push('\n');
}

/// The backend node id in a ref an agent gave back ("e123", or "123").
pub fn parse_ref(r: &str) -> Option<u64> {
    let r = r.trim().trim_start_matches("[ref=").trim_end_matches(']');
    r.strip_prefix('e').unwrap_or(r).parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn n(id: &str, parent: Option<&str>, role: &str, name: &str, children: &[&str], backend: u64) -> Value {
        let mut v = json!({"nodeId": id, "ignored": false, "role": {"value": role}, "name": {"value": name}, "childIds": children, "backendDOMNodeId": backend});
        if let Some(p) = parent {
            v["parentId"] = json!(p);
        }
        v
    }

    #[test]
    fn a_sign_in_page_reads_as_an_outline() {
        let mut email = n("4", Some("2"), "textbox", "Email", &[], 20);
        email["properties"] = json!([{"name": "focused", "value": {"type": "boolean", "value": true}}, {"name": "required", "value": {"type": "boolean", "value": false}}]);
        email["value"] = json!({"type": "string", "value": "me@example.com"});
        let mut h = n("3", Some("2"), "heading", "Sign in", &["7"], 14);
        h["properties"] = json!([{"name": "level", "value": {"type": "integer", "value": 1}}]);
        let mut check = n("8", Some("2"), "checkbox", "Remember me", &[], 25);
        check["properties"] = json!([{"name": "checked", "value": {"type": "tristate", "value": "false"}}]);
        let tree = json!({"nodes": [
            n("1", None, "RootWebArea", "Sign in – Example", &["2"], 1),
            n("2", Some("1"), "generic", "", &["3", "4", "5", "8", "9"], 2),
            h,
            n("7", Some("3"), "StaticText", "Sign in", &[], 15),
            email,
            n("5", Some("2"), "button", "Next", &["6"], 22),
            n("6", Some("5"), "StaticText", "Next", &[], 23),
            check,
            n("9", Some("2"), "StaticText", "Forgot your password?", &[], 30),
        ]});
        let (text, cut) = outline(&tree);
        assert!(!cut);
        assert_eq!(
            text,
            "- RootWebArea \"Sign in – Example\" [ref=e1]\n  \
             - heading \"Sign in\" [level=1] [ref=e14]\n  \
             - textbox \"Email\": me@example.com [focused] [ref=e20]\n  \
             - button \"Next\" [ref=e22]\n  \
             - checkbox \"Remember me\" [checked=false] [ref=e25]\n  \
             - text: Forgot your password?\n"
        );
    }

    #[test]
    fn ignored_nodes_pass_their_children_up() {
        let mut hidden = n("2", Some("1"), "generic", "", &["3"], 2);
        hidden["ignored"] = json!(true);
        let tree = json!({"nodes": [n("1", None, "RootWebArea", "", &["2"], 1), hidden, n("3", Some("2"), "link", "Docs", &[], 9)]});
        assert_eq!(outline(&tree).0, "- link \"Docs\" [ref=e9]\n");
    }

    #[test]
    fn a_huge_page_is_cut() {
        let kids: Vec<String> = (0..5000).map(|i| format!("c{i}")).collect();
        let mut nodes = vec![n("1", None, "RootWebArea", "", &kids.iter().map(String::as_str).collect::<Vec<_>>(), 1)];
        for (i, k) in kids.iter().enumerate() {
            nodes.push(n(k, Some("1"), "button", &format!("Button number {i}"), &[], 100 + i as u64));
        }
        let (text, cut) = outline(&json!({"nodes": nodes}));
        assert!(cut);
        assert!(text.len() < SNAPSHOT_LIMIT + 300);
    }

    #[test]
    fn refs_read_back() {
        assert_eq!(parse_ref("e123"), Some(123));
        assert_eq!(parse_ref(" [ref=e7]"), Some(7));
        assert_eq!(parse_ref("42"), Some(42));
        assert_eq!(parse_ref("button"), None);
    }
}
