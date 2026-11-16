//! The web app's routes: assets, the SPA fallback, and the line between them and
//! the API.
//!
//! These tests drive the router with a temp directory as the web build, so they
//! prove the serving rules without needing `web/dist` (and without Node) in the
//! cargo job. The one rule that matters most is the last: nothing under `/api`
//! becomes public just because the page is.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use http_body_util::BodyExt;
use shprd_core::event::Actor;
use shprd_server::App;
use tempfile::TempDir;
use tower::ServiceExt;

/// The port the fixtures claim in `service.json`.
const PORT: u16 = 18_081;

/// One prepared service with its own web build, state directory and repository.
struct Fixture {
    app: App,
    token: String,
    _repo: TempDir,
    _state: TempDir,
    _web: TempDir,
}

impl Fixture {
    /// A service serving a small hand-made build.
    fn new() -> Self {
        Self::with_index(
            "<!doctype html><html><body><div id=\"root\">shprd app shell</div></body></html>",
        )
    }

    /// A service serving a build whose `index.html` is `index`.
    fn with_index(index: &str) -> Self {
        let repo = tempfile::tempdir().expect("repo dir");
        let state = tempfile::tempdir().expect("state dir");
        let web = tempfile::tempdir().expect("web dir");
        std::fs::create_dir_all(web.path().join("assets")).expect("mkdir assets");
        std::fs::write(web.path().join("index.html"), index).expect("write index.html");
        std::fs::write(
            web.path().join("assets").join("index-abc123.js"),
            "console.log('shprd');",
        )
        .expect("write a hashed asset");
        std::fs::write(web.path().join("favicon.svg"), "<svg/>").expect("write the icon");
        let app = App::prepare(repo.path(), PORT, &Actor::Human, state.path(), None)
            .expect("prepare")
            .with_web_dir(web.path().to_path_buf());
        let token = std::fs::read_to_string(app.info().token_path)
            .expect("the token file")
            .trim()
            .to_string();
        Self {
            app,
            token,
            _repo: repo,
            _state: state,
            _web: web,
        }
    }

    /// Sends one GET through the router.
    async fn get(&self, uri: &str) -> Reply {
        self.get_with_token(uri, None).await
    }

    /// Sends one GET carrying `token` when there is one.
    async fn get_with_token(&self, uri: &str, token: Option<&str>) -> Reply {
        let mut builder = Request::builder().method("GET").uri(uri);
        if let Some(token) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let request = builder.body(Body::empty()).expect("a well-formed request");
        let response = self
            .app
            .router()
            .oneshot(request)
            .await
            .expect("the router answers");
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("the body reads")
            .to_bytes();
        Reply {
            status,
            headers,
            body: String::from_utf8_lossy(&bytes).to_string(),
        }
    }
}

/// What a request answered.
struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

impl Reply {
    /// One header, as text.
    fn header(&self, name: header::HeaderName) -> &str {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
    }

    /// The body as JSON.
    fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).expect("the body is JSON")
    }
}

#[tokio::test]
async fn an_spa_route_serves_the_page_without_a_session() {
    // The login screen is the page, so the page must load before anyone has a
    // session: `/tasks` is a client-side route, not an API path.
    let fixture = Fixture::new();

    for uri in ["/tasks", "/tasks/M11.3", "/settings/remote"] {
        let reply = fixture.get(uri).await;
        assert_eq!(reply.status, StatusCode::OK, "{uri}");
        assert!(
            reply.header(header::CONTENT_TYPE).starts_with("text/html"),
            "{uri}"
        );
        assert_eq!(reply.header(header::CACHE_CONTROL), "no-cache", "{uri}");
        assert_eq!(
            reply.header(header::X_CONTENT_TYPE_OPTIONS),
            "nosniff",
            "{uri}"
        );
        assert_eq!(
            reply.header(header::REFERRER_POLICY),
            "no-referrer",
            "{uri}"
        );
        assert!(
            reply.body.contains("shprd app shell"),
            "{uri}: {}",
            reply.body
        );
    }
}

#[tokio::test]
async fn the_root_is_the_page() {
    let fixture = Fixture::new();
    let reply = fixture.get("/").await;

    assert_eq!(reply.status, StatusCode::OK);
    assert!(reply.body.contains("shprd app shell"));
}

#[tokio::test]
async fn a_hashed_asset_is_cached_for_ever() {
    // Vite puts a content hash in an asset's name, so it can never go stale.
    let fixture = Fixture::new();
    let reply = fixture.get("/assets/index-abc123.js").await;

    assert_eq!(reply.status, StatusCode::OK);
    assert!(
        reply
            .header(header::CONTENT_TYPE)
            .starts_with("text/javascript")
    );
    assert_eq!(
        reply.header(header::CACHE_CONTROL),
        "public, max-age=31536000, immutable"
    );
    assert_eq!(reply.body, "console.log('shprd');");
}

#[tokio::test]
async fn a_missing_asset_is_a_404_not_the_page() {
    // `assets/` holds only content-hashed files the build named: answering a
    // miss there with the page would hide a broken build behind a 200.
    let fixture = Fixture::new();
    let reply = fixture.get("/assets/index-gone.js").await;

    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert!(!reply.body.contains("shprd app shell"));
}

#[tokio::test]
async fn the_api_stays_gated_behind_the_page() {
    // This is the split the task exists for: the page is public, the API is not.
    let fixture = Fixture::new();

    let refused = fixture.get("/api/state").await;
    assert_eq!(refused.status, StatusCode::UNAUTHORIZED);
    assert_eq!(refused.json()["code"], serde_json::json!("unauthorized"));

    let allowed = fixture
        .get_with_token("/api/state", Some(&fixture.token))
        .await;
    assert_eq!(allowed.status, StatusCode::OK);
    assert!(
        allowed
            .header(header::CONTENT_TYPE)
            .starts_with("application/json")
    );

    // An unknown API path is the API's typed 404, never the SPA page.
    let missing = fixture.get("/api/no-such-route").await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    assert_eq!(missing.json()["code"], serde_json::json!("not_found"));
}

#[tokio::test]
async fn a_binary_without_a_web_build_says_how_to_build_one() {
    // The embedding must not fail without `web/dist`; the page then says what to
    // run, in words a person can act on.
    let empty = tempfile::tempdir().expect("empty web dir");
    let fixture = Fixture::with_index("<!doctype html><div id=\"root\"></div>");
    let app = fixture.app.with_web_dir(empty.path().to_path_buf());
    let response = app
        .router()
        .oneshot(
            Request::builder()
                .uri("/tasks")
                .body(Body::empty())
                .expect("a well-formed request"),
        )
        .await
        .expect("the router answers");

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("the body reads")
        .to_bytes();
    let body = String::from_utf8_lossy(&bytes);
    assert!(body.contains("without the web app"), "{body}");
    assert!(body.contains("npm ci"), "{body}");
}
