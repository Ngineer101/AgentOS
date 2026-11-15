//! Milestone 3: sandbox escape tests. The fake agent runs inside the real
//! sandbox (srt: bubblewrap on Linux, Seatbelt on macOS) and tries to get out.
//!
//! Needs the pinned sandbox runtime: set AGENTHOST_TEST_TOOLS to a tools dir
//! made by `agenthostd --data-dir <dir> install-tools` (the tools dir is
//! `<dir>/tools`). Without it these tests print a notice and pass vacuously;
//! CI sets it so they always run there.

mod common;

use agenthost_cli::Client;
use agenthost_proto::methods as m;
use agenthost_proto::*;
use common::*;
use serde_json::json;
use std::path::PathBuf;
use std::time::Duration;

const T: Duration = Duration::from_secs(60);

fn tools() -> Option<PathBuf> {
    let p = PathBuf::from(std::env::var_os("AGENTHOST_TEST_TOOLS")?);
    p.join("node_modules/@anthropic-ai/sandbox-runtime/dist/cli.js")
        .is_file()
        .then_some(p)
}

struct Sbx {
    th: TestHost,
    c: Client,
    n: tokio::sync::mpsc::Receiver<agenthost_proto::jsonrpc::Notification>,
    sid: String,
    ws: String,
    home: PathBuf,
}

async fn sandboxed() -> Option<Sbx> {
    let Some(tools) = tools() else {
        eprintln!("SKIP: set AGENTHOST_TEST_TOOLS to run sandbox escape tests");
        return None;
    };
    let th = TestHost::start_with(|c| {
        c.fake_agent_unsandboxed = false;
        c.tools_dir = tools;
    })
    .await;
    let home = th.host.ctx.cfg.home_dir.clone();
    // Things that must stay unreadable.
    std::fs::create_dir_all(home.join(".ssh")).unwrap();
    std::fs::write(home.join(".ssh/id_test"), "SSH-PRIVATE-KEY").unwrap();
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::write(home.join(".claude/.credentials.json"), "CLAUDE-TOKEN").unwrap();
    std::fs::create_dir_all(home.join(".aws")).unwrap();
    std::fs::write(home.join(".aws/credentials"), "AWS-SECRET").unwrap();
    let (c, n) = th.client().await;
    let ws = th.workspace_id(&c).await;
    let s = c.create_session(&ws, AgentKind::Fake).await.unwrap();
    let s = wait_state(&c, &s.session_id, SessionState::Idle, T).await;
    c.subscribe(&s.session_id, 0).await.unwrap();
    Some(Sbx {
        th,
        c,
        n,
        sid: s.session_id,
        ws,
        home,
    })
}

impl Sbx {
    /// Run a shell command in the sandbox; returns (output, events of the turn).
    async fn sh(&mut self, cmd: &str) -> (String, Vec<Event>) {
        self.c
            .prompt(&self.sid, &format!("sh {cmd}"))
            .await
            .unwrap();
        let (ev, _) = collect_until(&mut self.n, T, turn_end).await;
        let out = ev
            .iter()
            .find(|e| e.kind == EventType::ToolResult)
            .map(|e| e.payload["output"].as_str().unwrap_or("").to_string())
            .unwrap_or_default();
        (out, ev)
    }
}

#[tokio::test]
async fn escape_suite() {
    let Some(mut s) = sandboxed().await else {
        return;
    };
    let data = s.th.host.ctx.cfg.data_dir.clone();
    let runtime = s.th.host.ctx.cfg.runtime_dir.clone();
    let mut failures = vec![];
    let mut check = |name: &str, ok: bool, detail: String| {
        eprintln!("{} {name}", if ok { "PASS" } else { "FAIL" });
        if !ok {
            eprintln!("    output: {detail}");
        }
        if !ok {
            failures.push(format!("{name}: {detail}"));
        }
    };

    // 1. Denied reads: secrets folders.
    let (o, _) = s
        .sh(&format!(
            "cat {}/.ssh/id_test {}/.aws/credentials",
            s.home.display(),
            s.home.display()
        ))
        .await;
    check(
        "1 secrets folders are unreadable",
        !o.contains("SSH-PRIVATE-KEY") && !o.contains("AWS-SECRET"),
        o,
    );

    // 2. Other agents' credentials.
    let (o, _) = s
        .sh(&format!(
            "cat {}/.claude/.credentials.json",
            s.home.display()
        ))
        .await;
    check(
        "2 other agents' credentials are unreadable",
        !o.contains("CLAUDE-TOKEN"),
        o,
    );

    // 3. The host's own data (database, secrets, logs).
    let (o, _) = s
        .sh(&format!(
            "ls {} ; head -c 100 {}/host.db",
            data.display(),
            data.display()
        ))
        .await;
    check(
        "3 host data is unreadable",
        !o.contains("SQLite format") && !o.contains("logs\n"),
        o,
    );

    // 4. Writes outside the workspace.
    let outside = s.home.join("escaped.txt");
    let (o, _) = s
        .sh(&format!(
            "echo x > {} ; echo y > /tmp/ah-escape-$$ ; ls /tmp/ah-escape-* 2>&1",
            outside.display()
        ))
        .await;
    check(
        "4 writes outside the workspace fail",
        !outside.exists() && (o.contains("No such file") || !o.contains("/tmp/ah-escape-")),
        o,
    );

    // 5. Writes inside the workspace work.
    let (o, _) = s.sh("echo ok > inside.txt && cat inside.txt").await;
    check(
        "5 writes inside the workspace work",
        s.th.workspace.join("inside.txt").exists(),
        o,
    );

    // 6. An unlisted host through the proxy is refused and logged.
    let (o, ev) = s
        .sh("curl -sS -m 10 -o /dev/null -w '%{http_code}' https://example.com")
        .await;
    let denied = ev
        .iter()
        .any(|e| e.kind == EventType::NetDenied && e.payload["host"] == "example.com");
    check(
        "6 unlisted host is refused with net.denied",
        denied && !o.contains("200"),
        o,
    );

    // 7. Proxy bypass: direct connections fail.
    let (o, _) = s.sh("curl -sS -m 5 --noproxy '*' -o /dev/null -w '%{http_code}' https://1.1.1.1 ; echo; curl -sS -m 5 --noproxy '*' http://example.com -o /dev/null -w '%{http_code}'").await;
    check(
        "7 direct connections (proxy bypass) fail",
        !o.contains("200") && !o.contains("301"),
        o,
    );

    // 8. Host loopback: a service on the host's 127.0.0.1 is unreachable, directly or via the proxy.
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut c, _)) = l.accept().await {
            use tokio::io::AsyncWriteExt;
            let _ = c
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nLEAKED")
                .await;
        }
    });
    let (o, _) = s.sh(&format!("curl -sS -m 5 http://127.0.0.1:{port}/ ; curl -sS -m 5 --noproxy '*' http://127.0.0.1:{port}/ ; curl -sS -m 5 http://localhost:{port}/")).await;
    check("8 host loopback is unreachable", !o.contains("LEAKED"), o);

    // 9. Private addresses stay denied even when explicitly listed (forced through the proxy).
    s.c.request(
        m::WORKSPACE_UPDATE,
        json!({"workspace_id": s.ws, "add_custom_host": "127.0.0.1"}),
    )
    .await
    .unwrap();
    let (o, ev) = s.sh(&format!("curl -sS -m 5 --noproxy '' http://127.0.0.1:{port}/ ; curl -sS -m 5 http://192.168.1.1/ -o /dev/null -w '%{{http_code}}'")).await;
    let private = ev.iter().any(|e| {
        e.kind == EventType::NetDenied
            && e.payload["reason"]
                .as_str()
                .unwrap_or("")
                .contains("private")
    });
    check(
        "9 loopback/LAN addresses are denied even when listed",
        !o.contains("LEAKED") && private,
        o,
    );

    // 10. No direct DNS resolution inside the sandbox.
    let (o, _) = s.sh("getent hosts example.com || echo NO-DNS; (exec 3<>/dev/udp/1.1.1.1/53 && echo UDP-OK) 2>&1 || echo NO-UDP").await;
    check(
        "10 no direct DNS",
        o.contains("NO-DNS") && !o.contains("UDP-OK"),
        o,
    );

    // 11. Another session's temp folder and the runtime folder are unreadable.
    std::fs::create_dir_all(runtime.join("sother")).unwrap();
    std::fs::write(runtime.join("sother/secret.txt"), "OTHER-SESSION").unwrap();
    let (o, _) = s
        .sh(&format!(
            "cat {}/sother/secret.txt; ls {}",
            runtime.display(),
            runtime.display()
        ))
        .await;
    check(
        "11 other sessions' temp folders are unreadable",
        !o.contains("OTHER-SESSION") && !o.contains("gw.sock"),
        o,
    );

    // 12. Nothing listens: a server inside the sandbox is not reachable from the host.
    let (o, _) = s.sh("timeout 2 sh -c 'nc -l 127.0.0.1 47123 || python3 -m http.server 47123 --bind 127.0.0.1' >/dev/null 2>&1 & sleep 1; echo started").await;
    let reach = tokio::time::timeout(
        Duration::from_secs(1),
        tokio::net::TcpStream::connect("127.0.0.1:47123"),
    )
    .await
    .map(|r| r.is_ok())
    .unwrap_or(false);
    check(
        "12 sandbox listeners are not reachable from outside",
        !reach,
        o,
    );

    // 13. Unix sockets: the host's client socket is unreachable.
    let sock = s.th.host.ctx.cfg.socket_path.clone();
    let (o, _) = s.sh(&format!("python3 -c \"import socket;s=socket.socket(socket.AF_UNIX);s.connect('{}');print('CONNECTED')\" 2>&1 | tail -1", sock.display())).await;
    check(
        "13 the host socket is unreachable",
        !o.contains("CONNECTED"),
        o,
    );

    let _ = s.th.host.ctx.cfg.clone();
    assert!(
        failures.is_empty(),
        "escape tests failed:\n{}",
        failures.join("\n")
    );
}

#[tokio::test]
async fn profile_change_applies_without_restart() {
    let Some(mut s) = sandboxed().await else {
        return;
    };
    let (o, _) = s
        .sh("curl -sS -m 15 -o /dev/null -w '%{http_code}' https://registry.npmjs.org/")
        .await;
    assert!(!o.contains("200"), "registry reachable on agent-only: {o}");
    s.c.request(
        m::WORKSPACE_UPDATE,
        json!({"workspace_id": s.ws, "network": {"presets": ["packages"], "custom_hosts": []}}),
    )
    .await
    .unwrap();
    let (o, _) = s
        .sh("curl -sS -m 15 -o /dev/null -w '%{http_code}' https://registry.npmjs.org/")
        .await;
    assert!(
        o.contains("200"),
        "registry not reachable after switching to packages: {o}"
    );
}
