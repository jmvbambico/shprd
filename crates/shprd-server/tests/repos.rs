//! One service, several repositories: registration, routing by `?repo=`, the
//! refusals, and the sync that stops serving a repository whose space closed.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use shprd_server::{Config, RepoFactory, RepoParts, Service};
use tower::ServiceExt;

/// A factory that serves any folder as its own repository and starts nothing.
#[derive(Debug)]
struct Plain;

impl RepoFactory for Plain {
    fn root_of(&self, folder: &Path) -> PathBuf {
        folder.to_path_buf()
    }

    fn build(
        &self,
        _root: &Path,
        _folder: &Path,
        _hub: shprd_server::hub::Hub,
        _dir: &Path,
    ) -> Result<RepoParts, String> {
        Ok(RepoParts::default())
    }
}

struct Fixture {
    service: Service,
    token: String,
    state: tempfile::TempDir,
}

async fn fixture() -> Fixture {
    let state = tempfile::tempdir().unwrap();
    let service = Service::start(Config::service(state.path().join("shprd"), Arc::new(Plain)))
        .await
        .expect("the service starts");
    let token = std::fs::read_to_string(&service.info().token_path)
        .unwrap()
        .trim()
        .to_string();
    Fixture {
        service,
        token,
        state,
    }
}

impl Fixture {
    async fn call(&self, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "127.0.0.1")
            .header("authorization", format!("Bearer {}", self.token))
            .header("x-shprd", "1");
        let body = match body {
            Some(body) => {
                request = request.header("content-type", "application/json");
                Body::from(body.to_string())
            }
            None => Body::empty(),
        };
        let response = self
            .service
            .registry()
            .router()
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }
}

fn folder(name: &str) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(name);
    std::fs::create_dir(&path).unwrap();
    let text = path.display().to_string();
    (dir, text)
}

#[tokio::test]
async fn repositories_are_registered_once_and_answered_by_their_own_slot() {
    let fx = fixture().await;
    let (_a, shop) = folder("shop");
    let (_b, blog) = folder("blog");

    let (status, one) = fx
        .call("POST", "/api/repos", Some(json!({"folder": shop})))
        .await;
    assert_eq!(status, StatusCode::OK, "{one}");
    let (_, again) = fx
        .call("POST", "/api/repos", Some(json!({"folder": shop})))
        .await;
    assert_eq!(one["id"], again["id"], "a repeat is the same repository");

    // With one repository registered a request that names none is its.
    let (status, _) = fx.call("GET", "/api/state", None).await;
    assert_eq!(status, StatusCode::OK);

    let (_, two) = fx
        .call("POST", "/api/repos", Some(json!({"folder": blog})))
        .await;
    assert_ne!(one["id"], two["id"]);
    let (status, body) = fx.call("GET", "/api/state", None).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "several repositories: name one: {body}"
    );

    // By id, by path, and by a folder inside the repository.
    let id = one["id"].as_str().unwrap();
    for key in [id.to_string(), shop.clone(), format!("{shop}/sub")] {
        let uri = format!("/api/state?repo={}", key.replace('/', "%2F"));
        let (status, _) = fx.call("GET", &uri, None).await;
        assert_eq!(status, StatusCode::OK, "{key}");
    }
    let (status, _) = fx.call("GET", "/api/state?repo=%2Fnowhere", None).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "an unregistered path is never opened"
    );

    // Their change files are their own.
    assert_ne!(one["dir"], two["dir"]);
    assert!(
        Path::new(one["dir"].as_str().unwrap())
            .join("changes")
            .exists()
    );
}

#[tokio::test]
async fn a_sync_serves_exactly_the_layouts_folders() {
    let fx = fixture().await;
    let (_a, shop) = folder("shop");
    let (_b, blog) = folder("blog");
    let (_c, gone) = folder("gone");
    let (status, _) = fx
        .call(
            "PUT",
            "/api/repos",
            Some(json!({"folders": [shop, blog, gone]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, listed) = fx.call("GET", "/api/repos", None).await;
    assert_eq!(listed["repos"].as_array().unwrap().len(), 3);

    let (_, synced) = fx
        .call(
            "PUT",
            "/api/repos",
            Some(json!({"folders": [shop, blog, "/not/here"]})),
        )
        .await;
    assert_eq!(
        synced["repos"].as_array().unwrap().len(),
        2,
        "the closed space's repository is dropped"
    );
    assert_eq!(synced["failed"].as_array().unwrap().len(), 1, "{synced}");
    let uri = format!("/api/state?repo={}", gone.replace('/', "%2F"));
    assert_eq!(fx.call("GET", &uri, None).await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_request_without_the_token_learns_nothing_about_repositories() {
    let fx = fixture().await;
    let (_a, shop) = folder("shop");
    fx.call("POST", "/api/repos", Some(json!({"folder": shop})))
        .await;
    for uri in ["/api/state?repo=nope", "/api/repos"] {
        let response = fx
            .service
            .registry()
            .router()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header("host", "127.0.0.1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{uri}");
    }
    drop(fx.state);
}

#[tokio::test]
async fn the_repository_list_carries_each_ones_task_count() {
    let fx = fixture().await;
    let (_a, shop) = folder("shop");
    fx.call("POST", "/api/repos", Some(json!({"folder": shop})))
        .await;
    let (status, listed) = fx.call("GET", "/api/repos", None).await;
    assert_eq!(status, StatusCode::OK);
    let repos = listed["repos"].as_array().unwrap();
    assert_eq!(repos.len(), 1);
    assert_eq!(
        repos[0]["tasks"], 0,
        "a repository with no task says zero, so a window shows its empty state at once: {listed}"
    );
}

/// A factory whose repositories count how often their space was prepared.
#[derive(Debug)]
struct Counting(Arc<std::sync::atomic::AtomicUsize>);

impl RepoFactory for Counting {
    fn root_of(&self, folder: &Path) -> PathBuf {
        folder.to_path_buf()
    }

    fn build(
        &self,
        _root: &Path,
        _folder: &Path,
        _hub: shprd_server::hub::Hub,
        _dir: &Path,
    ) -> Result<RepoParts, String> {
        let count = Arc::clone(&self.0);
        Ok(RepoParts {
            prepare: Some(Arc::new(move || {
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            })),
            ..RepoParts::default()
        })
    }
}

#[tokio::test]
async fn showing_a_repository_again_prepares_its_space_again() {
    // SHPR-80: a folder's space closed and added again is shown again; the window tells the
    // service, and the coordinator of the new space is started (the step is idempotent).
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let state = tempfile::tempdir().unwrap();
    let service = Service::start(Config::service(
        state.path().join("shprd"),
        Arc::new(Counting(Arc::clone(&count))),
    ))
    .await
    .expect("the service starts");
    let (_dir, shop) = folder("shop");
    let token = std::fs::read_to_string(&service.info().token_path)
        .unwrap()
        .trim()
        .to_string();
    for _ in 0..2 {
        let request = Request::builder()
            .method("POST")
            .uri("/api/repos")
            .header("host", "127.0.0.1")
            .header("authorization", format!("Bearer {token}"))
            .header("x-shprd", "1")
            .header("content-type", "application/json")
            .body(Body::from(json!({"folder": shop}).to_string()))
            .unwrap();
        let response = service.registry().router().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        // The step runs on its own thread: let it finish before the next show.
        for _ in 0..100 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            if count.load(std::sync::atomic::Ordering::SeqCst) > 0 {
                break;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    }
    assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 2);
}

/// A factory whose repositories count how often they were retired.
#[derive(Debug)]
struct Retiring(Arc<std::sync::atomic::AtomicUsize>);

impl RepoFactory for Retiring {
    fn root_of(&self, folder: &Path) -> std::path::PathBuf {
        folder.to_path_buf()
    }

    fn build(
        &self,
        _root: &Path,
        _folder: &Path,
        _hub: shprd_server::hub::Hub,
        _dir: &Path,
    ) -> Result<RepoParts, String> {
        let count = Arc::clone(&self.0);
        Ok(RepoParts {
            retire: Some(Arc::new(move || {
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            })),
            ..RepoParts::default()
        })
    }
}

#[tokio::test]
async fn a_repository_whose_space_closed_is_retired_and_one_still_shown_is_not() {
    // SHPR-86: the window's layout sync no longer lists a closed space's folder;
    // the service drops the repository and stops the agents it started for it.
    let retired = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let state = tempfile::tempdir().unwrap();
    let service = Service::start(Config::service(
        state.path().join("shprd"),
        Arc::new(Retiring(Arc::clone(&retired))),
    ))
    .await
    .expect("the service starts");
    let (_a, shop) = folder("shop");
    let (_b, blog) = folder("blog");
    let registry = service.registry();
    registry.sync(&[shop.clone().into(), blog.clone().into()]);
    registry.sync(&[shop.clone().into()]);
    for _ in 0..100 {
        if retired.load(std::sync::atomic::Ordering::SeqCst) > 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    assert_eq!(
        retired.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "only the folder that went away is retired"
    );
    assert_eq!(registry.list().len(), 1);
}
