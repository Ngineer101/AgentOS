//! The integrations.sh catalog (`connector.search`) and connector icons.
//!
//! Both are plain HTTPS fetches by the host. Their base URLs come from
//! [`HostConfig`](crate::config::HostConfig), so tests point them at a local
//! server or turn them off.

use agenthost_proto::ConnectorSuggestion;
use anyhow::{bail, Context, Result};
use base64::Engine;
use serde_json::Value;
use std::time::Duration;

/// At most this many suggestions per search.
pub const MAX_SUGGESTIONS: usize = 20;
/// Icons larger than this are left out.
pub const MAX_ICON_BYTES: usize = 64 * 1024;
const SEARCH_TIMEOUT: Duration = Duration::from_secs(10);
const ICON_TIMEOUT: Duration = Duration::from_secs(10);

/// Loopback, where tests serve over plain http.
fn is_loopback(u: &url::Url) -> bool {
    matches!(u.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
}

/// Search the catalog for MCP servers; each comes with its icon when it
/// could be fetched.
pub async fn search(http: &reqwest::Client, base: &str, query: &str) -> Result<Vec<ConnectorSuggestion>> {
    let mut u = url::Url::parse(base)
        .with_context(|| format!("bad catalog address {base}"))?
        .join("api/search")?;
    u.query_pairs_mut().append_pair("q", query.trim());
    let r = http
        .get(u)
        .timeout(SEARCH_TIMEOUT)
        .send()
        .await
        .context("integrations.sh did not answer; check your connection")?;
    if !r.status().is_success() {
        bail!("integrations.sh search failed (HTTP {})", r.status());
    }
    let body: Value = r.json().await.context("integrations.sh sent something other than JSON")?;
    let mut out = parse_search(&body);
    let icons = futures::future::join_all(out.iter().map(|s| {
        let icon_url = s.icon.clone();
        async move {
            match icon_url {
                Some(u) => fetch_icon(http, &u).await,
                None => None,
            }
        }
    }))
    .await;
    for (s, icon) in out.iter_mut().zip(icons) {
        s.icon = icon;
    }
    Ok(out)
}

/// The suggestions in a search response: each result's MCP surfaces, with
/// `icon` still the icon's address.
pub fn parse_search(body: &Value) -> Vec<ConnectorSuggestion> {
    let mut out = vec![];
    for r in body["results"].as_array().into_iter().flatten() {
        let domain = r["domain"].as_str().unwrap_or_default();
        for s in r["surfaces"].as_array().into_iter().flatten() {
            if s["kind"] != "mcp" {
                continue;
            }
            let Some(url) = s["url"].as_str() else {
                continue;
            };
            if out.len() >= MAX_SUGGESTIONS {
                return out;
            }
            out.push(ConnectorSuggestion {
                domain: domain.to_string(),
                name: r["name"].as_str().unwrap_or(domain).to_string(),
                description: r["description"]
                    .as_str()
                    .map(str::trim)
                    .filter(|d| !d.is_empty())
                    .map(str::to_string),
                url: url.to_string(),
                auth: s["auth"]["kind"].as_str().unwrap_or("none").to_string(),
                icon: s["icon"].as_str().map(str::to_string),
            });
        }
    }
    out
}

/// Fetch an image as a `data:` URL: https only (http on loopback, for
/// tests), an `image/*` type, at most [`MAX_ICON_BYTES`]. `None` on any failure.
pub async fn fetch_icon(http: &reqwest::Client, address: &str) -> Option<String> {
    let u = url::Url::parse(address).ok()?;
    if u.scheme() != "https" && !(u.scheme() == "http" && is_loopback(&u)) {
        return None;
    }
    let mut r = http.get(u).timeout(ICON_TIMEOUT).send().await.ok()?;
    if !r.status().is_success() {
        return None;
    }
    let mime = r
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(';').next().unwrap_or("").trim().to_ascii_lowercase())?;
    if !mime.starts_with("image/") {
        return None;
    }
    if r.content_length().is_some_and(|n| n as usize > MAX_ICON_BYTES) {
        return None;
    }
    let mut data = vec![];
    while let Some(chunk) = r.chunk().await.ok()? {
        data.extend_from_slice(&chunk);
        if data.len() > MAX_ICON_BYTES {
            return None;
        }
    }
    if data.is_empty() {
        return None;
    }
    Some(format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&data)
    ))
}

/// The site a connector's address belongs to, for its icon: the last two
/// labels of the host (`mcp.linear.app` → `linear.app`). `None` for an IP
/// address or a name without a dot (`localhost`).
pub fn icon_domain(connector_url: &str) -> Option<String> {
    let u = url::Url::parse(connector_url).ok()?;
    let host = match u.host()? {
        url::Host::Domain(d) => d.to_ascii_lowercase(),
        _ => return None,
    };
    let labels: Vec<&str> = host.trim_end_matches('.').split('.').collect();
    if labels.len() < 2 {
        return None;
    }
    Some(labels[labels.len() - 2..].join("."))
}

/// The favicon service's address for a connector, or `None` when it has none.
pub fn favicon_address(base: &str, connector_url: &str) -> Option<String> {
    let domain = icon_domain(connector_url)?;
    let mut u = url::Url::parse(base).ok()?;
    u.query_pairs_mut()
        .append_pair("domain", &domain)
        .append_pair("sz", "64");
    Some(u.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn domains_and_mapping() {
        assert_eq!(icon_domain("https://mcp.linear.app/mcp").as_deref(), Some("linear.app"));
        assert_eq!(icon_domain("https://notion.com/x").as_deref(), Some("notion.com"));
        assert_eq!(icon_domain("http://127.0.0.1:9/mcp"), None);
        assert_eq!(icon_domain("http://localhost:9/mcp"), None);
        assert_eq!(
            favicon_address("https://www.google.com/s2/favicons", "https://mcp.linear.app/mcp").as_deref(),
            Some("https://www.google.com/s2/favicons?domain=linear.app&sz=64")
        );
        let body = json!({"results": [{
            "domain": "linear.app", "name": "linear.app", "description": "Linear ",
            "surfaces": [
                {"kind": "mcp", "url": "https://mcp.linear.app/mcp", "icon": "https://i/x.png", "auth": {"kind": "oauth"}},
                {"kind": "graphql", "url": "https://api.linear.app/graphql"},
                {"kind": "mcp", "url": "https://mcp.linear.app/sse"}
            ]
        }]});
        let s = parse_search(&body);
        assert_eq!(s.len(), 2);
        assert_eq!((s[0].auth.as_str(), s[0].description.as_deref()), ("oauth", Some("Linear")));
        assert_eq!((s[1].auth.as_str(), s[1].icon.as_deref()), ("none", None));
    }
}
