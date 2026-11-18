//! Finding connectors in the integrations.sh catalog, connector icons, and
//! renaming a connector while sessions use it. A local server stands in for
//! integrations.sh and the favicon service.

mod common;
#[path = "common/mcp_server.rs"]
mod mcp_server;

use agenthost_proto::jsonrpc::codes;
use agenthost_proto::methods as m;
use agenthost_proto::*;
use bytes::Bytes;
use common::*;
use http_body_util::Full;
use hyper::{Request, Response};
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const T: Duration = Duration::from_secs(20);
/// A 1×1 PNG.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0x0d, 0x49, 0x48, 0x44, 0x52, 0, 0, 0, 1,
    0, 0, 0, 1, 8, 6, 0, 0, 0, 0x1f, 0x15, 0xc4, 0x89,
];

struct Catalog {
    base: String,
    /// Every request's path and query, in order.
    seen: Arc<Mutex<Vec<String>>>,
}

/// integrations.sh's search and icons, plus a favicon service, on loopback.
async fn catalog() -> Catalog {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let seen = Arc::new(Mutex::new(vec![]));
    let (b, s) = (base.clone(), seen.clone());
    tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else { break };
            let (b, s) = (b.clone(), s.clone());
            tokio::spawn(async move {
                let svc = hyper::service::service_fn(move |req: Request<hyper::body::Incoming>| {
                    let (b, s) = (b.clone(), s.clone());
                    async move {
                        let pq = req.uri().path_and_query().map(|x| x.to_string()).unwrap_or_default();
                        s.lock().unwrap().push(pq.clone());
                        let r = Response::builder();
                        let r = match req.uri().path() {
                            "/api/search" => r.header("content-type", "application/json").body(Full::new(Bytes::from(
                                json!({"results": [
                                    {"domain": "linear.app", "name": "Linear", "description": "Issues", "kinds": ["mcp", "graphql"],
                                     "url": format!("{b}/linear.app/"),
                                     "surfaces": [
                                        {"kind": "mcp", "slug": "linear-app", "url": "https://mcp.linear.app/mcp", "icon": format!("{b}/icon.png"), "auth": {"kind": "oauth"}},
                                        {"kind": "graphql", "url": "https://api.linear.app/graphql", "auth": {"kind": "api_key"}}
                                     ]},
                                    {"domain": "big.example", "name": "Big", "surfaces": [
                                        {"kind": "mcp", "url": "https://big.example/mcp", "icon": format!("{b}/big.png")}
                                    ]},
                                    {"domain": "page.example", "name": "Page", "surfaces": [
                                        {"kind": "mcp", "url": "https://page.example/mcp", "icon": format!("{b}/page"), "auth": {"kind": "api_key"}}
                                    ]},
                                    {"domain": "cli.example", "name": "CLI only", "surfaces": [{"kind": "cli"}]}
                                ]})
                                .to_string(),
                            ))),
                            "/icon.png" | "/favicons" => r.header("content-type", "image/png").body(Full::new(Bytes::from_static(PNG))),
                            "/big.png" => r.header("content-type", "image/png").body(Full::new(Bytes::from(vec![0u8; 70 * 1024]))),
                            _ => r.header("content-type", "text/html").body(Full::new(Bytes::from_static(b"<html></html>"))),
                        };
                        Ok::<_, std::convert::Infallible>(r.unwrap())
                    }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(hyper_util::rt::TokioIo::new(sock), svc)
                    .await;
            });
        }
    });
    Catalog { base, seen }
}

#[tokio::test]
async fn search_finds_mcp_servers_with_icons() {
    let cat = catalog().await;
    let base = cat.base.clone();
    let th = TestHost::start_with(|c| c.integrations_url = Some(base)).await;
    let (c, _n) = th.client().await;
    let found: Vec<ConnectorSuggestion> = c
        .call(m::CONNECTOR_SEARCH, json!({"query": "linear & co"}))
        .await
        .unwrap();
    assert!(cat.seen.lock().unwrap()[0].starts_with("/api/search?q=linear+%26+co"), "{:?}", cat.seen.lock().unwrap());
    // MCP surfaces only; the CLI-only result has none.
    let urls: Vec<&str> = found.iter().map(|s| s.url.as_str()).collect();
    assert_eq!(urls, vec!["https://mcp.linear.app/mcp", "https://big.example/mcp", "https://page.example/mcp"]);
    let linear = &found[0];
    assert_eq!((linear.domain.as_str(), linear.name.as_str()), ("linear.app", "Linear"));
    assert_eq!((linear.auth.as_str(), linear.description.as_deref()), ("oauth", Some("Issues")));
    let icon = linear.icon.as_deref().unwrap();
    assert!(icon.starts_with("data:image/png;base64,"), "{icon}");
    // Too big, and not an image: no icon, but still suggested.
    assert_eq!((found[1].auth.as_str(), found[1].icon.as_deref()), ("none", None));
    assert_eq!((found[2].auth.as_str(), found[2].icon.as_deref()), ("api_key", None));

    // An empty query asks nothing.
    let n = cat.seen.lock().unwrap().len();
    let none: Vec<ConnectorSuggestion> = c.call(m::CONNECTOR_SEARCH, json!({"query": " "})).await.unwrap();
    assert!(none.is_empty());
    assert_eq!(cat.seen.lock().unwrap().len(), n);
}

#[tokio::test]
async fn search_is_off_without_a_catalog() {
    let th = TestHost::start().await;
    let (c, _n) = th.client().await;
    let e = c
        .request(m::CONNECTOR_SEARCH, json!({"query": "linear"}))
        .await
        .unwrap_err();
    assert_eq!(e.code, codes::UNAVAILABLE);
}

async fn icon_of(c: &agenthost_cli::Client, id: &str) -> Option<String> {
    let deadline = tokio::time::Instant::now() + T;
    loop {
        let list: Vec<Connector> = c.call(m::CONNECTOR_LIST, json!({})).await.unwrap();
        let icon = list.into_iter().find(|x| x.connector_id == id).unwrap().icon;
        if icon.is_some() || tokio::time::Instant::now() > deadline {
            return icon;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn connectors_get_their_sites_icon() {
    let cat = catalog().await;
    let favicons = format!("{}/favicons", cat.base);
    let th = TestHost::start_with(|c| c.favicon_url = Some(favicons.clone())).await;
    let (c, mut n) = th.client().await;
    // `.invalid` never resolves, so the connector itself just errors.
    let conn: Connector = c
        .call(m::CONNECTOR_ADD, json!({"url": "https://mcp.notes.invalid/mcp", "name": "Notes"}))
        .await
        .unwrap();
    let icon = icon_of(&c, &conn.connector_id).await.expect("an icon");
    assert!(icon.starts_with("data:image/png;base64,"));
    // For the site (the last two labels of the host), at 64 px.
    assert!(
        cat.seen.lock().unwrap().iter().any(|p| p == "/favicons?domain=notes.invalid&sz=64"),
        "{:?}",
        cat.seen.lock().unwrap()
    );
    // Clients heard about it.
    let deadline = tokio::time::Instant::now() + T;
    loop {
        let note = tokio::time::timeout_at(deadline, n.recv()).await.unwrap().unwrap();
        if note.method == m::N_CONNECTOR_CHANGED && note.params["connector_id"] == conn.connector_id.as_str() {
            break;
        }
    }

    // A connector added while icons were off gets one at the next start.
    drop(c);
    let (dir, mut cfg) = th.stop().await;
    cfg.favicon_url = None;
    let th = TestHost::start_in(dir, cfg).await;
    let (c, _n) = th.client().await;
    let later: Connector = c
        .call(m::CONNECTOR_ADD, json!({"url": "https://mcp.later.invalid/mcp", "name": "Later"}))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let list: Vec<Connector> = c.call(m::CONNECTOR_LIST, json!({})).await.unwrap();
    assert_eq!(list.iter().find(|x| x.connector_id == later.connector_id).unwrap().icon, None);
    drop(c);
    let (dir, mut cfg) = th.stop().await;
    cfg.favicon_url = Some(favicons);
    let th = TestHost::start_in(dir, cfg).await;
    let (c, _n) = th.client().await;
    assert!(icon_of(&c, &later.connector_id).await.is_some());
    // Kept across restarts.
    assert!(icon_of(&c, &conn.connector_id).await.is_some());
}

#[tokio::test]
async fn renaming_keeps_live_sessions_working_and_refuses_clashes() {
    let server = mcp_server::start(None).await;
    let th = TestHost::start().await;
    let (c, mut n) = th.client().await;
    let ws = th.workspace_id(&c).await;
    let probe: Connector = c
        .call(m::CONNECTOR_ADD, json!({"url": server.url, "name": "Probe"}))
        .await
        .unwrap();
    let other: Connector = c
        .call(m::CONNECTOR_ADD, json!({"url": "https://mcp.other.invalid/mcp", "name": "Other"}))
        .await
        .unwrap();
    let s = c.create_session(&ws, AgentKind::Fake).await.unwrap();
    wait_state(&c, &s.session_id, SessionState::Idle, T).await;
    c.subscribe(&s.session_id, 0).await.unwrap();

    // A name whose tool prefix another connector (or the host) already uses.
    for name in ["probe!", "PROBE", "AgentHost"] {
        let e = c
            .request(m::CONNECTOR_UPDATE, json!({"connector_id": other.connector_id, "name": name}))
            .await
            .unwrap_err();
        assert_eq!(e.code, codes::INVALID_PARAMS, "{name}");
    }
    // Probe itself may change case or punctuation.
    let p: Connector = c
        .call(m::CONNECTOR_UPDATE, json!({"connector_id": probe.connector_id, "name": "Probe!"}))
        .await
        .unwrap();
    assert_eq!(p.name, "Probe!");

    // Renamed while the session runs: it listed `probe__echo` and keeps
    // calling it so; new lists show the new prefix.
    let r: Connector = c
        .call(m::CONNECTOR_UPDATE, json!({"connector_id": probe.connector_id, "name": "Tester"}))
        .await
        .unwrap();
    assert_eq!(r.name, "Tester");
    for tool in ["probe__echo", "tester__echo"] {
        c.prompt(&s.session_id, &format!(r#"mcp {tool} {{"text":"hi"}}"#)).await.unwrap();
        let (ev, _) = collect_until(&mut n, T, turn_end).await;
        let res = ev.iter().find(|e| e.kind == EventType::ToolResult).expect("tool.result");
        assert_eq!(res.payload["status"], "completed", "{tool}: {:?}", res.payload);
        assert_eq!(res.payload["output"], "echo: hi");
    }
    // Both calls reached the server under the tool's own name.
    assert_eq!(server.state.calls.lock().unwrap().iter().filter(|(t, _)| t == "echo").count(), 2);
    c.prompt(&s.session_id, "mcp-list").await.unwrap();
    let (ev, _) = collect_until(&mut n, T, turn_end).await;
    let msg = ev.iter().find(|e| e.kind == EventType::AgentMessage).unwrap().payload["text"].to_string();
    assert!(msg.contains("tester__echo") && !msg.contains("probe__echo"), "{msg}");
    // The old prefix is free again for another connector.
    let o: Connector = c
        .call(m::CONNECTOR_UPDATE, json!({"connector_id": other.connector_id, "name": "Probe"}))
        .await
        .unwrap();
    assert_eq!(o.name, "Probe");
}
