// Kurnace desktop shell (spec §4, Phase 2).
//
// The app is a client of the core (`kurn daemon`) over its local socket, so a
// headless core started by an agent and the app share one writer (spec §4.2).
// This backend:
//   - starts or attaches to the core;
//   - relays JSON calls (`core_call`) and streams module progress;
//   - relays live doc updates + op records (`doc_subscribe` → "kurn-doc" events);
//   - serves library assets over loopback HTTP with Range support (see asset server);
//   - hosts terminal panes for agents (`pty_*`).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use base64::Engine;
use portable_pty::{CommandBuilder, MasterPty, NativePtySystem, PtySize, PtySystem};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State};

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

fn actor() -> String {
    std::env::var("KURN_ACTOR").unwrap_or_else(|_| {
        format!(
            "human:{}",
            std::env::var("USER").unwrap_or_else(|_| "user".into())
        )
    })
}

// ---------- settings / Fish Audio ----------
//
// The API key lives in the OS credential store (macOS Keychain / Windows
// Credential Manager). Non-secret defaults live in ~/.kurnace/settings.json.
const FISH_SERVICE: &str = "dev.kurnace.app.fish";
const FISH_ACCOUNT: &str = "api-key";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct FishPrefs {
    #[serde(default)]
    voice: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    speed: Option<f64>,
}

fn settings_file() -> PathBuf {
    kurn_core::core::socket_path().with_file_name("settings.json")
}

fn read_settings() -> Value {
    std::fs::read_to_string(settings_file())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| json!({}))
}

fn write_settings(v: &Value) -> Result<(), String> {
    let f = settings_file();
    if let Some(dir) = f.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(f, serde_json::to_vec_pretty(v).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}

fn fish_prefs() -> FishPrefs {
    let v = read_settings();
    serde_json::from_value(v.get("fish").cloned().unwrap_or(json!({}))).unwrap_or_default()
}

fn fish_key() -> Option<String> {
    keyring::Entry::new(FISH_SERVICE, FISH_ACCOUNT)
        .ok()?
        .get_password()
        .ok()
        .filter(|k| !k.trim().is_empty())
}

fn fish_status() -> Value {
    let key = fish_key();
    let prefs = fish_prefs();
    json!({
        "configured": key.is_some(),
        "hint": key.as_ref().map(|k| {
            let tail: String = k.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
            if k.chars().count() > 4 { format!("…{tail}") } else { "set".into() }
        }),
        "voice": prefs.voice,
        "model": prefs.model,
        "speed": prefs.speed.unwrap_or(1.0),
    })
}

fn inject_fish_env(params: &mut Value) -> Result<(), String> {
    let mut env = params
        .get("env")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if let Some(k) = fish_key() {
        env.insert("FISH_API_KEY".into(), json!(k));
    }
    let p = fish_prefs();
    if !p.voice.trim().is_empty() {
        env.insert("KURN_FISH_VOICE".into(), json!(p.voice));
    }
    if !p.model.trim().is_empty() {
        env.insert("KURN_FISH_MODEL".into(), json!(p.model));
    }
    if let Some(s) = p.speed {
        env.insert("KURN_FISH_SPEED".into(), json!(s));
    }
    params["env"] = Value::Object(env);
    Ok(())
}

#[tauri::command]
fn fish_status_cmd() -> Value {
    fish_status()
}

#[tauri::command]
fn fish_save(
    api_key: Option<String>,
    voice: Option<String>,
    model: Option<String>,
    speed: Option<f64>,
) -> Result<Value, String> {
    if let Some(key) = api_key.filter(|k| !k.trim().is_empty()) {
        keyring::Entry::new(FISH_SERVICE, FISH_ACCOUNT)
            .map_err(|e| e.to_string())?
            .set_password(key.trim())
            .map_err(|e| e.to_string())?;
    }
    let mut v = read_settings();
    let mut fish = v.get("fish").cloned().unwrap_or(json!({}));
    if let Some(x) = voice {
        fish["voice"] = json!(x);
    }
    if let Some(x) = model {
        fish["model"] = json!(x);
    }
    if let Some(x) = speed {
        fish["speed"] = json!(x);
    }
    v["fish"] = fish;
    write_settings(&v)?;
    Ok(fish_status())
}

#[tauri::command]
fn fish_clear() -> Result<Value, String> {
    if let Ok(e) = keyring::Entry::new(FISH_SERVICE, FISH_ACCOUNT) {
        let _ = e.delete_credential();
    }
    Ok(fish_status())
}

/// Find the `kurn` binary: $KURN_BIN, next to this executable, or a dev build up the tree.
fn kurn_bin() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("KURN_BIN") {
        return Some(PathBuf::from(p));
    }
    let exe = std::env::current_exe().ok()?;
    let bin = if cfg!(windows) { "kurn.exe" } else { "kurn" };
    let mut d = exe.parent().map(PathBuf::from);
    while let Some(dir) = d {
        for cand in [
            dir.join(bin),
            dir.join("target/debug").join(bin),
            dir.join("target/release").join(bin),
        ] {
            if cand.is_file() {
                return Some(cand);
            }
        }
        d = dir.parent().map(PathBuf::from);
    }
    None
}

fn connect() -> Result<kurn_core::transport::Conn, String> {
    let sock = kurn_core::core::socket_path();
    if let Ok(s) = kurn_core::transport::connect(&sock) {
        return Ok(s);
    }
    let bin = kurn_bin().ok_or("core not running and `kurn` binary not found (set KURN_BIN)")?;
    std::process::Command::new(bin)
        .arg("daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("starting core: {e}"))?;
    let t0 = Instant::now();
    loop {
        if let Ok(s) = kurn_core::transport::connect(&sock) {
            return Ok(s);
        }
        if t0.elapsed() > Duration::from_secs(10) {
            return Err(format!("core did not start on {}", sock.display()));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// One request on a fresh connection; streamed events go to `on_event`.
fn call(method: &str, params: Value, mut on_event: impl FnMut(Value)) -> Result<Value, String> {
    let s = connect()?;
    let mut w = s.try_clone().map_err(|e| e.to_string())?;
    writeln!(
        w,
        "{}",
        json!({ "id": 1, "method": method, "actor": actor(), "params": params })
    )
    .map_err(|e| e.to_string())?;
    for line in BufReader::new(s).lines() {
        let v: Value =
            serde_json::from_str(&line.map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        if let Some(ev) = v.get("event") {
            on_event(ev.clone());
            continue;
        }
        return if v["ok"] == json!(true) {
            Ok(v["result"].clone())
        } else {
            Err(v["error"].as_str().unwrap_or("error").to_string())
        };
    }
    Err("core closed the connection".into())
}

static CALL_ID: AtomicU64 = AtomicU64::new(1);

#[tauri::command]
async fn core_call(app: AppHandle, method: String, params: Value) -> Result<Value, String> {
    let id = CALL_ID.fetch_add(1, Ordering::Relaxed);
    let mut params = params;
    if matches!(method.as_str(), "run" | "lib.variant") {
        inject_fish_env(&mut params)?;
    }
    tauri::async_runtime::spawn_blocking(move || {
        call(&method, params, |ev| {
            let _ = app.emit(
                "kurn-job",
                json!({ "call": id, "method": method, "event": ev }),
            );
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Stream doc state/updates/op records for a project as "kurn-doc" events.
#[tauri::command]
fn doc_subscribe(app: AppHandle, project: String) -> Result<(), String> {
    let s = connect()?;
    let mut w = s.try_clone().map_err(|e| e.to_string())?;
    writeln!(w, "{}", json!({ "id": "sub", "method": "doc.subscribe", "actor": actor(), "params": { "project": project } })).map_err(|e| e.to_string())?;
    std::thread::spawn(move || {
        for line in BufReader::new(s).lines() {
            let Ok(line) = line else { break };
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let payload = if let Some(ev) = v.get("event") {
                json!({ "project": project, "kind": ev["type"], "event": ev })
            } else if v["ok"] == json!(true) {
                json!({ "project": project, "kind": "state", "event": v["result"] })
            } else {
                json!({ "project": project, "kind": "error", "event": v })
            };
            let _ = app.emit("kurn-doc", payload);
        }
        let _ = app.emit("kurn-doc", json!({ "project": project, "kind": "closed" }));
    });
    Ok(())
}

/// Project to open at launch: $KURN_PROJECT, else the last one opened.
#[tauri::command]
fn initial_project() -> Option<String> {
    // KURN_PROJECT keeps the dev/automation path; normal launches go to the
    // dashboard instead of silently reopening the last project.
    std::env::var("KURN_PROJECT")
        .ok()
        .and_then(|p| std::fs::canonicalize(p).ok())
        .map(|p| p.to_string_lossy().into_owned())
}

#[tauri::command]
fn remember_project(project: String) {
    let f = kurn_core::core::socket_path().with_file_name("app.json");
    let _ = std::fs::write(f, json!({ "last_project": project }).to_string());
}

/// Dev/test hook: write a data-URL PNG (Player frame capture) to disk.
#[tauri::command]
fn save_png(path: String, data_url: String) -> Result<(), String> {
    let b = B64
        .decode(data_url.trim_start_matches("data:image/png;base64,"))
        .map_err(|e| e.to_string())?;
    if let Some(dir) = std::path::Path::new(&path).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(path, b).map_err(|e| e.to_string())
}

#[tauri::command]
fn log_line(line: String) {
    println!("[webview] {line}");
}

/// Reveal an output file in Finder / the platform file manager.
#[tauri::command]
fn reveal_path(path: String) -> Result<(), String> {
    let p = PathBuf::from(&path);
    if !p.exists() {
        return Err(format!("not found: {path}"));
    }
    #[cfg(target_os = "macos")]
    let status = std::process::Command::new("open")
        .arg("-R")
        .arg(&p)
        .status();
    #[cfg(target_os = "windows")]
    let status = std::process::Command::new("explorer")
        .arg("/select,")
        .arg(&p)
        .status();
    #[cfg(all(unix, not(target_os = "macos")))]
    let status = std::process::Command::new("xdg-open")
        .arg(p.parent().unwrap_or(&p))
        .status();
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("file manager exited with {s}")),
        Err(e) => Err(format!("opening {path}: {e}")),
    }
}

// ---------- asset server ----------
//
// Library media is served over loopback HTTP, not a custom URL scheme: WebKit's
// media stack stalls on backward seeks of <video> loaded from a custom scheme
// (WKWebView, Revideo preview), while plain HTTP with Range works. Bound to
// 127.0.0.1 only, every URL carries a random per-launch token, and only library
// assets are served, by id.

/// id → (blob path, mime). Asset ids are stable forever, so caching is safe.
type AssetCache = Arc<Mutex<HashMap<String, (PathBuf, String)>>>;

fn resolve_asset(cache: &AssetCache, id: &str) -> Result<(PathBuf, String), String> {
    if let Some(hit) = cache.lock().unwrap().get(id) {
        return Ok(hit.clone());
    }
    let a = call("lib.get", json!({ "id": id }), |_| {})?;
    let path = PathBuf::from(a["path"].as_str().ok_or("asset has no path")?);
    let mime = a["mime"]
        .as_str()
        .unwrap_or("application/octet-stream")
        .to_string();
    cache
        .lock()
        .unwrap()
        .insert(id.to_string(), (path.clone(), mime.clone()));
    Ok((path, mime))
}

struct AssetServer {
    base: String,
    token: String,
}

fn parse_range(h: &str, len: u64) -> Option<(u64, u64)> {
    let r = h.trim().strip_prefix("bytes=")?;
    let (a, b) = r.split(',').next()?.split_once('-')?;
    let (start, end) = if a.is_empty() {
        let n: u64 = b.parse().ok()?;
        (len.saturating_sub(n), len.saturating_sub(1))
    } else {
        let s: u64 = a.parse().ok()?;
        let e: u64 = if b.is_empty() {
            len.saturating_sub(1)
        } else {
            b.parse::<u64>().ok()?.min(len.saturating_sub(1))
        };
        (s, e)
    };
    (start <= end && start < len).then_some((start, end))
}

fn handle_asset_conn(
    mut conn: std::net::TcpStream,
    token: &str,
    cache: &AssetCache,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(conn.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let mut range = None;
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h)? == 0 || h == "\r\n" || h == "\n" {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            if k.eq_ignore_ascii_case("range") {
                range = Some(v.trim().to_string());
            }
        }
    }
    let cors = "Access-Control-Allow-Origin: *\r\nAccess-Control-Allow-Headers: Range\r\nAccept-Ranges: bytes\r\nConnection: close\r\n";
    let fail = |conn: &mut std::net::TcpStream, code: &str| {
        write!(conn, "HTTP/1.1 {code}\r\n{cors}Content-Length: 0\r\n\r\n")
    };
    if method == "OPTIONS" {
        return fail(&mut conn, "204 No Content");
    }
    // /<token>/<id>[.<ext>]  ·  /<token>/thumbs/<id>  ·  /<token>/peaks/<id>
    let mut seg = target.trim_start_matches('/').splitn(2, '/');
    if seg.next() != Some(token) {
        return fail(&mut conn, "403 Forbidden");
    }
    let rest = seg.next().unwrap_or("");
    if let Some((what, tail)) = rest.split_once('/') {
        let id = tail.split('.').next().unwrap_or("");
        match what {
            "thumbs" => {
                let Ok(v) = call("lib.thumb", json!({ "id": id }), |_| {}) else {
                    return fail(&mut conn, "404 Not Found");
                };
                let (Some(path), Some(mime)) = (v["path"].as_str(), v["mime"].as_str()) else {
                    return fail(&mut conn, "404 Not Found");
                };
                let Ok(mut f) = std::fs::File::open(path) else {
                    return fail(&mut conn, "404 Not Found");
                };
                let len = f.metadata()?.len();
                write!(
                    conn,
                    "HTTP/1.1 200 OK\r\n{cors}Content-Type: {mime}\r\nContent-Length: {len}\r\n\r\n"
                )?;
                if method != "HEAD" {
                    std::io::copy(&mut f, &mut conn)?;
                }
                return conn.flush();
            }
            "peaks" => {
                let Ok(v) = call("lib.peaks", json!({ "id": id }), |_| {}) else {
                    return fail(&mut conn, "404 Not Found");
                };
                let body = serde_json::to_vec(&v)?;
                write!(
                    conn,
                    "HTTP/1.1 200 OK\r\n{cors}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                )?;
                if method != "HEAD" {
                    conn.write_all(&body)?;
                }
                return conn.flush();
            }
            "seq" => {
                let Some((sid, frame)) = tail.split_once('/') else {
                    return fail(&mut conn, "404 Not Found");
                };
                if frame.contains("..") || frame.contains('/') {
                    return fail(&mut conn, "404 Not Found");
                }
                let Ok(v) = call("lib.get", json!({ "id": sid }), |_| {}) else {
                    return fail(&mut conn, "404 Not Found");
                };
                let listed = v["frames"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|x| x.as_str() == Some(frame)));
                let (Some(dir), true) = (v["path"].as_str(), listed) else {
                    return fail(&mut conn, "404 Not Found");
                };
                let path = PathBuf::from(dir).join(frame);
                let Ok(mut f) = std::fs::File::open(&path) else {
                    return fail(&mut conn, "404 Not Found");
                };
                let mime = match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
                    "jpg" | "jpeg" => "image/jpeg",
                    "webp" => "image/webp",
                    _ => "image/png",
                };
                let len = f.metadata()?.len();
                write!(
                    conn,
                    "HTTP/1.1 200 OK\r\n{cors}Content-Type: {mime}\r\nContent-Length: {len}\r\n\r\n"
                )?;
                if method != "HEAD" {
                    std::io::copy(&mut f, &mut conn)?;
                }
                return conn.flush();
            }
            _ => return fail(&mut conn, "404 Not Found"),
        }
    }
    let id = rest.split('.').next().unwrap_or("").to_string();
    let Ok((path, mime)) = resolve_asset(cache, &id) else {
        return fail(&mut conn, "404 Not Found");
    };
    let Ok(mut f) = std::fs::File::open(&path) else {
        return fail(&mut conn, "404 Not Found");
    };
    let len = f.metadata()?.len();
    let (status, start, end) = match range.as_deref().map(|r| parse_range(r, len)) {
        Some(Some((s, e))) => ("206 Partial Content", s, e),
        Some(None) => {
            return write!(
                conn,
                "HTTP/1.1 416 Range Not Satisfiable\r\n{cors}Content-Range: bytes */{len}\r\nContent-Length: 0\r\n\r\n"
            );
        }
        None => ("200 OK", 0, len.saturating_sub(1)),
    };
    let n = if len == 0 { 0 } else { end - start + 1 };
    let mut head =
        format!("HTTP/1.1 {status}\r\n{cors}Content-Type: {mime}\r\nContent-Length: {n}\r\n");
    if status.starts_with("206") {
        head.push_str(&format!("Content-Range: bytes {start}-{end}/{len}\r\n"));
    }
    head.push_str("\r\n");
    conn.write_all(head.as_bytes())?;
    if method != "HEAD" && n > 0 {
        f.seek(SeekFrom::Start(start))?;
        std::io::copy(&mut f.take(n), &mut conn)?;
    }
    conn.flush()
}

fn start_asset_server() -> std::io::Result<AssetServer> {
    // Debug builds use a fixed port so the Vite dev server can proxy /kurn-asset
    // to it: media must be same-origin with the page, or WebKit stalls drawing
    // video frames after a seek. Release builds pick a free port.
    let port = std::env::var("KURN_ASSET_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(if cfg!(debug_assertions) { 1421 } else { 0 });
    let listener = std::net::TcpListener::bind(("127.0.0.1", port))?;
    let port = listener.local_addr()?.port();
    // 128 random bits from std's OS-seeded hasher keys.
    let token = {
        use std::hash::BuildHasher;
        let r = || std::collections::hash_map::RandomState::new().hash_one(std::process::id());
        format!("{:016x}{:016x}", r(), r())
    };
    let cache: AssetCache = Arc::default();
    let tok = token.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            let (tok, cache) = (tok.clone(), cache.clone());
            std::thread::spawn(move || {
                let _ = handle_asset_conn(conn, &tok, &cache);
            });
        }
    });
    Ok(AssetServer {
        base: format!("http://127.0.0.1:{port}/{token}"),
        token,
    })
}

/// Base URL for library assets: `<base>/<id>.<ext>`. In dev it is same-origin
/// (`/kurn-asset/<token>`, proxied by Vite to the asset server).
#[tauri::command]
fn asset_base(server: State<AssetServer>) -> String {
    if cfg!(debug_assertions) {
        format!("/kurn-asset/{}", server.token)
    } else {
        server.base.clone()
    }
}

// ---------- terminal panes ----------

struct Pty {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
}

#[derive(Default)]
struct Ptys(Mutex<HashMap<u64, Pty>>);
static PTY_ID: AtomicU64 = AtomicU64::new(1);

/// Spawn a command in a PTY. Output arrives as "pty-data" {id, data(base64)}; exit as "pty-exit".
#[tauri::command]
fn pty_spawn(
    app: AppHandle,
    ptys: State<Ptys>,
    cmd: Vec<String>,
    cwd: Option<String>,
    env: HashMap<String, String>,
    cols: u16,
    rows: u16,
) -> Result<u64, String> {
    let (prog, args) = cmd.split_first().ok_or("empty command")?;
    let pair = NativePtySystem::default()
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| e.to_string())?;
    let mut c = CommandBuilder::new(prog);
    c.args(args);
    if let Some(d) = cwd {
        c.cwd(d);
    }
    c.env("TERM", "xterm-256color");
    if let Some(dir) = kurn_bin().and_then(|b| b.parent().map(PathBuf::from)) {
        let path = std::env::var("PATH").unwrap_or_default();
        c.env("PATH", format!("{}:{path}", dir.display()));
    }
    let mut env = env;
    if let Some(key) = fish_key() {
        env.insert("FISH_API_KEY".into(), key);
    }
    let prefs = fish_prefs();
    if !prefs.voice.trim().is_empty() {
        env.insert("KURN_FISH_VOICE".into(), prefs.voice);
    }
    if !prefs.model.trim().is_empty() {
        env.insert("KURN_FISH_MODEL".into(), prefs.model);
    }
    if let Some(s) = prefs.speed {
        env.insert("KURN_FISH_SPEED".into(), s.to_string());
    }
    for (k, v) in env {
        c.env(k, v);
    }
    let child = pair
        .slave
        .spawn_command(c)
        .map_err(|e| format!("{prog}: {e}"))?;
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
    let writer = pair.master.take_writer().map_err(|e| e.to_string())?;
    let id = PTY_ID.fetch_add(1, Ordering::Relaxed);
    let app2 = app.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 16384];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let _ = app2.emit(
                        "pty-data",
                        json!({ "id": id, "data": B64.encode(&buf[..n]) }),
                    );
                }
            }
        }
        let _ = app2.emit("pty-exit", json!({ "id": id }));
    });
    ptys.0.lock().unwrap().insert(
        id,
        Pty {
            master: pair.master,
            writer,
            child,
        },
    );
    Ok(id)
}

#[tauri::command]
fn pty_write(ptys: State<Ptys>, id: u64, data: String) -> Result<(), String> {
    let mut m = ptys.0.lock().unwrap();
    let p = m.get_mut(&id).ok_or("no such pty")?;
    p.writer
        .write_all(data.as_bytes())
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn pty_resize(ptys: State<Ptys>, id: u64, cols: u16, rows: u16) -> Result<(), String> {
    let m = ptys.0.lock().unwrap();
    let p = m.get(&id).ok_or("no such pty")?;
    p.master
        .resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn pty_kill(ptys: State<Ptys>, id: u64) -> Result<(), String> {
    if let Some(mut p) = ptys.0.lock().unwrap().remove(&id) {
        let _ = p.child.kill();
    }
    Ok(())
}

/// Is `name` on PATH? (to offer Claude / OpenCode / Herdr panes)
#[tauri::command]
fn which(name: String) -> Option<String> {
    #[allow(unused_mut)]
    let mut names = vec![name.clone()];
    #[cfg(windows)]
    for ext in ["exe", "cmd", "bat"] {
        names.push(format!("{name}.{ext}"));
    }
    std::env::var_os("PATH")
        .and_then(|p| {
            std::env::split_paths(&p)
                .find_map(|d| names.iter().map(|n| d.join(n)).find(|c| c.is_file()))
        })
        .map(|p| p.to_string_lossy().into_owned())
}

fn main() {
    // GUI apps on macOS don't inherit the shell PATH; agents and modules need it.
    #[cfg(unix)]
    if let Ok(out) = std::process::Command::new(std::env::var("SHELL").unwrap_or("/bin/zsh".into()))
        .args(["-lc", "printf %s \"$PATH\""])
        .output()
    {
        if out.status.success() && !out.stdout.is_empty() {
            // SAFETY: before any threads are spawned.
            unsafe { std::env::set_var("PATH", String::from_utf8_lossy(&out.stdout).to_string()) };
        }
    }
    let assets = start_asset_server().expect("starting asset server");
    tauri::Builder::default()
        .manage(Ptys::default())
        .manage(assets)
        .invoke_handler(tauri::generate_handler![
            core_call,
            doc_subscribe,
            asset_base,
            initial_project,
            remember_project,
            save_png,
            log_line,
            reveal_path,
            fish_status_cmd,
            fish_save,
            fish_clear,
            pty_spawn,
            pty_write,
            pty_resize,
            pty_kill,
            which
        ])
        .setup(|app| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.set_title("Kurnace");
            }
            // Production bundles carry the core modules (and optionally node) in
            // Contents/Resources. Use them instead of the developer checkout.
            if let Ok(res) = app.path().resource_dir() {
                let modules = res.join("modules");
                if modules.exists() {
                    // SAFETY: setup runs before any project/module job starts.
                    unsafe {
                        std::env::set_var("KURNACE_MODULES", modules.as_os_str());
                        if res.join("puppeteer").exists() {
                            std::env::set_var("PUPPETEER_CACHE_DIR", res.join("puppeteer"));
                        }
                        let bin = res.join("bin");
                        if bin.exists() {
                            let mut paths = vec![bin.clone()];
                            if let Some(old) = std::env::var_os("PATH") {
                                paths.extend(std::env::split_paths(&old));
                            }
                            std::env::set_var(
                                "PATH",
                                std::env::join_paths(paths).unwrap_or_default(),
                            );
                            if bin.join("node").exists() {
                                std::env::set_var("KURN_NODE", bin.join("node"));
                            }
                            if bin.join("python3").exists() {
                                std::env::set_var("KURN_PYTHON", bin.join("python3"));
                            }
                        }
                    }
                }
            }
            // Portable Windows/zip layout: keep the core, modules, Node and
            // Chromium next to the .exe and point the module runner at them.
            if let Ok(exe) = std::env::current_exe() {
                if let Some(dir) = exe.parent() {
                    // SAFETY: setup runs before project/module jobs start.
                    unsafe {
                        if dir.join("modules").exists() {
                            std::env::set_var("KURNACE_MODULES", dir.join("modules"));
                        }
                        if dir.join("node.exe").exists() {
                            std::env::set_var("KURN_NODE", dir.join("node.exe"));
                        }
                        let py = dir.join("python").join("python.exe");
                        if py.exists() {
                            std::env::set_var("KURN_PYTHON", py);
                        }
                        if dir.join("puppeteer").exists() {
                            std::env::set_var("PUPPETEER_CACHE_DIR", dir.join("puppeteer"));
                        }
                        if dir.join("ffmpeg.exe").exists() {
                            let mut paths = vec![dir.to_path_buf()];
                            if let Some(old) = std::env::var_os("PATH") {
                                paths.extend(std::env::split_paths(&old));
                            }
                            std::env::set_var(
                                "PATH",
                                std::env::join_paths(paths).unwrap_or_default(),
                            );
                        }
                    }
                }
            }
            use tauri::menu::{MenuBuilder, MenuItemBuilder, SubmenuBuilder};
            let projects = MenuItemBuilder::with_id("projects", "Projects…")
                .accelerator("CmdOrCtrl+1")
                .build(app)?;
            let new_project = MenuItemBuilder::with_id("new-project", "New Project…")
                .accelerator("CmdOrCtrl+N")
                .build(app)?;
            let settings = MenuItemBuilder::with_id("settings", "Settings…")
                .accelerator("CmdOrCtrl+,")
                .build(app)?;
            let file = SubmenuBuilder::new(app, "File")
                .item(&projects)
                .item(&new_project)
                .separator()
                .close_window()
                .build()?;
            let app_menu = SubmenuBuilder::new(app, "Kurnace")
                .about(None)
                .item(&settings)
                .separator()
                .services()
                .separator()
                .hide()
                .hide_others()
                .show_all()
                .separator()
                .quit()
                .build()?;
            let edit = SubmenuBuilder::new(app, "Edit")
                .undo()
                .redo()
                .separator()
                .cut()
                .copy()
                .paste()
                .select_all()
                .build()?;
            let view = SubmenuBuilder::new(app, "View").fullscreen().build()?;
            let window = SubmenuBuilder::new(app, "Window")
                .minimize()
                .maximize()
                .separator()
                .close_window()
                .build()?;
            let menu = MenuBuilder::new(app)
                .items(&[&app_menu, &file, &edit, &view, &window])
                .build()?;
            app.set_menu(menu)?;
            app.on_menu_event(|app, event| {
                let id = event.id().as_ref().to_string();
                if id == "projects" || id == "new-project" || id == "settings" {
                    let _ = app.emit("kurn-menu", id);
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Kurnace");
}
