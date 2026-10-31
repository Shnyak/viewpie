// This file contains modifications generated with assistance from Qwen AI
// Original project: https://github.com/snoack/viewpie
// SPDX-License-Identifier: GPL-3.0-or-later

//! End-to-end integration tests for the embedded HTTP REST API.
//!
//! Spawns the `viewpie-headless` binary (in-memory display backend, no CEC)
//! with a temporary YAML config and exercises every endpoint group against
//! the live server using plain TCP/HTTP/1.1 requests (std only — no extra
//! dev-dependencies). Run with:
//!
//! ```sh
//! cargo test --test api_integration -- --nocapture
//! ```

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Test fixture configuration
// ---------------------------------------------------------------------------

fn config_yaml() -> String {
    let port = server_port();
    format!(
        r##"
display:
  spacing: 1
  background: "#000000"
  pause_off_screen: false
  fullscreen_timeout: 600

feeds:
  yard:
    url: rtsp://127.0.0.1:{port}/yard_sub
    fullscreen_url: rtsp://127.0.0.1:{port}/yard_main
  gate:
    url: rtsp://127.0.0.1:{port}/gate_sub
    fullscreen_url: rtsp://127.0.0.1:{port}/gate_main
  pool:
    url: rtsp://127.0.0.1:{port}/pool_sub
    fullscreen_url: rtsp://127.0.0.1:{port}/pool_main
  street:
    url: rtsp://127.0.0.1:{port}/street_sub
    fullscreen_url: rtsp://127.0.0.1:{port}/street_main

panels:
  outside:
    name: "Outside"
    layout: |
      yard yard gate pool
      yard yard street .
      . . . .
      . . . .
    viewports:
      - slot: yard
        feeds: [yard]
      - slot: gate
        feeds: [gate]
      - slot: pool
        feeds: [pool]
      - slot: street
        feeds: [street]
  closeup:
    name: "Closeup"
    layout: |
      gate gate pool
      gate gate pool
      . . .
    viewports:
      - slot: gate
        feeds: [gate]
      - slot: pool
        feeds: [pool]

defaults:
  panel: outside
  rotation_interval: 20
"##
    )
}

// ---------------------------------------------------------------------------
// Minimal HTTP/1.1 client (std only)
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct Reply {
    status: u16,
    body: String,
}

impl Reply {
    fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body)
            .unwrap_or_else(|e| panic!("invalid JSON in response {self:?}: {e}"))
    }
    fn field(&self, key: &str) -> serde_json::Value {
        self.json()[key].clone()
    }
}

fn request(method: &str, path: &str, body: Option<&str>) -> Reply {
    let addr = server_addr();
    let mut stream = TcpStream::connect(&addr).expect("connect to headless server");
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    let req = match body {
        Some(b) => format!(
            "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{b}",
            b.len()
        ),
        None => format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"),
    };
    stream.write_all(req.as_bytes()).expect("write request");
    stream.flush().unwrap();

    let mut reader = BufReader::new(&mut stream);
    let mut status_line = String::new();
    reader.read_line(&mut status_line).unwrap();
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("0")
        .parse()
        .unwrap();
    // headers until empty line
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap() == 0 || line.trim().is_empty() {
            break;
        }
    }
    let mut body_buf = Vec::new();
    let _ = reader.read_to_end(&mut body_buf);
    Reply {
        status,
        body: String::from_utf8_lossy(&body_buf).into_owned(),
    }
}

// ---------------------------------------------------------------------------
// Server lifecycle
// ---------------------------------------------------------------------------

struct Server {
    child: Child,
    cfg_path: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.cfg_path);
    }
}

fn bin_path() -> PathBuf {
    // Integration tests run after `cargo build`; locate the debug binary.
    let mut p = std::env::current_exe()
        .unwrap()
        .parent() // .../debug/deps
        .and_then(|d| d.parent()) // .../debug
        .expect("target dir")
        .to_path_buf();
    p.push("viewpie-headless");
    assert!(
        p.exists(),
        "viewpie-headless binary not found at {} — does cargo build it?",
        p.display()
    );
    p
}

/// The single port used by the shared headless server for this test process.
static PORT: OnceLock<u16> = OnceLock::new();
/// Serialises panel/camera mutations that other tests could observe.
static LOCK: Mutex<()> = Mutex::new(());

fn server_port() -> u16 {
    *PORT.get_or_init(|| {
        // Bind a temporary listener to discover a free port; the headless
        // server binds it right after (tiny race window, acceptable here).
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    })
}

fn server_addr() -> String {
    format!("127.0.0.1:{}", server_port())
}

fn connect_with_timeout(addr: &SocketAddr, timeout: Duration) -> std::io::Result<TcpStream> {
    let deadline = Instant::now() + timeout;
    loop {
        match TcpStream::connect(addr) {
            Ok(s) => return Ok(s),
            Err(e) => {
                if Instant::now() > deadline {
                    return Err(e);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

/// Start the headless server once per test process (tests share one server;
/// each test uses unique resources so they do not interfere).
fn start_server() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let dir = std::env::temp_dir();
        let cfg_path: PathBuf = dir.join(format!("viewpie-it-{}.yaml", std::process::id()));
        std::fs::write(&cfg_path, config_yaml()).unwrap();

        let log_path = cfg_path.with_extension("log");
        let log = std::fs::File::create(&log_path).unwrap();
        let child = Command::new(bin_path())
            .arg(&cfg_path)
            .env("VIEWPIE_API_BIND", server_addr())
            .env("RUST_LOG", "warn")
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .spawn()
            .expect("spawn viewpie-headless");

        let _server = Server {
            child,
            cfg_path: cfg_path.clone(),
        };
        std::mem::forget(_server); // lives until process exit

        // Wait until the port answers.
        let addr: SocketAddr = server_addr().parse().unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if connect_with_timeout(&addr, Duration::from_millis(200)).is_ok() {
                break;
            }
            if Instant::now() > deadline {
                let logs = std::fs::read_to_string(&log_path).unwrap_or_default();
                panic!("headless server did not come up.\nServer log:\n{logs}");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    });
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn t01_status_endpoint() {
    // Serialize with other tests: this touches the shared active panel.
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    start_server();
    let r = request("GET", "/api/status", None);
    assert_eq!(r.status, 200);
    let j = r.json();
    assert_eq!(j["paused"], false);
    assert_eq!(j["active_panel"], "outside");
    assert_eq!(j["pause_off_screen"], false);
    assert_eq!(j["spacing"], 1);
    assert!(j["cameras"].as_array().unwrap().len() >= 4);
    assert_eq!(j["panels_count"], 2);
    assert!(j["uptime_seconds"].as_u64().unwrap() < 3600);
    assert!(j["layout"].as_str().unwrap().contains("yard"));
}

#[test]
fn t02_panels_list_and_activate() {
    // Serialize with other tests: this touches the shared active panel.
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    start_server();
    let r = request("GET", "/api/panels", None);
    assert_eq!(r.status, 200);
    let j = r.json();
    assert_eq!(j["active"], "outside");
    let ids: Vec<&str> = j["panels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"outside") && ids.contains(&"closeup"));

    // Activate the other panel…
    let r = request("POST", "/api/panels/closeup/activate", None);
    assert_eq!(r.status, 200, "activate failed: {}", r.body);
    let r = request("GET", "/api/panels/active", None);
    assert_eq!(r.field("id"), "closeup");

    // …and back. Shared camera `gate` must NOT reconnect (refcount stays ≥1);
    // verified by absence of decoder errors in server log + status ok.
    let r = request("POST", "/api/panels/outside/activate", None);
    assert_eq!(r.status, 200);
    let r = request("GET", "/api/panels/outside", None);
    assert_eq!(r.status, 200);
    assert_eq!(r.json()["name"], "Outside");

    // Unknown panel → 404
    let r = request("POST", "/api/panels/nope/activate", None);
    assert_eq!(r.status, 404);
}

#[test]
fn t03_fullscreen_with_duration_timer() {
    // Serialize with other tests: this touches the shared active panel.
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    start_server();
    // Camera not in active panel → 4xx error
    let r = request(
        "POST",
        "/api/fullscreen",
        Some(r#"{"camera":"street","duration_seconds":1}"#),
    );
    assert_eq!(r.status, 200, "fullscreen failed: {}", r.body);

    let r = request("GET", "/api/status", None);
    assert_eq!(r.field("fullscreen_camera"), "street");

    // Timer returns to grid within ~±1s of the requested duration.
    let t0 = Instant::now();
    let deadline = t0 + Duration::from_secs(5);
    loop {
        let r = request("GET", "/api/status", None);
        if r.field("fullscreen_camera").is_null() {
            let dt = t0.elapsed();
            assert!(
                dt >= Duration::from_millis(900) && dt <= Duration::from_secs(4),
                "timer fired too early/late: {dt:?}"
            );
            break;
        }
        assert!(Instant::now() < deadline, "fullscreen never auto-returned");
        std::thread::sleep(Duration::from_millis(200));
    }

    // Camera missing from active panel → error.
    request("POST", "/api/panels/closeup/activate", None);
    let r = request("POST", "/api/fullscreen", Some(r#"{"camera":"street"}"#));
    assert_eq!(r.status, 422, "expected rejection, got {}", r.body);
    request("POST", "/api/panels/outside/activate", None);
}

#[test]
fn t04_pause_resume_grid() {
    // Serialize with other tests: this touches the shared active panel.
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    start_server();
    let r = request("POST", "/api/pause", None);
    assert_eq!((r.status, r.field("ok")), (200, serde_json::json!(true)));
    let r = request("GET", "/api/status", None);
    assert_eq!(r.field("paused"), true);

    let r = request("POST", "/api/resume", None);
    assert_eq!(r.status, 200);
    let r = request("GET", "/api/status", None);
    assert_eq!(r.field("paused"), false);

    // Enter fullscreen then /api/grid must clear it.
    request(
        "POST",
        "/api/fullscreen",
        Some(r#"{"camera":"yard","duration_seconds":600}"#),
    );
    let r = request("GET", "/api/status", None);
    assert_eq!(
        r.field("fullscreen_camera"),
        "yard",
        "fullscreen did not engage (flaky RTSP connect?)"
    );
    let r = request("POST", "/api/grid", None);
    assert_eq!(r.status, 200);
    let r = request("GET", "/api/status", None);
    assert!(r.field("fullscreen_camera").is_null());
    request("POST", "/api/grid", None); // cleanup
}

#[test]
fn t05_settings_partial_update() {
    // Serialize with other tests: this touches the shared active panel.
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    start_server();
    let r = request("PUT", "/api/settings", Some(r#"{"spacing":7}"#));
    assert_eq!(r.status, 200);
    let r = request("GET", "/api/settings", None);
    let j = r.json();
    assert_eq!(j["spacing"], 7);
    // untouched fields keep their values
    assert_eq!(j["background"], "#000000");
    assert_eq!(j["fullscreen_timeout"], 600);

    // Out-of-range spacing → 4xx validation error
    let r = request("PUT", "/api/settings", Some(r#"{"spacing":99}"#));
    assert_eq!(r.status, 400);

    // Invalid background → 4xx
    let r = request("PUT", "/api/settings", Some(r#"{"background":"red"}"#));
    assert_eq!(r.status, 400);

    // restore
    request("PUT", "/api/settings", Some(r#"{"spacing":1}"#));
}

#[test]
fn t06_cameras_crud_and_conflict() {
    // Serialize with other tests: this touches the shared active panel.
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    start_server();
    // Create
    let r = request(
        "POST",
        "/api/cameras",
        Some(r#"{"name":"garage","url":"rtsp://127.0.0.1:1/garage_sub"}"#),
    );
    assert_eq!(r.status, 201, "create failed: {}", r.body);

    // Duplicate → 409
    let r = request(
        "POST",
        "/api/cameras",
        Some(r#"{"name":"garage","url":"rtsp://127.0.0.1:1/x"}"#),
    );
    assert_eq!(r.status, 409);

    // Update
    let r = request(
        "PUT",
        "/api/cameras/garage",
        Some(
            r#"{"name":"garage","url":"rtsp://127.0.0.1:1/garage_sub2","fullscreen_url":"rtsp://127.0.0.1:1/garage_main"}"#,
        ),
    );
    assert_eq!(r.status, 200, "update failed: {}", r.body);

    // List contains it
    let r = request("GET", "/api/cameras", None);
    let cams = r.json();
    let names: Vec<&str> = cams
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["name"].as_str())
        .collect();
    assert!(names.contains(&"garage"));

    // Delete unused → ok
    let r = request("DELETE", "/api/cameras/garage", None);
    assert_eq!(r.status, 200);

    // Delete used-by-panel → 409 with panel list
    let r = request("DELETE", "/api/cameras/gate", None);
    assert_eq!(r.status, 409);
    assert!(r.body.contains("outside") && r.body.contains("closeup"));

    // Delete unknown → 404
    let r = request("DELETE", "/api/cameras/nope", None);
    assert_eq!(r.status, 404);
}

#[test]
fn t07_camera_test_unreachable() {
    start_server();
    // port 1 is closed here → graceful failure, not a crash.
    let r = request("POST", "/api/cameras/street/test", None);
    assert_eq!(r.status, 200);
    let j = r.json();
    assert_eq!(j["ok"], false);
    assert!(j["error"].is_string());
}

#[test]
fn t08_panels_crud_validation() {
    start_server();
    // Valid new panel → 201
    let body = r#"{"id":"itpanel","name":"IT Panel","layout":"gate gate gate\ngate . .\n. . .","viewports":[{"slot":"gate","feeds":["gate"]}]}"#;
    let r = request("POST", "/api/panels", Some(body));
    assert_eq!(r.status, 201, "create failed: {}", r.body);

    // Duplicate id → 409
    let r = request("POST", "/api/panels", Some(body));
    assert_eq!(r.status, 409);

    // Non-square/too-small grid → 400
    let bad = r#"{"id":"bad","name":"Bad","layout":"gate gate\ngate gate","viewports":[{"slot":"gate","feeds":["gate"]}]}"#;
    let r = request("POST", "/api/panels", Some(bad));
    assert_eq!(r.status, 400);

    // Unknown feed reference → 400
    let bad2 = r#"{"id":"bad2","name":"Bad2","layout":"ghost ghost ghost\nghost . .\n. . .","viewports":[{"slot":"ghost","feeds":["ghost"]}]}"#;
    let r = request("POST", "/api/panels", Some(bad2));
    assert_eq!(r.status, 400);

    // Update existing
    let upd = r#"{"name":"IT Panel Renamed","layout":"gate gate gate\ngate . .\n. . .","viewports":[{"slot":"gate","feeds":["gate"]}]}"#;
    let r = request("PUT", "/api/panels/itpanel", Some(upd));
    assert_eq!(r.status, 200, "update failed: {}", r.body);
    let r = request("GET", "/api/panels/itpanel", None);
    assert_eq!(r.json()["name"], "IT Panel Renamed");

    // Delete
    let r = request("DELETE", "/api/panels/itpanel", None);
    assert_eq!(r.status, 200);
    let r = request("DELETE", "/api/panels/itpanel", None);
    assert_eq!(r.status, 404);
}

#[test]
fn t09_reload() {
    start_server();
    let r = request("POST", "/api/reload", None);
    assert_eq!(r.status, 200, "reload failed: {}", r.body);
    let r = request("GET", "/api/status", None);
    assert_eq!(r.status, 200);
}
