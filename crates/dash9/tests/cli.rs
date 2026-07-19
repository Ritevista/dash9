//! Black-box regression coverage for the `dash9` CLI. `test` cases
//! run the real compiled binary against a throwaway local TCP
//! listener that hand-rolls one canned HTTP response, standing in
//! for Prometheus — no live network access, no mocking dependency,
//! fully offline and deterministic (same technique used in
//! `dash9-prom`'s own tests).

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;

/// Starts a background thread that accepts exactly one TCP
/// connection, ignores whatever request it reads, and always writes
/// back `body` as a `200 OK` JSON response. Returns the base URL to
/// point a `PrometheusDatasource` (or the CLI) at.
fn fake_prometheus(body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    std::thread::spawn(move || {
        if let Ok((mut socket, _)) = listener.accept() {
            let mut buf = [0u8; 4096];
            let _ = socket.read(&mut buf);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = socket.write_all(response.as_bytes());
        }
    });
    format!("http://{addr}")
}

fn write_dashboard(
    dir: &Path,
    base_url: &str,
    query: &str,
    panel_type: &str,
    allow_empty: bool,
) -> PathBuf {
    let path = dir.join("dashboard.toml");
    let toml = format!(
        r#"
[dashboard]
title = "t"
refresh = "30s"
default_range = "5m"

[[datasources]]
name = "prom"
type = "prometheus"
url = "{base_url}"

[[panels]]
title = "p"
type = "{panel_type}"
datasource = "prom"
query = "{query}"
allow_empty = {allow_empty}
grid = {{ row = 0, col = 0, w = 1, h = 1 }}
"#
    );
    std::fs::write(&path, toml).expect("write dashboard fixture");
    path
}

#[test]
fn passing_panel_exits_zero() {
    let body = r#"{"status":"success","data":{"resultType":"vector","result":[{"metric":{},"value":[1700000000.0,"1"]}]}}"#;
    let base_url = fake_prometheus(body);
    let dir = tempfile::tempdir().unwrap();
    let path = write_dashboard(dir.path(), &base_url, "up", "stat", false);

    Command::cargo_bin("dash9")
        .unwrap()
        .args(["test", path.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("PASS"))
        .stdout(predicate::str::contains("all panels passed"));
}

#[test]
fn unexpectedly_empty_result_exits_one() {
    let body = r#"{"status":"success","data":{"resultType":"vector","result":[]}}"#;
    let base_url = fake_prometheus(body);
    let dir = tempfile::tempdir().unwrap();
    let path = write_dashboard(dir.path(), &base_url, "nonexistent_metric", "stat", false);

    Command::cargo_bin("dash9")
        .unwrap()
        .args(["test", path.to_str().unwrap()])
        .assert()
        .code(1)
        .stdout(predicate::str::contains("FAIL"))
        .stdout(predicate::str::contains("no data"));
}

#[test]
fn allow_empty_panel_still_exits_zero_on_empty_result() {
    let body = r#"{"status":"success","data":{"resultType":"vector","result":[]}}"#;
    let base_url = fake_prometheus(body);
    let dir = tempfile::tempdir().unwrap();
    let path = write_dashboard(dir.path(), &base_url, "nonexistent_metric", "table", true);

    Command::cargo_bin("dash9")
        .unwrap()
        .args(["test", path.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("PASS"));
}

#[test]
fn datasource_api_error_exits_one() {
    let body = r#"{"status":"error","errorType":"bad_data","error":"invalid query"}"#;
    let base_url = fake_prometheus(body);
    let dir = tempfile::tempdir().unwrap();
    let path = write_dashboard(dir.path(), &base_url, "up{", "stat", false);

    Command::cargo_bin("dash9")
        .unwrap()
        .args(["test", path.to_str().unwrap()])
        .assert()
        .code(1)
        .stdout(predicate::str::contains("FAIL"));
}

#[test]
fn invalid_dashboard_file_exits_two_without_attempting_panels() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad.toml");
    std::fs::write(&path, "not = [valid").unwrap();

    Command::cargo_bin("dash9")
        .unwrap()
        .args(["test", path.to_str().unwrap()])
        .assert()
        .code(2)
        .stdout(predicate::str::contains("PASS").not())
        .stdout(predicate::str::contains("FAIL").not());
}

#[test]
fn missing_dashboard_file_exits_two() {
    Command::cargo_bin("dash9")
        .unwrap()
        .args(["test", "/nonexistent/dash9-cli-test-fixture.toml"])
        .assert()
        .code(2);
}
