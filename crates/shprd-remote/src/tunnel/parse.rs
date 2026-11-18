//! Reading a tunnel program's output: the pure part.
//!
//! Every provider announces its public address differently — cloudflared in a
//! box on stderr, tunnl.gg as a JSON line or a coloured banner, ngrok through a
//! local JSON API, Tailscale through `status --json` — and each of those formats
//! is somebody else's, so the parsers here take **text in, answer out** and
//! touch no process or file. They are tested against output captured from the
//! real programs (`tests/fixtures/`).
//!
//! The URL a quick tunnel is reached at is the only secret that stands in front
//! of the login page, so nothing in this module formats a full URL into an
//! error or a log line: [`mask_url`] and [`redact`] are what every message that
//! could carry one goes through (M7d §4.3, threat row 3).
//!
//! The Tailscale helpers are ported from Shepherdr's `service/tailscale.ts`
//! (eliasstravik/shepherdr, commit a3f327a, MIT): the owner and name from
//! `status --json`, what `serve` does with the mount, who holds an HTTPS port
//! and which one to suggest instead. See `THIRD_PARTY_NOTICES.md`.

use serde_json::Value;

/// Removes ANSI escape sequences (colours, cursor moves) and carriage returns.
///
/// tunnl.gg paints its banner for a terminal, and `ssh -t` makes it believe
/// there is one. Only CSI (`ESC [ … final`) and OSC (`ESC ] … BEL`/`ESC \`)
/// sequences are removed; anything else is kept as text.
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\r' => {}
            '\u{1b}' => match chars.peek().copied() {
                Some('[') => {
                    chars.next();
                    // Parameters and intermediates run to a final byte 0x40–0x7e.
                    for next in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&next) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    while let Some(next) = chars.next() {
                        if next == '\u{7}' {
                            break;
                        }
                        if next == '\u{1b}' {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            },
            other => out.push(other),
        }
    }
    out
}

/// The `https://` tokens in a line, trailing punctuation removed.
///
/// A token ends at whitespace, a quote, a bracket or a box-drawing `|`, which is
/// how cloudflared frames its banner.
pub fn https_tokens(line: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut rest = line;
    while let Some(at) = rest.find("https://") {
        let tail = &rest[at..];
        let end = tail
            .find(|ch: char| {
                ch.is_whitespace() || matches!(ch, '"' | '\'' | '<' | '>' | '(' | ')' | '|' | '`')
            })
            .unwrap_or(tail.len());
        let token = tail[..end].trim_end_matches(['.', ',', ';', ':']);
        if token.len() > "https://".len() {
            found.push(token);
        }
        rest = &tail[end..];
    }
    found
}

/// The `host[:port]` of an `https://` URL, lowercase, when it is a plain one.
///
/// Anything with credentials, an unusual character or an empty name is `None`:
/// the result goes straight into the `Host` allowlist, and a name that is not
/// plainly a name must not get there.
pub fn authority_of(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = rest[..end].to_ascii_lowercase();
    let (name, port) = match authority.rsplit_once(':') {
        Some((name, port)) => (name, Some(port)),
        None => (authority.as_str(), None),
    };
    if name.is_empty()
        || name.starts_with('.')
        || name.starts_with('-')
        || name.ends_with('.')
        || name.contains("..")
        || !name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '.')
    {
        return None;
    }
    if let Some(port) = port
        && (port.is_empty()
            || !port.chars().all(|ch| ch.is_ascii_digit())
            || port.parse::<u16>().is_err())
    {
        return None;
    }
    // A default port is the same endpoint as no port, and `Host` carries none.
    Some(
        authority
            .strip_suffix(":443")
            .unwrap_or(&authority)
            .to_owned(),
    )
}

/// The name part of an authority, without a port.
pub fn name_of(authority: &str) -> &str {
    authority
        .rsplit_once(':')
        .map(|(name, _)| name)
        .unwrap_or(authority)
}

/// A URL with everything that identifies it hidden: the leading labels of its
/// host and its path.
///
/// `https://quiet-green-river.trycloudflare.com/x` becomes
/// `https://****.trycloudflare.com/…`. It is the only form of a tunnel URL that
/// may appear in a log, an error or `shprd web status` without `--show-url`.
pub fn mask_url(url: &str) -> String {
    let (scheme, rest) = match url.split_once("://") {
        Some((scheme, rest)) => (scheme, rest),
        None => return "****".to_owned(),
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    let has_more = end < rest.len() && !rest[end..].trim_matches('/').is_empty();
    let (name, port) = match authority.rsplit_once(':') {
        Some((name, port)) if port.chars().all(|ch| ch.is_ascii_digit()) => (name, Some(port)),
        _ => (authority, None),
    };
    let labels: Vec<&str> = name.split('.').collect();
    let shown = if labels.len() > 2 {
        format!("****.{}", labels[labels.len() - 2..].join("."))
    } else {
        "****".to_owned()
    };
    let mut masked = format!("{scheme}://{shown}");
    if let Some(port) = port {
        masked.push(':');
        masked.push_str(port);
    }
    if has_more {
        masked.push_str("/…");
    }
    masked
}

/// `text` with every URL in it masked.
///
/// A child's own words are never put in an error as they are: a failure line
/// from cloudflared may repeat the address it was reachable at.
pub fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut first = true;
    for word in text.split_whitespace() {
        if !first {
            out.push(' ');
        }
        first = false;
        let scheme = word
            .find("https://")
            .into_iter()
            .chain(word.find("http://"))
            .min();
        match scheme {
            Some(at) => {
                out.push_str(&word[..at]);
                out.push_str(&mask_url(&word[at..]));
            }
            None => out.push_str(word),
        }
    }
    out
}

/// Whether `name` is `domain` or a name under it, and not one of `except`.
fn under(name: &str, domain: &str, except: &[&str]) -> bool {
    let name = name.trim_end_matches('.');
    name.len() > domain.len() + 1
        && name.ends_with(domain)
        && name.as_bytes()[name.len() - domain.len() - 1] == b'.'
        && !except.contains(&name)
}

/// The quick-tunnel address in one line of cloudflared's output, if there is one.
///
/// cloudflared prints other `https://` addresses — its terms of service, the
/// API it asked for the tunnel (`api.trycloudflare.com`) — so a candidate must
/// be a *name under* `trycloudflare.com` and not the API's.
pub fn cloudflared_quick(line: &str) -> Option<String> {
    for token in https_tokens(&strip_ansi(line)) {
        let Some(authority) = authority_of(token) else {
            continue;
        };
        if under(&authority, "trycloudflare.com", &["api.trycloudflare.com"]) {
            return Some(format!("https://{authority}"));
        }
    }
    None
}

/// Whether a cloudflared line says a connection to the edge is up.
///
/// A named tunnel prints no address of its own (the hostname is the person's),
/// so this line is how "it is running" is known.
pub fn cloudflared_registered(line: &str) -> bool {
    strip_ansi(line).contains("Registered tunnel connection")
}

/// The tunnl.gg address in one line of its output, if there is one.
///
/// With `output=json` the line is `{"event":"tunnel","url":"https://…"}`; without
/// it, the banner has a `URL` row. Both are accepted so a change in what the
/// service does with the option does not leave a tunnel that cannot be found.
/// The address must be a subdomain of `tunnl.gg`: the banner also links the
/// pricing and dashboard pages on the bare domain.
pub fn tunnl(line: &str) -> Option<String> {
    let line = strip_ansi(line);
    let trimmed = line.trim();
    if trimmed.starts_with('{')
        && let Ok(Value::Object(object)) = serde_json::from_str::<Value>(trimmed)
    {
        if object.get("event").and_then(Value::as_str) != Some("tunnel") {
            return None;
        }
        let url = object.get("url").and_then(Value::as_str)?;
        return tunnl_url(url);
    }
    // The banner row is `URL      https://…`; any other line that links a
    // subdomain is not the tunnel (the upgrade line, the dashboard).
    let after = trimmed.strip_prefix("URL")?;
    https_tokens(after).into_iter().find_map(tunnl_url)
}

/// `url` when it is a plain tunnl.gg tunnel address.
fn tunnl_url(url: &str) -> Option<String> {
    let authority = authority_of(url)?;
    under(&authority, "tunnl.gg", &["proxy.tunnl.gg"]).then(|| format!("https://{authority}"))
}

/// The public address of the ngrok tunnel that forwards to `port`, from the
/// agent's `GET /api/tunnels` answer.
///
/// The agent's API is a loopback port anyone on the machine can answer on, and
/// a second ngrok may be running for something else, so the entry must be an
/// `https` tunnel whose `config.addr` points at *this* service's port.
pub fn ngrok(api_json: &str, port: u16) -> Option<String> {
    let value: Value = serde_json::from_str(api_json).ok()?;
    for tunnel in value.get("tunnels")?.as_array()? {
        let addr = tunnel.pointer("/config/addr").and_then(Value::as_str)?;
        if !addr_has_port(addr, port) {
            continue;
        }
        let url = tunnel.get("public_url").and_then(Value::as_str)?;
        if let Some(authority) = authority_of(url) {
            return Some(format!("https://{authority}"));
        }
    }
    None
}

/// Whether an upstream address such as `http://localhost:8080` ends in `port`.
fn addr_has_port(addr: &str, port: u16) -> bool {
    addr.trim_end_matches('/')
        .rsplit_once(':')
        .and_then(|(_, tail)| tail.parse::<u16>().ok())
        == Some(port)
}

/// Who owns this machine on the tailnet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Me {
    /// The owner's login, e.g. an email address.
    pub login: String,
    /// The machine's tailnet name, without the trailing dot.
    pub dns: String,
}

/// The owner and the machine's tailnet name from `tailscale status --json`.
///
/// `None` when Tailscale is not running or the answer is not the shape this
/// reads; the caller refuses to start rather than guessing a name.
pub fn tailscale_me(status_json: &str) -> Option<Me> {
    let value: Value = serde_json::from_str(status_json).ok()?;
    let user_id = value.pointer("/Self/UserID").map(|id| match id {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    })?;
    let login = value
        .pointer(&format!("/User/{user_id}/LoginName"))
        .and_then(Value::as_str)?
        .to_owned();
    let dns = value
        .pointer("/Self/DNSName")
        .and_then(Value::as_str)?
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if dns.is_empty() || !under(&dns, "ts.net", &[]) {
        return None;
    }
    Some(Me { login, dns })
}

/// What `tailscale serve` does with a mount point now.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Served {
    /// Where the mount proxies to, when it is mounted at all.
    pub proxy: Option<String>,
    /// The HTTPS port it is served on.
    pub port: Option<u16>,
    /// Whether Funnel is on for that port.
    pub funnel: bool,
}

/// What `serve status --json` says about `mount` on this machine's name.
pub fn tailscale_served(status_json: &str, dns: &str, mount: &str) -> Served {
    let Ok(value) = serde_json::from_str::<Value>(status_json) else {
        return Served::default();
    };
    let Some(web) = value.get("Web").and_then(Value::as_object) else {
        return Served::default();
    };
    for (host_port, entry) in web {
        let (host, port) = match host_port.rsplit_once(':') {
            Some((host, port)) => (host, port.parse::<u16>().ok()),
            None => (host_port.as_str(), None),
        };
        if !host.eq_ignore_ascii_case(dns) {
            continue;
        }
        if let Some(handler) = entry
            .get("Handlers")
            .and_then(|handlers| handlers.get(mount))
        {
            return Served {
                proxy: handler
                    .get("Proxy")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                port,
                funnel: value
                    .pointer(&format!("/AllowFunnel/{host_port}"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            };
        }
    }
    Served::default()
}

/// Who else holds HTTPS port `port` on this machine's tailnet name: a raw TCP
/// forward or plain TCP. `None` when it is free, or already Tailscale's own
/// HTTPS (the mount can join it).
pub fn tailscale_held_by(status_json: &str, port: u16) -> Option<String> {
    let value: Value = serde_json::from_str(status_json).ok()?;
    let entry = value.pointer(&format!("/TCP/{port}"))?;
    if let Some(forward) = entry.get("TCPForward").and_then(Value::as_str) {
        return Some(format!("a TCP forward to {forward}"));
    }
    let https = entry.get("HTTPS").and_then(Value::as_bool).unwrap_or(false);
    let http = entry.get("HTTP").and_then(Value::as_bool).unwrap_or(false);
    (!https && !http).then(|| "another app's TCP share".to_owned())
}

/// The HTTPS ports Tailscale will serve on, in the order they are tried.
///
/// `tailscale serve` accepts 443, 8443 and 10000 and nothing else, so a taken
/// 443 falls to the next of these rather than to an arbitrary number.
pub const TAILSCALE_HTTPS_PORTS: [u16; 3] = [443, 8443, 10000];

/// The first of [`TAILSCALE_HTTPS_PORTS`] nobody else holds, or `None`.
pub fn tailscale_pick_port(status_json: &str) -> Option<u16> {
    TAILSCALE_HTTPS_PORTS
        .into_iter()
        .find(|port| tailscale_held_by(status_json, *port).is_none())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    const CLOUDFLARED: &str = include_str!("../../tests/fixtures/cloudflared-quick.stderr");
    const TUNNL_JSON: &str = include_str!("../../tests/fixtures/tunnl.json.out");
    const TUNNL_BANNER: &str = include_str!("../../tests/fixtures/tunnl.banner.out");
    const NGROK: &str = include_str!("../../tests/fixtures/ngrok-api.json");
    const TS_STATUS: &str = include_str!("../../tests/fixtures/tailscale-status.json");
    const TS_SERVE: &str = include_str!("../../tests/fixtures/tailscale-serve-status.json");

    fn first<'a>(text: &'a str, parse: impl Fn(&'a str) -> Option<String> + 'a) -> Option<String> {
        text.lines().find_map(parse)
    }

    #[test]
    fn cloudflared_banner_gives_the_quick_address() {
        assert_eq!(
            first(CLOUDFLARED, cloudflared_quick).as_deref(),
            Some("https://quiet-green-river-sample.trycloudflare.com")
        );
    }

    #[test]
    fn cloudflared_other_addresses_are_not_the_tunnel() {
        // Its terms page, and the API it asks for the tunnel, are on stderr too.
        for line in [
            "INF see https://www.cloudflare.com/website-terms/ for the terms",
            r#"ERR Post "https://api.trycloudflare.com/tunnel": dial tcp: lookup failed"#,
            "INF https://trycloudflare.com",
            "INF https://evil.example.com/.trycloudflare.com",
            "INF https://trycloudflare.com.evil.example",
        ] {
            assert_eq!(cloudflared_quick(line), None, "{line}");
        }
    }

    #[test]
    fn cloudflared_ready_line_is_found() {
        assert!(CLOUDFLARED.lines().any(cloudflared_registered));
        assert!(!cloudflared_registered("INF Starting metrics server"));
    }

    #[test]
    fn tunnl_json_line_gives_the_address() {
        assert_eq!(
            first(TUNNL_JSON, tunnl).as_deref(),
            Some("https://bold-fox-00000000.tunnl.gg")
        );
    }

    #[test]
    fn tunnl_banner_gives_the_address_through_the_colour_codes() {
        assert!(TUNNL_BANNER.contains('\u{1b}'));
        assert_eq!(
            first(TUNNL_BANNER, tunnl).as_deref(),
            Some("https://kind-stone-00000000.tunnl.gg")
        );
    }

    #[test]
    fn tunnl_pricing_and_dashboard_links_are_not_the_tunnel() {
        for line in [
            "  Pro: your own name, from $5/mo: https://tunnl.gg/pricing?utm_source=ssh",
            "  Account  See and replay every request: https://tunnl.gg/dashboard",
            r#"{"event":"other","url":"https://x-y-00000000.tunnl.gg"}"#,
            r#"{"event":"tunnel","url":"https://tunnl.gg"}"#,
            r#"{"event":"tunnel","url":"https://proxy.tunnl.gg"}"#,
            r#"{"event":"tunnel","url":"https://evil.example.com"}"#,
        ] {
            assert_eq!(tunnl(line), None, "{line}");
        }
    }

    #[test]
    fn ngrok_answer_gives_the_address_for_our_port_only() {
        assert_eq!(
            ngrok(NGROK, 18765).as_deref(),
            Some("https://sample-name-here.ngrok-free.dev")
        );
        // Another ngrok forwarding something else is not our tunnel.
        assert_eq!(ngrok(NGROK, 9999), None);
        assert_eq!(ngrok("{}", 18765), None);
        assert_eq!(ngrok("not json", 18765), None);
    }

    #[test]
    fn tailscale_status_gives_owner_and_name() {
        let me = tailscale_me(TS_STATUS).unwrap();
        assert_eq!(me.login, "person@example.com");
        assert_eq!(me.dns, "studio.tail1234.ts.net");
        assert_eq!(tailscale_me("{}"), None);
        assert_eq!(
            tailscale_me(&TS_STATUS.replace("studio.tail1234.ts.net.", "studio.example.com.")),
            None,
            "a name outside ts.net is not the tailnet's"
        );
    }

    #[test]
    fn tailscale_serve_status_is_read() {
        let served = tailscale_served(TS_SERVE, "studio.tail1234.ts.net", "/shprd");
        assert_eq!(served.proxy.as_deref(), Some("http://127.0.0.1:9999"));
        assert_eq!(served.port, Some(443));
        assert!(!served.funnel);
        assert_eq!(
            tailscale_served(TS_SERVE, "studio.tail1234.ts.net", "/other"),
            Served::default()
        );
        let funnel = TS_SERVE.replace("false", "true");
        assert!(tailscale_served(&funnel, "studio.tail1234.ts.net", "/shprd").funnel);
    }

    #[test]
    fn tailscale_ports_are_held_or_free() {
        assert_eq!(tailscale_held_by(TS_SERVE, 443), None);
        let forward = r#"{"TCP":{"443":{"TCPForward":"127.0.0.1:22"},"8443":{}}}"#;
        assert_eq!(
            tailscale_held_by(forward, 443).as_deref(),
            Some("a TCP forward to 127.0.0.1:22")
        );
        assert_eq!(
            tailscale_held_by(forward, 8443).as_deref(),
            Some("another app's TCP share")
        );
        assert_eq!(tailscale_pick_port(forward), Some(10000));
        assert_eq!(tailscale_pick_port("{}"), Some(443));
        assert_eq!(tailscale_pick_port(TS_SERVE), Some(443));
    }

    #[test]
    fn authority_refuses_what_is_not_plainly_a_name() {
        assert_eq!(
            authority_of("https://A.B.example.com/x?y").as_deref(),
            Some("a.b.example.com")
        );
        assert_eq!(
            authority_of("https://h.ts.net:443/shprd/").as_deref(),
            Some("h.ts.net")
        );
        assert_eq!(
            authority_of("https://h.ts.net:8443/").as_deref(),
            Some("h.ts.net:8443")
        );
        for bad in [
            "http://x.example.com",
            "https://",
            "https://user@x.example.com",
            "https://x example.com",
            "https://x.example.com:99999",
            "https://x.example.com:",
            "https://.example.com",
            "https://a..b.example.com",
            "https://[::1]:8080",
        ] {
            assert_eq!(authority_of(bad), None, "{bad}");
        }
    }

    #[test]
    fn mask_hides_the_name_and_the_path() {
        assert_eq!(
            mask_url("https://quiet-green-river.trycloudflare.com"),
            "https://****.trycloudflare.com"
        );
        assert_eq!(
            mask_url("https://studio.tail1234.ts.net:8443/shprd/"),
            "https://****.ts.net:8443/…"
        );
        assert_eq!(mask_url("https://tunnl.gg"), "https://****");
        assert_eq!(mask_url("garbage"), "****");
    }

    #[test]
    fn redact_masks_every_address_in_a_message() {
        let said = "failed: reach https://quiet-green-river.trycloudflare.com/x, retrying";
        let out = redact(said);
        assert!(!out.contains("quiet-green"), "{out}");
        assert!(out.contains("https://****.trycloudflare.com/…"), "{out}");
        assert!(out.starts_with("failed: reach"));
    }

    #[test]
    fn strip_ansi_removes_colours_and_carriage_returns() {
        assert_eq!(
            strip_ansi("\u{1b}[38;5;141mhttps://a.tunnl.gg\u{1b}[0m\r"),
            "https://a.tunnl.gg"
        );
        assert_eq!(strip_ansi("\u{1b}]0;title\u{7}plain"), "plain");
    }
}
