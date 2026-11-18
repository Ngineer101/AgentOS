//! Egress proxy: the only network route out of a sandbox.
//!
//! One proxy per session, with an HTTP listener (CONNECT and absolute-form
//! requests) and a SOCKS5 listener, both on 127.0.0.1 ephemeral ports that
//! the sandbox runtime bridges into the sandbox. The proxy:
//!
//! - allows only the session's hosts, re-read on every connection so a
//!   profile change applies without restarting anything;
//! - refuses IP literals and resolves names itself, dropping loopback,
//!   private, link-local and other non-public addresses;
//! - routes the reserved internal name of the MCP gateway to the gateway's
//!   Unix socket, which nothing else can reach;
//! - on Linux, refuses connections from sockets owned by another user;
//! - reports every refusal so it becomes a `net.denied` event.

use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UnixStream};
use tokio::task::JoinHandle;

/// The reserved name agents use to reach the MCP gateway.
pub const GATEWAY_HOST: &str = "mcp.agenthost.internal";

pub trait ProxyHooks: Send + Sync + 'static {
    /// The hosts allowed right now.
    fn allowed_hosts(&self) -> Vec<String>;
    /// A connection was refused.
    fn on_denied(&self, host: &str, port: u16, reason: &str);
    /// Unix socket of the MCP gateway, if one serves this session.
    fn gateway_socket(&self) -> Option<PathBuf>;
}

/// Where the proxy is inside the sandbox, as a base URL for the gateway
/// (`{base}/s/<token>/mcp`). On Linux the sandbox runtime bridges it to a
/// fixed port; on macOS the sandbox shares the host's loopback.
pub fn direct_gateway_base(http_port: u16) -> String {
    if cfg!(target_os = "linux") {
        "http://127.0.0.1:3128".into()
    } else {
        format!("http://127.0.0.1:{http_port}")
    }
}

pub struct EgressProxy {
    pub http_port: u16,
    pub socks_port: u16,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for EgressProxy {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

struct Shared {
    hooks: Arc<dyn ProxyHooks>,
    recent: Mutex<HashMap<(String, u16), Instant>>,
    listen: [SocketAddr; 2],
}

impl Shared {
    fn deny(&self, host: &str, port: u16, reason: &str) {
        let key = (host.to_string(), port);
        {
            let mut r = self.recent.lock().unwrap();
            let now = Instant::now();
            if let Some(t) = r.get(&key) {
                if now.duration_since(*t) < Duration::from_secs(2) {
                    return;
                }
            }
            r.insert(key, now);
        }
        tracing::info!(host, port, reason, "egress denied");
        self.hooks.on_denied(host, port, reason);
    }
}

impl EgressProxy {
    pub async fn start(hooks: Arc<dyn ProxyHooks>) -> Result<EgressProxy> {
        let http = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let socks = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let shared = Arc::new(Shared {
            hooks,
            recent: Mutex::new(HashMap::new()),
            listen: [http.local_addr()?, socks.local_addr()?],
        });
        let http_port = shared.listen[0].port();
        let socks_port = shared.listen[1].port();
        let s1 = shared.clone();
        let t1 = tokio::spawn(async move {
            while let Ok((stream, peer)) = http.accept().await {
                let s = s1.clone();
                tokio::spawn(async move {
                    if !same_user(peer, s.listen[0]) {
                        tracing::warn!(%peer, "egress proxy: connection from another user refused");
                        return;
                    }
                    if let Err(e) = handle_http(stream, &s).await {
                        tracing::debug!("http proxy connection: {e}");
                    }
                });
            }
        });
        let s2 = shared.clone();
        let t2 = tokio::spawn(async move {
            while let Ok((stream, peer)) = socks.accept().await {
                let s = s2.clone();
                tokio::spawn(async move {
                    if !same_user(peer, s.listen[1]) {
                        tracing::warn!(%peer, "egress proxy: connection from another user refused");
                        return;
                    }
                    if let Err(e) = handle_socks(stream, &s).await {
                        tracing::debug!("socks proxy connection: {e}");
                    }
                });
            }
        });
        Ok(EgressProxy {
            http_port,
            socks_port,
            tasks: vec![t1, t2],
        })
    }
}

// ---------------------------------------------------------------------------
// Policy
// ---------------------------------------------------------------------------

/// The allowlist entry for "any public host" (the `web` preset). Users can't
/// type it as a custom host; addresses are still checked by [`ip_is_denied`].
pub const ANY_PUBLIC_HOST: &str = "*";

/// Whether `host:port` matches an allowlist entry (`name`, `*.suffix`, optional
/// `:port`, or [`ANY_PUBLIC_HOST`]).
pub fn host_allowed(allowed: &[String], host: &str, port: u16) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    allowed.iter().any(|entry| {
        if entry == ANY_PUBLIC_HOST {
            return true;
        }
        let entry = entry.trim().to_ascii_lowercase();
        let (pat, pport) = match entry.rsplit_once(':') {
            Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !h.contains(':') => {
                (h.to_string(), p.parse::<u16>().ok())
            }
            _ => (entry.clone(), None),
        };
        if let Some(p) = pport {
            if p != port {
                return false;
            }
        }
        if let Some(suffix) = pat.strip_prefix("*.") {
            host.len() > suffix.len()
                && host.ends_with(suffix)
                && host.as_bytes()[host.len() - suffix.len() - 1] == b'.'
        } else {
            host == pat
        }
    })
}

/// Addresses a sandbox must never reach, even through an allowed name.
pub fn ip_is_denied(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4_denied(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return v4_denied(v4);
            }
            let seg = v6.segments();
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (seg[0] & 0xfe00) == 0xfc00 // unique local
                || (seg[0] & 0xffc0) == 0xfe80 // link local
                || (seg[0] == 0x64 && seg[1] == 0xff9b) // NAT64 embeds v4
                || seg[0] == 0x2002 // 6to4 embeds v4
                || v6 == Ipv6Addr::new(0xfd00, 0xec2, 0, 0, 0, 0, 0, 0x254)
        }
    }
}

fn v4_denied(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        || o[0] == 0
        || (o[0] == 100 && (o[1] & 0xc0) == 64) // CGNAT 100.64/10
        || (o[0] == 192 && o[1] == 0 && o[2] == 0) // IETF protocol assignments
        || o[0] >= 240
        || ip == Ipv4Addr::new(100, 100, 100, 200)
        || ip == Ipv4Addr::new(168, 63, 129, 16)
}

enum Target {
    Remote(TcpStream),
    Gateway(UnixStream),
}

/// Decide and connect, reporting denials.
async fn connect(shared: &Shared, host: &str, port: u16) -> Result<Target, String> {
    let host_l = host.trim_end_matches('.').to_ascii_lowercase();
    if host_l == GATEWAY_HOST {
        let Some(sock) = shared.hooks.gateway_socket() else {
            shared.deny(host, port, "no MCP gateway for this session");
            return Err("no gateway".into());
        };
        return UnixStream::connect(sock)
            .await
            .map(Target::Gateway)
            .map_err(|e| e.to_string());
    }
    let literal = host_l
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
        .ok();
    let allowed = shared.hooks.allowed_hosts();
    if !host_allowed(&allowed, &host_l, port) {
        let reason = if literal.is_some() {
            "IP addresses are not on the allow list"
        } else {
            "host is not on the allow list"
        };
        shared.deny(host, port, reason);
        return Err(reason.into());
    }
    let addrs: Vec<SocketAddr> = match literal {
        Some(ip) => vec![SocketAddr::new(ip, port)],
        None => match tokio::net::lookup_host((host_l.as_str(), port)).await {
            Ok(a) => a.collect(),
            Err(e) => {
                shared.deny(host, port, "name does not resolve");
                return Err(e.to_string());
            }
        },
    };
    let public: Vec<SocketAddr> = addrs
        .into_iter()
        .filter(|a| !ip_is_denied(a.ip()))
        .collect();
    if public.is_empty() {
        shared.deny(host, port, "resolves to a private or local address");
        return Err("private address".into());
    }
    let mut last = String::new();
    for a in public {
        match tokio::time::timeout(Duration::from_secs(15), TcpStream::connect(a)).await {
            Ok(Ok(s)) => return Ok(Target::Remote(s)),
            Ok(Err(e)) => last = e.to_string(),
            Err(_) => last = "connect timed out".into(),
        }
    }
    Err(last)
}

async fn splice<C: AsyncRead + AsyncWrite + Unpin>(
    mut client: C,
    target: Target,
    prefix: &[u8],
) -> Result<()> {
    match target {
        Target::Remote(mut s) => {
            s.write_all(prefix).await?;
            let _ = tokio::io::copy_bidirectional(&mut client, &mut s).await;
        }
        Target::Gateway(mut s) => {
            s.write_all(prefix).await?;
            let _ = tokio::io::copy_bidirectional(&mut client, &mut s).await;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

async fn read_head(stream: &mut TcpStream) -> Result<(Vec<u8>, Vec<u8>)> {
    let mut buf = Vec::with_capacity(4096);
    let mut tmp = [0u8; 4096];
    loop {
        let n = tokio::time::timeout(Duration::from_secs(30), stream.read(&mut tmp)).await??;
        if n == 0 {
            return Err(anyhow!("closed before request head"));
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let rest = buf.split_off(pos + 4);
            return Ok((buf, rest));
        }
        if buf.len() > 64 * 1024 {
            return Err(anyhow!("request head too large"));
        }
    }
}

fn split_host_port(authority: &str, default_port: u16) -> (String, u16) {
    if let Some(rest) = authority.strip_prefix('[') {
        if let Some((h, p)) = rest.split_once(']') {
            let port = p
                .strip_prefix(':')
                .and_then(|p| p.parse().ok())
                .unwrap_or(default_port);
            return (h.to_string(), port);
        }
    }
    match authority.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty() => {
            (h.to_string(), p.parse().unwrap_or(default_port))
        }
        _ => (authority.to_string(), default_port),
    }
}

async fn respond(stream: &mut TcpStream, status: &str, reason: &str) {
    let body = format!("agenthost egress proxy: {reason}\n");
    let msg = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nX-Proxy-Error: blocked-by-agenthost\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(msg.as_bytes()).await;
}

async fn handle_http(mut stream: TcpStream, shared: &Shared) -> Result<()> {
    let (head, rest) = read_head(&mut stream).await?;
    let text = String::from_utf8_lossy(&head).to_string();
    let mut lines = text.split("\r\n");
    let request_line = lines.next().unwrap_or_default().to_string();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();
    let version = parts.next().unwrap_or("HTTP/1.1").to_string();

    if method.eq_ignore_ascii_case("CONNECT") {
        let (host, port) = split_host_port(&target, 443);
        match connect(shared, &host, port).await {
            Ok(t) => {
                stream
                    .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                    .await?;
                splice(stream, t, &rest).await?;
            }
            Err(reason) => respond(&mut stream, "403 Forbidden", &reason).await,
        }
        return Ok(());
    }

    // Origin-form request made to the proxy itself: the MCP gateway, for
    // agents whose MCP client ignores HTTP_PROXY (see `DIRECT_GATEWAY_URL`).
    let direct;
    let target = if target.starts_with("/s/") {
        direct = format!("http://{GATEWAY_HOST}{target}");
        direct.as_str()
    } else {
        target.as_str()
    };
    // Absolute-form request: http://host[:port]/path
    let Some(after) = target.strip_prefix("http://") else {
        respond(
            &mut stream,
            "400 Bad Request",
            "only absolute http:// URLs and CONNECT are supported",
        )
        .await;
        return Ok(());
    };
    let (authority, path) = match after.find('/') {
        Some(i) => (&after[..i], &after[i..]),
        None => (after, "/"),
    };
    let (host, port) = split_host_port(authority, 80);
    let t = match connect(shared, &host, port).await {
        Ok(t) => t,
        Err(reason) => {
            respond(&mut stream, "403 Forbidden", &reason).await;
            return Ok(());
        }
    };
    // Rewrite to origin form, one request per connection: a later request on
    // the same client connection must not reach this origin unchecked.
    let mut out = format!("{method} {path} {version}\r\n");
    for l in lines {
        if l.is_empty() {
            continue;
        }
        let lower = l.to_ascii_lowercase();
        if lower.starts_with("proxy-")
            || lower.starts_with("connection:")
            || lower.starts_with("keep-alive:")
        {
            continue;
        }
        out.push_str(l);
        out.push_str("\r\n");
    }
    out.push_str("Connection: close\r\n\r\n");
    let mut prefix = out.into_bytes();
    prefix.extend_from_slice(&rest);
    splice(stream, t, &prefix).await
}

// ---------------------------------------------------------------------------
// SOCKS5 (no auth, CONNECT only)
// ---------------------------------------------------------------------------

async fn handle_socks(mut s: TcpStream, shared: &Shared) -> Result<()> {
    let mut hdr = [0u8; 2];
    s.read_exact(&mut hdr).await?;
    if hdr[0] != 5 {
        return Err(anyhow!("not SOCKS5"));
    }
    let mut methods = vec![0u8; hdr[1] as usize];
    s.read_exact(&mut methods).await?;
    if !methods.contains(&0) {
        s.write_all(&[5, 0xff]).await?;
        return Ok(());
    }
    s.write_all(&[5, 0]).await?;
    let mut req = [0u8; 4];
    s.read_exact(&mut req).await?;
    let host = match req[3] {
        1 => {
            let mut a = [0u8; 4];
            s.read_exact(&mut a).await?;
            Ipv4Addr::from(a).to_string()
        }
        3 => {
            let mut l = [0u8; 1];
            s.read_exact(&mut l).await?;
            let mut name = vec![0u8; l[0] as usize];
            s.read_exact(&mut name).await?;
            String::from_utf8_lossy(&name).to_string()
        }
        4 => {
            let mut a = [0u8; 16];
            s.read_exact(&mut a).await?;
            Ipv6Addr::from(a).to_string()
        }
        _ => return Err(anyhow!("bad address type")),
    };
    let mut p = [0u8; 2];
    s.read_exact(&mut p).await?;
    let port = u16::from_be_bytes(p);
    if req[1] != 1 {
        // Only CONNECT; BIND and UDP ASSOCIATE are refused.
        s.write_all(&[5, 7, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
        return Ok(());
    }
    match connect(shared, &host, port).await {
        Ok(t) => {
            s.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
            splice(s, t, &[]).await
        }
        Err(_) => {
            s.write_all(&[5, 2, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Peer user check
// ---------------------------------------------------------------------------

/// On Linux, check that the connecting socket belongs to our own user by
/// finding it in /proc/net/tcp. Elsewhere, loopback-only binding is all we have.
fn same_user(peer: SocketAddr, listen: SocketAddr) -> bool {
    #[cfg(target_os = "linux")]
    {
        let uid = unsafe { libc::getuid() };
        match socket_owner(peer, listen) {
            Some(owner) => owner == uid,
            // Not found (raced with close): refuse.
            None => false,
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (peer, listen);
        true
    }
}

#[cfg(target_os = "linux")]
fn socket_owner(peer: SocketAddr, listen: SocketAddr) -> Option<u32> {
    let (SocketAddr::V4(p), SocketAddr::V4(l)) = (peer, listen) else {
        return None;
    };
    let enc = |a: &std::net::SocketAddrV4| {
        format!(
            "{:08X}:{:04X}",
            u32::from_le_bytes(a.ip().octets()),
            a.port()
        )
    };
    let want_local = enc(&p);
    let want_remote = enc(&l);
    let table = std::fs::read_to_string("/proc/net/tcp").ok()?;
    for line in table.lines().skip(1) {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() > 7 && cols[1] == want_local && cols[2] == want_remote {
            return cols[7].parse().ok();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_matching() {
        let a = vec![
            "chatgpt.com".to_string(),
            "*.github.com".to_string(),
            "example.org:8443".to_string(),
        ];
        assert!(host_allowed(&a, "chatgpt.com", 443));
        assert!(host_allowed(&a, "ChatGPT.com.", 443));
        assert!(!host_allowed(&a, "ab.chatgpt.com", 443));
        assert!(host_allowed(&a, "api.github.com", 443));
        assert!(!host_allowed(&a, "github.com", 443));
        assert!(!host_allowed(&a, "evilgithub.com", 443));
        assert!(host_allowed(&a, "example.org", 8443));
        assert!(!host_allowed(&a, "example.org", 443));
    }

    #[test]
    fn open_web_allows_any_name_but_no_private_address() {
        let open = vec![ANY_PUBLIC_HOST.to_string()];
        assert!(host_allowed(&open, "example.org", 443));
        assert!(host_allowed(&open, "en.wikipedia.org", 80));
        // Names are allowed; where they resolve is checked separately.
        assert!(ip_is_denied("10.0.0.1".parse().unwrap()));
        assert!(ip_is_denied("127.0.0.1".parse().unwrap()));
        assert!(!host_allowed(&["example.org".to_string()], "other.org", 443));
    }

    #[test]
    fn private_addresses_denied() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "192.168.1.1",
            "172.16.0.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fe80::1",
            "fd00::1",
            "::ffff:192.168.0.1",
            "224.0.0.1",
        ] {
            assert!(ip_is_denied(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["1.1.1.1", "140.82.112.3", "2606:4700::1111"] {
            assert!(!ip_is_denied(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn host_port_split() {
        assert_eq!(split_host_port("a.com:8080", 80), ("a.com".into(), 8080));
        assert_eq!(split_host_port("a.com", 443), ("a.com".into(), 443));
        assert_eq!(split_host_port("[::1]:22", 443), ("::1".into(), 22));
    }
}
