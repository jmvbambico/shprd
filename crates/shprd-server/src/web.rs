//! The built web app, served from the same origin as the API.
//!
//! `web/dist` is embedded at compile time by [`rust-embed`], so `shprd serve`
//! is one binary: the page, its hashed assets and the API all come from the one
//! loopback listener. A path that is not `/api…` is a static file: a stored
//! asset is served with the cache header its name asks for, and an extensionless
//! path falls back to `index.html`, which is what makes `/tasks` a client-side
//! route rather than a 404.
//!
//! A binary built without `web/dist` still compiles and still serves the API;
//! the page it serves says how to build the app (`npm ci && npm run build` in
//! `web/`, then build again). CI builds the web app separately, so the cargo
//! job never depends on Node.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

use crate::api::AppState;

/// The build this binary was compiled with, if there was one.
///
/// `allow_missing` is what keeps a checkout without `web/dist` compiling; the
/// binary then serves [`NOT_BUILT`] instead of a page.
#[derive(RustEmbed)]
#[folder = "../../web/dist"]
#[allow_missing = true]
struct Dist;

/// Whether this binary embedded a web build.
pub(crate) fn built() -> bool {
    Dist::get("index.html").is_some()
}

/// The page a binary without a web build serves.
///
/// It is deliberately plain HTML: the page exists to tell whoever opened the
/// port why there is no app and what to run, not to look like the app.
pub(crate) const NOT_BUILT: &str = r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>shprd · no web build</title>
    <style>
      body { margin: 0; min-height: 100vh; display: grid; place-items: center;
             background: #21252b; color: #abb2bf;
             font: 13px/1.5 -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; }
      main { max-width: 46rem; padding: 2.5rem; }
      h1 { font-size: 1.1rem; color: #e6e8eb; }
      code { font-family: ui-monospace, Menlo, monospace; color: #61afef; }
      p { margin: 0.75rem 0; }
    </style>
  </head>
  <body>
    <main>
      <h1>This shprd was built without the web app</h1>
      <p>The service is running and its API answers, but no page was compiled
         into this binary.</p>
      <p>Build the app, then build shprd again:</p>
      <p><code>cd web &amp;&amp; npm ci &amp;&amp; npm run build</code><br />
         <code>cargo build</code></p>
    </main>
  </body>
</html>
"#;

/// Where a static file comes from.
#[derive(Debug, Clone, Default)]
pub(crate) enum Source {
    /// The build embedded in this binary.
    #[default]
    Embedded,
    /// A directory, for a test that wants its own `index.html` and assets.
    ///
    /// Production never uses it: `shprd serve` serves the embedded build. A
    /// test points it at a temp dir so the SPA fallback and the cache headers
    /// are proven without depending on whether `web/dist` was built.
    Dir(PathBuf),
}

/// One file: its bytes and content type.
struct File {
    /// The file's bytes.
    bytes: Cow<'static, [u8]>,
    /// The content type to answer with.
    mime: String,
}

impl Source {
    /// Reads `name`, if it exists.
    fn get(&self, name: &str) -> Option<File> {
        match self {
            Self::Embedded => Dist::get(name).map(|asset| {
                let mut mime = asset.metadata.mimetype().to_string();
                // A text file without a charset is guessed by the browser;
                // saying it is UTF-8 keeps an accented word intact (the `Dir`
                // source below already says so).
                if mime.starts_with("text/") && !mime.contains("charset") {
                    mime.push_str("; charset=utf-8");
                }
                File {
                    bytes: asset.data,
                    mime,
                }
            }),
            Self::Dir(dir) => {
                // Only a test uses this source, with a handful of tiny files
                // in a temp directory, so the read stays simple: production
                // embeds the app and never touches a file system here.
                let path = safe_join(dir, name)?;
                let bytes = std::fs::read(&path).ok()?;
                Some(File {
                    mime: mime_for(name).to_string(),
                    bytes: Cow::Owned(bytes),
                })
            }
        }
    }
}

/// The fallback handler: everything the API routes did not match.
///
/// `/api…` keeps the API's typed 404; every other GET is a file, an SPA route,
/// or the "no web build" page.
pub(crate) async fn serve(State(state): State<AppState>, request: Request) -> Response {
    let path = request.uri().path().to_string();
    if path == "/api" || path.starts_with("/api/") {
        return crate::api::not_found().into_response();
    }
    if !matches!(*request.method(), Method::GET | Method::HEAD) {
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            [(header::ALLOW, "GET, HEAD")],
            "the web app is read-only",
        )
            .into_response();
    }
    answer(&state.web, &path).await
}

/// The answer for one non-API path.
async fn answer(source: &Source, path: &str) -> Response {
    let wanted = match normalize(path) {
        Some(name) => name,
        None => return plain(StatusCode::NOT_FOUND, "not found"),
    };
    let name = if wanted.is_empty() {
        "index.html"
    } else {
        wanted
    };

    if let Some(file) = source.get(name) {
        return asset(name, file);
    }

    // Everything under `assets/` is a content-hashed file the build named: a
    // miss there is a broken build, and answering with the page would hide it
    // behind a 200. Every other path is an SPA route (`/tasks`, `/tasks/M11.3`),
    // so it gets the page.
    if is_built_asset(name) {
        return plain(StatusCode::NOT_FOUND, "not found");
    }

    match source.get("index.html") {
        Some(file) => asset("index.html", file),
        None => page(StatusCode::OK, NOT_BUILT),
    }
}

/// One stored file, with the cache header its name asks for.
fn asset(name: &str, file: File) -> Response {
    // Vite puts a content hash in every built asset's name, so an asset can be
    // cached for ever; the page itself must be re-read every time, or a new
    // build would never reach an open browser.
    let cache = if name.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    let mut response = Response::new(Body::from(file.bytes));
    let headers = response.headers_mut();
    if let Ok(value) = axum::http::HeaderValue::from_str(&file.mime) {
        headers.insert(header::CONTENT_TYPE, value);
    }
    headers.insert(header::CACHE_CONTROL, cache_header(cache));
    security_headers(headers);
    response
}

/// The "no web build" page, or any other HTML answer.
fn page(status: StatusCode, html: &'static str) -> Response {
    let mut response = Response::new(Body::from(html));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/html; charset=utf-8"),
    );
    headers.insert(header::CACHE_CONTROL, cache_header("no-cache"));
    security_headers(headers);
    *response.status_mut() = status;
    response
}

/// A plain-text refusal.
fn plain(status: StatusCode, message: &'static str) -> Response {
    let mut response = Response::new(Body::from(message));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    security_headers(headers);
    *response.status_mut() = status;
    response
}

/// `Cache-Control`, from a static string.
fn cache_header(value: &'static str) -> axum::http::HeaderValue {
    axum::http::HeaderValue::from_static(value)
}

/// The headers every static answer carries.
///
/// The page is served to a browser from a loopback origin and, once a tunnel is
/// on, from the internet: a browser must never guess a content type, and a
/// referrer must never carry a tunnel URL somewhere else (M7d §2.2).
fn security_headers(headers: &mut axum::http::HeaderMap) {
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        axum::http::HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        axum::http::HeaderValue::from_static("no-referrer"),
    );
}

/// The requested path as a file name, or `None` when it cannot be one.
///
/// The lookup is a map key (embedded) or a join under one directory (tests), so
/// a segment that could leave the directory is refused rather than cleaned up:
/// `..`, a dot file segment, a backslash, or a NUL.
fn normalize(path: &str) -> Option<&str> {
    let name = path.strip_prefix('/')?;
    for part in name.split('/') {
        if part == "." || part == ".." || part.contains('\\') || part.contains('\0') {
            return None;
        }
    }
    Some(name)
}

/// Whether `name` is a file the build put under `assets/`.
///
/// That folder is the only one Vite writes, with a content hash in every name;
/// everything else a client asks for is a route and gets the page.
fn is_built_asset(name: &str) -> bool {
    name.starts_with("assets/")
}

/// Joins `name` under `dir`, refusing any segment that could leave it.
fn safe_join(dir: &Path, name: &str) -> Option<PathBuf> {
    let mut path = dir.to_path_buf();
    for part in name.split('/') {
        if part.is_empty() || part == "." || part == ".." || part.contains('\\') {
            return None;
        }
        path.push(part);
    }
    Some(path)
}

/// A content type from a file's extension.
///
/// It is only used for [`Source::Dir`]; an embedded file carries the type
/// `rust-embed`'s mime guess gave it at compile time.
fn mime_for(name: &str) -> &'static str {
    let extension = name.rsplit('.').next().unwrap_or_default();
    match extension {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "webp" => "image/webp",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "wasm" => "application/wasm",
        "txt" | "md" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn a_path_is_a_file_name_or_nothing() {
        assert_eq!(normalize("/"), Some(""));
        assert_eq!(normalize("/tasks"), Some("tasks"));
        assert_eq!(normalize("/assets/app-1.js"), Some("assets/app-1.js"));
        assert_eq!(normalize("/../etc/passwd"), None);
        assert_eq!(normalize("/a/../b"), None);
        assert_eq!(normalize("/a/./b"), None);
        assert_eq!(normalize("no-leading-slash"), None);
    }

    #[test]
    fn only_a_built_asset_is_a_file_rather_than_a_route() {
        // A task id may hold a dot (`/tasks/M11.3`), so an extension is not how
        // the two are told apart; the `assets/` folder the build writes is.
        assert!(is_built_asset("assets/app-1.js"));
        assert!(!is_built_asset("tasks"));
        assert!(!is_built_asset("tasks/M11.3"));
        assert!(!is_built_asset("favicon.svg"));
    }

    #[test]
    fn a_join_never_leaves_the_directory() {
        let dir = Path::new("/srv/web");
        assert_eq!(
            safe_join(dir, "assets/app.js"),
            Some(PathBuf::from("/srv/web/assets/app.js"))
        );
        assert_eq!(safe_join(dir, "../secret"), None);
        assert_eq!(safe_join(dir, "assets/../../secret"), None);
    }

    #[test]
    fn a_build_without_the_app_has_a_page_to_serve() {
        // The placeholder is the answer only when no `index.html` was embedded;
        // the page is asserted to say what to run, because that is its job.
        assert!(NOT_BUILT.contains("npm ci"));
        assert!(NOT_BUILT.contains("web"));
    }
}
