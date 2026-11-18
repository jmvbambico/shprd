//! `shprd web up | down | status`: the tunnel, from the command line.
//!
//! The tunnel is a child of the running `shprd serve` — it must die with it, and
//! it shares the service's `Host` allowlist, password and storm signal — so `up`
//! and `down` do not start anything themselves: they ask the service over its
//! loopback door with the bearer token (exempt from the fresh-password rule,
//! because the token is already local and physical, M7d §4.4 rule 6). With no
//! service running they say so and do nothing.
//!
//! `status` reads `web.json`, which needs no service. **Everything that shows an
//! address masks it unless `--show-url` asked**: a quick tunnel's address is the
//! one thing standing between the internet and a login page, and a terminal is
//! scrolled back, recorded and pasted.

use std::path::Path;
use std::time::Duration;

use anyhow::anyhow;
use serde_json::{Value, json};
use shprd_remote::tunnel::{WebFile, mask_url, qr_unicode};
use shprd_server::ServiceRecord;

use crate::cli::{StatusArgs, UpArgs};
use crate::ctx::CliError;

/// How long `up` waits for a provider that has to reach its edge.
const START_WAIT: Duration = Duration::from_secs(90);

/// How long `down` and the handshake wait.
const SHORT_WAIT: Duration = Duration::from_secs(30);

/// A running service this process may talk to.
struct Service {
    port: u16,
    token: String,
}

/// `shprd web up`.
pub(super) fn up(args: &UpArgs, dir: &Path, json: bool) -> Result<(), CliError> {
    let service = connect(dir)?;
    let mut body = serde_json::Map::new();
    if let Some(provider) = &args.tunnel {
        body.insert("provider".into(), json!(provider));
    }
    if args.stable {
        body.insert("stable".into(), json!(true));
    }
    let reply = service.post("/api/tunnels/start", Value::Object(body), START_WAIT)?;
    if json {
        return print_json(shown(&reply, args.show_url));
    }
    print_status(&reply, args.show_url, "tunnel up");
    Ok(())
}

/// `shprd web down`.
pub(super) fn down(dir: &Path, json: bool) -> Result<(), CliError> {
    let service = connect(dir)?;
    let reply = service.post("/api/tunnels/stop", json!({}), SHORT_WAIT)?;
    if json {
        return print_json(shown(&reply, false));
    }
    println!("tunnel stopped");
    Ok(())
}

/// `shprd web status`.
pub(super) fn status(args: &StatusArgs, dir: &Path, json: bool) -> Result<(), CliError> {
    let file = WebFile::read(dir);
    // A record that says "on" with no service behind it is a service that was
    // killed: its tunnel died with it, and the next `serve` clears the record.
    let service_running = read_record(dir).is_some_and(|record| pid_alive(record.pid));
    let stale = file.on && !service_running;
    let value = json!({
        "on": file.on && service_running,
        "stale": stale,
        "provider": file.provider,
        "link": file.link,
        "qr_svg": file.qr_svg,
        "why": if stale {
            Some("the service that started this tunnel is not running".to_owned())
        } else {
            file.why.clone()
        },
        "at": file.at,
        "version": file.version,
    });
    if json {
        return print_json(shown(&value, args.show_url));
    }
    print_status(&value, args.show_url, "tunnel on");
    Ok(())
}

/// The record with the link and the code removed, unless they were asked for.
fn shown(value: &Value, show_url: bool) -> Value {
    let mut value = value.clone();
    if show_url {
        return value;
    }
    if let Some(object) = value.as_object_mut() {
        if let Some(link) = object.get("link").and_then(Value::as_str) {
            let masked = mask_url(link);
            object.insert("link".into(), json!(masked));
        }
        object.remove("qr_svg");
    }
    value
}

/// The human form of a record.
fn print_status(value: &Value, show_url: bool, on_word: &str) {
    let text = |key: &str| value.get(key).and_then(Value::as_str);
    if value.get("on").and_then(Value::as_bool) != Some(true) {
        println!("tunnel off");
        if let Some(why) = text("why") {
            println!("  why: {why}");
        }
        return;
    }
    let provider = text("provider").unwrap_or("?");
    println!("{on_word}: {provider}");
    if let Some(version) = text("version") {
        println!("  program: {version}");
    }
    let link = text("link");
    match (link, show_url) {
        (Some(link), true) => {
            println!("  link: {link}");
            if let Some(code) = qr_unicode(link) {
                println!("{code}");
            }
        }
        (Some(link), false) => {
            println!("  link: {}", mask_url(link));
            println!("  (run `shprd web status --show-url` for the link and its QR code)");
        }
        (None, _) => {}
    }
}

/// Prints one JSON value on stdout.
fn print_json(value: Value) -> Result<(), CliError> {
    let text = serde_json::to_string_pretty(&value)
        .map_err(|err| CliError::other(anyhow!(err).context("serializing the answer")))?;
    println!("{text}");
    Ok(())
}

/// `service.json`, when a service wrote one.
fn read_record(dir: &Path) -> Option<ServiceRecord> {
    let text = std::fs::read_to_string(dir.join("service.json")).ok()?;
    serde_json::from_str(&text).ok()
}

/// Whether the process `pid` is there, asked of the kernel with signal `0`
/// through the system `kill` at an absolute path (the same way the service asks).
/// A machine with no `kill` answers "alive": a status that called a running
/// service dead would be the worse mistake.
fn pid_alive(pid: u32) -> bool {
    let Some(program) = ["/bin/kill", "/usr/bin/kill"]
        .into_iter()
        .map(Path::new)
        .find(|path| path.is_file())
    else {
        return true;
    };
    std::process::Command::new(program)
        .args(["-0", "--", &pid.to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(true)
}

/// Finds the running service and checks it is the one `service.json` names.
///
/// The bearer token is only sent to a port whose `/api/health` answers with the
/// id the record holds: a stale record points at whatever holds the port now.
fn connect(dir: &Path) -> Result<Service, CliError> {
    let none = || {
        CliError::Invalid(anyhow!(
            "no shprd service is running: the tunnel belongs to `shprd serve`, so start \
             that first (in this repository), then run this again"
        ))
    };
    let record = read_record(dir).ok_or_else(none)?;
    let token = std::fs::read_to_string(dir.join("token"))
        .map_err(|_| none())?
        .trim()
        .to_owned();
    if token.is_empty() {
        return Err(none());
    }
    let service = Service {
        port: record.port,
        token,
    };
    let health = service
        .request("GET", "/api/health", None, SHORT_WAIT, false)
        .map_err(|_| none())?;
    if health.get("id").and_then(Value::as_str) != Some(record.id.as_str()) {
        return Err(none());
    }
    Ok(service)
}

impl Service {
    /// A `POST` with the headers a write needs through the gate.
    fn post(&self, path: &str, body: Value, wait: Duration) -> Result<Value, CliError> {
        self.request("POST", path, Some(body), wait, true)
    }

    /// One request to the service's loopback port, answering its JSON.
    ///
    /// A refusal's own words are the error: the service says why a tunnel did
    /// not start (no password, nothing installed, one is already up), and those
    /// are the lines a person needs.
    fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        wait: Duration,
        authorized: bool,
    ) -> Result<Value, CliError> {
        let origin = format!("http://127.0.0.1:{}", self.port);
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(wait))
            .http_status_as_error(false)
            .build()
            .into();
        let url = format!("{origin}{path}");
        let mut response = if method == "GET" {
            let mut request = agent.get(&url);
            if authorized {
                request = request.header("Authorization", format!("Bearer {}", self.token));
            }
            request.call()
        } else {
            agent
                .post(&url)
                .header("Authorization", format!("Bearer {}", self.token))
                .header("Origin", &origin)
                .header("x-shprd", "1")
                .header("Content-Type", "application/json")
                .send(body.unwrap_or(Value::Null).to_string())
        }
        .map_err(|err| CliError::other(anyhow!("the service did not answer: {err}")))?;
        let status = response.status().as_u16();
        let text = response
            .body_mut()
            .read_to_string()
            .map_err(|err| CliError::other(anyhow!("reading the service's answer: {err}")))?;
        let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if (200..300).contains(&status) {
            return Ok(value);
        }
        let message = value
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("the service refused the request");
        // A rule or a setup problem is a verb that did not happen; a provider
        // that ran and failed is an error.
        if status == 502 || status >= 500 {
            Err(CliError::other(anyhow!("{message}")))
        } else {
            Err(CliError::Invalid(anyhow!("{message}")))
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn a_record_shows_a_masked_link_and_no_code_unless_asked() {
        let value = json!({
            "on": true,
            "link": "https://quiet-green-river.trycloudflare.com",
            "qr_svg": "<svg/>",
        });
        let masked = shown(&value, false);
        assert_eq!(masked["link"], json!("https://****.trycloudflare.com"));
        assert!(masked.get("qr_svg").is_none());
        let full = shown(&value, true);
        assert_eq!(full["link"], value["link"]);
        assert_eq!(full["qr_svg"], json!("<svg/>"));
    }
}
