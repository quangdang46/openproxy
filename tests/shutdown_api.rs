use std::net::TcpListener;
use std::process::Stdio;
use std::time::{Duration, Instant};

use reqwest::StatusCode;
use serde_json::json;
use tempfile::TempDir;
use tokio::process::{Child, Command};

struct ServerHandle {
    _temp_dir: TempDir,
    child: Child,
    client: reqwest::Client,
    base_url: String,
}

impl ServerHandle {
    async fn start(node_env: &str, shutdown_secret: Option<&str>) -> Self {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let port = free_port();
        let bin = std::env::var("CARGO_BIN_EXE_openproxy").expect("openproxy binary path");

        let mut command = Command::new(bin);
        command
            .env("HOST", "127.0.0.1")
            .env("PORT", port.to_string())
            .env("DATA_DIR", temp_dir.path())
            .env("NODE_ENV", node_env)
            .env("RUST_LOG", "error")
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        if let Some(secret) = shutdown_secret {
            command.env("SHUTDOWN_SECRET", secret);
        } else {
            command.env_remove("SHUTDOWN_SECRET");
        }

        let child = command.spawn().expect("spawn openproxy");
        let client = reqwest::Client::new();
        let base_url = format!("http://127.0.0.1:{port}");
        wait_for_ready(&client, &base_url).await;

        Self {
            _temp_dir: temp_dir,
            child,
            client,
            base_url,
        }
    }

    async fn post_shutdown(&self, authorization: Option<&str>) -> reqwest::Response {
        let mut request = self.client.post(format!("{}/api/shutdown", self.base_url));
        if let Some(authorization) = authorization {
            request = request.header("authorization", authorization);
        }
        request.send().await.expect("shutdown request")
    }

    async fn get(&self, path: &str) -> reqwest::Response {
        self.client
            .get(format!("{}{}", self.base_url, path))
            .send()
            .await
            .expect("get request")
    }

    fn assert_running(&mut self) {
        assert!(
            self.child.try_wait().expect("try_wait").is_none(),
            "server exited unexpectedly"
        );
    }

    async fn wait_for_exit(&mut self, timeout: Duration) -> std::process::ExitStatus {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait().expect("try_wait") {
                return status;
            }
            if Instant::now() >= deadline {
                panic!("server did not exit within {:?}", timeout);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn stop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await;
        }
    }
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind free port");
    let port = listener.local_addr().expect("local addr").port();
    drop(listener);
    port
}

async fn wait_for_ready(client: &reqwest::Client, base_url: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(response) = client.get(format!("{base_url}/health")).send().await {
            if response.status().is_success() {
                return;
            }
        }

        if Instant::now() >= deadline {
            panic!("server did not become ready at {base_url}");
        }

        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn shutdown_route_returns_401_when_secret_missing_like_9router() {
    let mut server = ServerHandle::start("development", None).await;
    let response = server.post_shutdown(None).await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response.json::<serde_json::Value>().await.unwrap(),
        json!({
            "success": false,
            "message": "Unauthorized"
        })
    );
    server.assert_running();
    server.stop().await;
}

#[tokio::test]
async fn shutdown_route_returns_403_in_production_before_auth() {
    let mut server = ServerHandle::start("production", Some("topsecret")).await;
    let response = server.post_shutdown(Some("Bearer topsecret")).await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        response.json::<serde_json::Value>().await.unwrap(),
        json!({
            "success": false,
            "message": "Not allowed in production"
        })
    );
    server.assert_running();
    server.stop().await;
}

#[tokio::test]
async fn shutdown_route_exits_process_after_successful_dev_request() {
    let mut server = ServerHandle::start("development", Some("topsecret")).await;
    let response = server.post_shutdown(Some("Bearer topsecret")).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.json::<serde_json::Value>().await.unwrap(),
        json!({
            "success": true,
            "message": "Shutting down..."
        })
    );

    let status = server.wait_for_exit(Duration::from_secs(5)).await;
    assert_eq!(status.code(), Some(0));
}

#[tokio::test]
async fn shutdown_legacy_routes_are_not_exposed() {
    let mut server = ServerHandle::start("development", Some("topsecret")).await;

    let status_response = server.get("/api/shutdown/status").await;
    assert_eq!(status_response.status(), StatusCode::NOT_FOUND);

    let health_response = server.get("/api/shutdown/health").await;
    assert_eq!(health_response.status(), StatusCode::NOT_FOUND);

    server.stop().await;
}
