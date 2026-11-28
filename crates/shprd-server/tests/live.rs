//! `live_agents`, `spaces` and the per-agent routes, served.
//!
//! The source is a recorder, so no host is touched: what is proved is the shape
//! `GET /api/state` answers (the contract the sidebar tree and the Agents pane
//! read), that a client can only reach an agent the source listed, and that a
//! service with no host lists none and refuses the routes.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::Value;
use shprd_core::event::Actor;
use shprd_server::{App, HostSpace, LiveAgentRow, LiveSource};
use tempfile::TempDir;
use tower::ServiceExt;

/// The port the fixture claims.
const PORT: u16 = 18_097;

/// A source listing one worker and one finished reviewer.
struct Two;

impl LiveSource for Two {
    fn space(&self) -> Option<HostSpace> {
        Some(HostSpace {
            id: "w-7".into(),
            name: "shprd · repo".into(),
        })
    }

    fn agents(&self) -> Vec<LiveAgentRow> {
        let row = |id: &str, role: &str, state: &str, ended: Option<u64>| LiveAgentRow {
            id: id.into(),
            task: Some("T1".into()),
            role: role.into(),
            program: "claude".into(),
            state: state.into(),
            terminal: id.into(),
            started_ms: Some(10),
            ended_ms: ended,
            since_ms: Some(20),
        };
        vec![
            row("t3", "worker", "working", None),
            row("t4", "reviewer", "done", Some(30)),
        ]
    }

    fn output(&self, id: &str, _lines: usize) -> Result<String, String> {
        Ok(format!("output of {id}"))
    }
}

struct Fixture {
    app: App,
    token: String,
    _dirs: Vec<TempDir>,
}

impl Fixture {
    fn new(source: Option<Arc<dyn LiveSource>>) -> Fixture {
        let state = TempDir::new().expect("state");
        let repo = TempDir::new().expect("repo");
        fs::create_dir_all(repo.path().join(".shprd")).expect("shprd dir");
        for args in [
            &["init", "-q", "-b", "feature/x"][..],
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "i",
            ],
        ] {
            let ok = std::process::Command::new("git")
                .args(args)
                .current_dir(repo.path())
                .status()
                .expect("git")
                .success();
            assert!(ok, "git {args:?}");
        }
        let mut app =
            App::prepare(repo.path(), PORT, &Actor::Human, state.path(), None).expect("prepare");
        if let Some(source) = source {
            app = app.with_live_source(source);
        }
        let token = fs::read_to_string(&app.info().token_path)
            .expect("token")
            .trim()
            .to_string();
        Fixture {
            app,
            token,
            _dirs: vec![state, repo],
        }
    }

    async fn call(&self, method: Method, uri: &str) -> (StatusCode, Value) {
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::HOST, format!("127.0.0.1:{PORT}"))
            .header(header::AUTHORIZATION, format!("Bearer {}", self.token))
            .header("x-shprd", "1")
            .body(Body::empty())
            .expect("request");
        let response = self.app.router().oneshot(request).await.expect("response");
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }
}

#[tokio::test]
async fn the_state_lists_the_space_and_the_agents_with_their_terminals() {
    let fixture = Fixture::new(Some(Arc::new(Two)));

    let (status, state) = fixture.call(Method::GET, "/api/state").await;

    assert_eq!(status, StatusCode::OK);
    let spaces = state["spaces"].as_array().expect("spaces");
    assert_eq!(spaces.len(), 1);
    let space = spaces[0]["id"].as_str().expect("space id");
    assert_eq!(
        spaces[0]["herdr_id"], "w-7",
        "Herdr's own space id, not the synthetic one"
    );
    assert_eq!(spaces[0]["herdr_name"], "shprd · repo");
    assert_eq!(
        spaces[0]["branch"], "feature/x",
        "the repo's current branch"
    );
    let agents = state["live_agents"].as_array().expect("live_agents");
    assert_eq!(agents.len(), 2);
    assert_eq!(agents[0]["role"], "worker");
    assert_eq!(agents[0]["task"], "T1");
    assert_eq!(agents[0]["state"], "working");
    assert_eq!(
        agents[0]["terminal"], "t3",
        "a window binds a terminal block to it"
    );
    assert_eq!(agents[0]["space"], space);
    assert_eq!(agents[1]["state"], "done");
    assert_eq!(agents[1]["ended_ms"], 30);
}

#[tokio::test]
async fn an_agents_output_and_attach_are_served_only_for_listed_agents() {
    let fixture = Fixture::new(Some(Arc::new(Two)));

    let (status, out) = fixture
        .call(Method::GET, "/api/agents/t3/output?lines=5")
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(out["text"], "output of t3");
    assert_eq!(out["live"], true);

    let (status, finished) = fixture.call(Method::GET, "/api/agents/t4/output").await;
    assert_eq!(status, StatusCode::OK, "a finished agent stays readable");
    assert_eq!(finished["live"], false);

    let (status, attach) = fixture.call(Method::POST, "/api/agents/t3/attach").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(attach["terminal"], "t3");
    assert_eq!(attach["socket"], "/api/terminal?space=agent:t3");

    let (status, _) = fixture.call(Method::POST, "/api/agents/t4/attach").await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "nothing to type into once it ended"
    );
    let (status, _) = fixture.call(Method::GET, "/api/agents/t99/output").await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "never a terminal nobody listed"
    );
}

#[tokio::test]
async fn a_service_with_no_host_lists_no_agents_and_refuses_the_routes() {
    let fixture = Fixture::new(None);

    let (_, state) = fixture.call(Method::GET, "/api/state").await;
    assert_eq!(state["live_agents"].as_array().expect("array").len(), 0);
    let (status, _) = fixture.call(Method::GET, "/api/agents/t3/output").await;
    assert_eq!(status, StatusCode::CONFLICT);
}
