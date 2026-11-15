//! A small remote MCP server (streamable HTTP) for gateway tests.

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, StatusCode};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct TestMcpState {
    pub calls: Mutex<Vec<(String, Value)>>,
    pub auth_headers: Mutex<Vec<String>>,
    pub inits: AtomicUsize,
}

pub struct TestMcp {
    pub url: String,
    pub state: Arc<TestMcpState>,
}

pub async fn start(token: Option<&str>) -> TestMcp {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://127.0.0.1:{}/mcp",
        listener.local_addr().unwrap().port()
    );
    let state = Arc::new(TestMcpState::default());
    let st = state.clone();
    let token = token.map(|t| format!("Bearer {t}"));
    tokio::spawn(async move {
        loop {
            let Ok((s, _)) = listener.accept().await else {
                break;
            };
            let st = st.clone();
            let token = token.clone();
            tokio::spawn(async move {
                let svc = hyper::service::service_fn(move |req| {
                    let st = st.clone();
                    let token = token.clone();
                    async move { Ok::<_, std::convert::Infallible>(handle(req, st, token).await) }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(hyper_util::rt::TokioIo::new(s), svc)
                    .await;
            });
        }
    });
    TestMcp { url, state }
}

async fn handle(
    req: Request<hyper::body::Incoming>,
    st: Arc<TestMcpState>,
    token: Option<String>,
) -> Response<Full<Bytes>> {
    let auth = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    st.auth_headers.lock().unwrap().push(auth.clone());
    if let Some(t) = &token {
        if &auth != t {
            return Response::builder()
                .status(StatusCode::UNAUTHORIZED)
                .body(Full::new(Bytes::new()))
                .unwrap();
        }
    }
    let body = req.into_body().collect().await.unwrap().to_bytes();
    let msg: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
    let id = msg.get("id").cloned();
    let method = msg["method"].as_str().unwrap_or("");
    let Some(id) = id.filter(|v| !v.is_null()) else {
        return Response::builder()
            .status(StatusCode::ACCEPTED)
            .body(Full::new(Bytes::new()))
            .unwrap();
    };
    let result = match method {
        "initialize" => {
            st.inits.fetch_add(1, Ordering::SeqCst);
            json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"test","version":"1"}})
        }
        "tools/list" => json!({"tools": [
            {"name":"echo","description":"Echo text back","inputSchema":{"type":"object","properties":{"text":{"type":"string"}}},"annotations":{"readOnlyHint":true}},
            {"name":"create_note","description":"Create a note","inputSchema":{"type":"object","properties":{"text":{"type":"string"}}}}
        ]}),
        "tools/call" => {
            let name = msg["params"]["name"].as_str().unwrap_or("").to_string();
            let args = msg["params"]["arguments"].clone();
            st.calls.lock().unwrap().push((name.clone(), args.clone()));
            let text = match name.as_str() {
                "echo" => format!("echo: {}", args["text"].as_str().unwrap_or("")),
                "create_note" => format!("created note: {}", args["text"].as_str().unwrap_or("")),
                _ => "unknown".into(),
            };
            // Answer tools/call as an event stream to exercise SSE parsing.
            let resp =
                json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":text}]}});
            return Response::builder()
                .header("content-type", "text/event-stream")
                .header("mcp-session-id", "test-session")
                .body(Full::new(Bytes::from(format!(
                    "event: message\ndata: {resp}\n\n"
                ))))
                .unwrap();
        }
        _ => {
            return Response::builder()
                .header("content-type", "application/json")
                .body(Full::new(Bytes::from(
                    json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"nope"}})
                        .to_string(),
                )))
                .unwrap()
        }
    };
    Response::builder()
        .header("content-type", "application/json")
        .header("mcp-session-id", "test-session")
        .body(Full::new(Bytes::from(
            json!({"jsonrpc":"2.0","id":id,"result":result}).to_string(),
        )))
        .unwrap()
}
