//! `kurn` — Kurnace CLI and headless core (spec Appendix A).
//! Exit codes: 0 ok · 1 error · 2 validation failed · 3 pending confirmation · 4 core not running
use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use kurn_core::core::socket_path;
use serde_json::{Map, Value, json};
use std::io::{BufRead, BufReader, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(
    name = "kurn",
    version,
    about = "Kurnace — co-create videos with agents",
    disable_help_subcommand = true
)]
struct Cli {
    /// Machine-readable JSON output (one object per command).
    #[arg(long, global = true)]
    json: bool,
    /// Project directory (default: nearest parent with project.toml).
    #[arg(long, global = true)]
    project: Option<PathBuf>,
    /// Who is acting: `agent:<name>` or `human:<name>` (default: $KURN_ACTOR or human:$USER).
    #[arg(long, global = true)]
    actor: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the core in the foreground (normally auto-started).
    Daemon,
    /// Core status.
    Status,
    /// Stop the running core.
    Stop,
    /// Projects.
    #[command(subcommand)]
    Project(ProjectCmd),
    /// Regenerate AGENTS.md for the project.
    AgentsMd,
    /// HTML scenes in the project's scenes/ folder (live, code-driven clips).
    #[command(subcommand)]
    Scene(SceneCmd),
    /// Timeline.
    #[command(subcommand)]
    Timeline(TimelineCmd),
    /// Whiteboard.
    #[command(subcommand)]
    Board(BoardCmd),
    /// Apply an op: `kurn op clip.move --args '{"clip":"c_1","start":30}'`.
    /// Also: `op batch --file ops.json`, `op status|wait|approve|reject <id>`, `op pending`, `op list`.
    Op {
        /// Op type or sub-action.
        what: String,
        /// Op id (for status/wait/approve/reject).
        id: Option<String>,
        #[arg(long)]
        args: Option<String>,
        #[arg(long)]
        file: Option<PathBuf>,
        #[arg(long, default_value_t = 300)]
        timeout: u64,
    },
    /// Undo your own last op.
    Undo,
    /// Redo your own last undone op.
    Redo,
    /// Publish your live playhead/selection for other editors (spec §7.4).
    Presence {
        #[command(subcommand)]
        cmd: PresenceCmd,
    },
    /// Op log.
    Log {
        #[arg(long)]
        actor_filter: Option<String>,
        #[arg(long, default_value_t = 30)]
        limit: u64,
    },
    /// Module API budget status.
    Budget,
    /// Asset library.
    #[command(subcommand)]
    Lib(LibCmd),
    /// List modules.
    Modules,
    /// Run a module: `kurn run tts.say --text "Hello"`.
    #[command(trailing_var_arg = true)]
    Run {
        module: String,
        #[arg(long)]
        seed: Option<i64>,
        /// Reuse a previous output of an identical recipe even for non-deterministic modules.
        #[arg(long)]
        cached: bool,
        #[arg(allow_hyphen_values = true)]
        params: Vec<String>,
    },
    /// Render a still, a draft, or the final video.
    Render {
        /// still | draft | final
        mode: String,
        #[arg(long)]
        frame: Option<i64>,
        #[arg(long)]
        range: Option<String>,
        #[arg(long)]
        scale: Option<f64>,
        #[arg(long, short)]
        out: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum PresenceCmd {
    /// Set your playhead and/or selected clip ids.
    Set {
        #[arg(long)]
        playhead: Option<i64>,
        #[arg(long, value_delimiter = ',')]
        select: Vec<String>,
    },
    /// Stop publishing presence.
    Clear,
    /// List everyone currently present.
    List,
}

#[derive(Subcommand)]
enum ProjectCmd {
    /// Create a project directory.
    New {
        dir: PathBuf,
        #[arg(long)]
        name: Option<String>,
        #[arg(long, default_value = "30/1")]
        fps: String,
        #[arg(long, default_value_t = 1920)]
        width: i64,
        #[arg(long, default_value_t = 1080)]
        height: i64,
        #[arg(long)]
        no_git: bool,
    },
    /// Show the current project.
    Show,
    /// List projects in the default projects directory plus recents.
    List,
}

#[derive(Subcommand)]
enum SceneCmd {
    /// List the project's scenes and their params.
    List,
    /// Create scenes/<name>/ from a template (html: CSS-animated page, canvas: kurnace.scene render loop).
    New {
        name: String,
        #[arg(long, default_value = "html")]
        template: String,
    },
}

#[derive(Subcommand)]
enum TimelineCmd {
    Show,
}

#[derive(Subcommand)]
enum BoardCmd {
    /// Add element skeletons: `board add --json '[{"type":"rectangle",...}]'`.
    Add {
        /// Element JSON (array or single object). `--json` with a JSON value is
        /// rewritten to this by the CLI for compatibility with the spec.
        #[arg(long)]
        elements: Option<String>,
        #[arg(long)]
        file: Option<PathBuf>,
    },
    /// Add a frame (a scene).
    Frame {
        name: String,
        /// Top-left position, `x,y`.
        #[arg(long, default_value = "0,0")]
        at: String,
        /// `w,h` (default 400,240).
        #[arg(long)]
        size: Option<String>,
        /// Link the frame to a storyboard scene id.
        #[arg(long)]
        scene: Option<String>,
        #[arg(long)]
        id: Option<String>,
    },
    /// Write a snapshot (SVG, or PNG via macOS Quick Look).
    Snapshot {
        #[arg(long, short)]
        out: Option<PathBuf>,
        /// PNG output path, e.g. `renders/board.png`.
        #[arg(long)]
        png: Option<PathBuf>,
    },
    /// Build a rough cut from frames (one batch op).
    ToTimeline,
    /// Print the board elements as JSON.
    Elements,
}

#[derive(Subcommand)]
enum LibCmd {
    /// Add files or directories (frame sequences) to the library.
    Add {
        paths: Vec<PathBuf>,
        #[arg(long, value_delimiter = ',')]
        tag: Vec<String>,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        license: Option<String>,
    },
    Search {
        query: Vec<String>,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        tag: Option<String>,
        /// Include deprecated assets.
        #[arg(long)]
        all: bool,
        /// Local semantic/similarity search (Ollama model, or offline n-gram fallback).
        #[arg(long)]
        semantic: bool,
        #[arg(long, default_value_t = 50)]
        limit: u64,
    },
    Get {
        id: String,
    },
    Tag {
        id: String,
        tags: Vec<String>,
    },
    Untag {
        id: String,
        tags: Vec<String>,
    },
    Deprecate {
        id: String,
    },
    /// Re-run an asset's recipe with overrides: `--set hue+=20`, `--set voice=Alex`.
    Variant {
        id: String,
        #[arg(long)]
        set: Vec<String>,
        #[arg(long)]
        seed: Option<i64>,
    },
}

// ---------- client ----------

struct Client {
    w: kurn_core::transport::Conn,
    r: BufReader<kurn_core::transport::Conn>,
    actor: String,
    next: u64,
}

fn spawn_daemon() -> Result<()> {
    let exe = std::env::current_exe()?;
    let log = socket_path().with_file_name("core.log");
    std::fs::create_dir_all(log.parent().unwrap())?;
    let f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)?;
    Command::new(exe)
        .arg("daemon")
        .stdin(Stdio::null())
        .stdout(f.try_clone()?)
        .stderr(f)
        .spawn()
        .context("starting core")?;
    Ok(())
}

impl Client {
    fn connect(actor: String) -> Result<Client> {
        let sock = socket_path();
        let s = match kurn_core::transport::connect(&sock) {
            Ok(s) => s,
            Err(_) if std::env::var_os("KURN_NO_AUTOSTART").is_none() => {
                spawn_daemon()?;
                let t0 = Instant::now();
                loop {
                    if let Ok(s) = kurn_core::transport::connect(&sock) {
                        break s;
                    }
                    if t0.elapsed() > Duration::from_secs(10) {
                        bail!(CoreDown(format!(
                            "core did not start; see {}",
                            sock.with_file_name("core.log").display()
                        )));
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
            Err(e) => bail!(CoreDown(format!(
                "core not running on {} ({e})",
                sock.display()
            ))),
        };
        Ok(Client {
            r: BufReader::new(s.try_clone()?),
            w: s,
            actor,
            next: 1,
        })
    }

    /// Send a request; stream events to `on_event`; return (code, result-or-error object).
    fn call(
        &mut self,
        method: &str,
        params: Value,
        mut on_event: impl FnMut(&Value),
    ) -> Result<(i32, Value)> {
        let id = self.next;
        self.next += 1;
        writeln!(
            self.w,
            "{}",
            json!({ "id": id, "method": method, "actor": self.actor, "params": params })
        )?;
        let mut line = String::new();
        loop {
            line.clear();
            if self.r.read_line(&mut line)? == 0 {
                bail!("core closed the connection");
            }
            let v: Value = serde_json::from_str(&line)?;
            if let Some(ev) = v.get("event") {
                on_event(ev);
                continue;
            }
            let code = v.get("code").and_then(Value::as_i64).unwrap_or(0) as i32;
            if v["ok"] == json!(true) {
                return Ok((code, v["result"].clone()));
            }
            return Ok((code.max(1), v));
        }
    }
}

#[derive(Debug)]
struct CoreDown(String);
impl std::fmt::Display for CoreDown {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for CoreDown {}

fn find_project(explicit: &Option<PathBuf>) -> Result<String> {
    let start = match explicit {
        Some(p) => p.clone(),
        None => std::env::current_dir()?,
    };
    let mut d = start
        .canonicalize()
        .with_context(|| format!("{} not found", start.display()))?;
    loop {
        if d.join("project.toml").exists() {
            return Ok(d.to_string_lossy().into_owned());
        }
        if !d.pop() {
            bail!(
                "not inside a Kurnace project (no project.toml); use --project or `kurn project new`"
            );
        }
    }
}

/// `--key value` / `--key=value` / `--flag` → JSON params (values stay strings; core coerces).
fn parse_kv(args: &[String]) -> Result<Map<String, Value>> {
    let mut out = Map::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        let key = a
            .strip_prefix("--")
            .ok_or_else(|| anyhow!("expected --name value, got {a:?}"))?;
        if let Some((k, v)) = key.split_once('=') {
            out.insert(k.into(), json!(v));
            i += 1;
        } else if i + 1 < args.len() && !args[i + 1].starts_with("--") {
            out.insert(key.into(), json!(args[i + 1]));
            i += 2;
        } else {
            out.insert(key.into(), json!("true"));
            i += 1;
        }
    }
    Ok(out)
}

fn progress_printer(json_mode: bool) -> impl FnMut(&Value) {
    let tty = std::io::stderr().is_terminal();
    move |ev: &Value| {
        if json_mode {
            return;
        }
        match ev["type"].as_str() {
            Some("progress") => {
                if let Some(p) = ev["pct"].as_f64() {
                    let n = (p / 100.0 * 30.0).round() as usize;
                    let msg = ev["msg"].as_str().unwrap_or("");
                    if tty {
                        eprint!(
                            "\r[{}{}] {:>3}% {:<40}",
                            "#".repeat(n.min(30)),
                            ".".repeat(30 - n.min(30)),
                            p as i64,
                            msg
                        );
                        if p >= 100.0 {
                            eprintln!();
                        }
                    } else if !msg.is_empty() {
                        eprintln!("{msg}");
                    }
                }
            }
            Some("log") => {
                let lvl = ev["level"].as_str().unwrap_or("info");
                if lvl == "warn" || lvl == "error" {
                    eprintln!("{lvl}: {}", ev["msg"].as_str().unwrap_or(""));
                }
            }
            _ => {}
        }
    }
}

fn print_human(method: &str, v: &Value) {
    match method {
        "project.list" => {
            for p in v["projects"].as_array().cloned().unwrap_or_default() {
                println!(
                    "{:<28} {:<9} {}",
                    p["name"].as_str().unwrap_or(""),
                    p["fps"].as_str().unwrap_or(""),
                    p["path"].as_str().unwrap_or("")
                );
            }
        }
        "project.budget" => {
            let spent = v["spent_usd"].as_f64().unwrap_or(0.0);
            match v["limit_usd"].as_f64() {
                Some(limit) => println!(
                    "${spent:.2} / ${limit:.2} used ({:.0}%){}",
                    v["used_pct"].as_f64().unwrap_or(0.0),
                    if v["over"] == json!(true) {
                        " · LIMIT REACHED"
                    } else if v["warning"] == json!(true) {
                        " · warning"
                    } else {
                        ""
                    }
                ),
                None => println!("${spent:.2} spent · no API budget set"),
            }
        }
        "board.apply" => {
            if let Some(id) = v["op"].as_str() {
                println!("{}", v["status"].as_str().unwrap_or("applied"));
                println!("op {id}");
            } else {
                println!("{}", v["status"].as_str().unwrap_or("applied"));
            }
        }
        "board.to-timeline" => {
            let d = &v["details"];
            println!(
                "{} scenes · {} clips · {} frames",
                d["scenes"], d["clips"], d["duration"]
            );
        }
        "board.elements" => {
            let els = v.as_array().cloned().unwrap_or_default();
            println!("{} elements", els.len());
            for e in els {
                println!(
                    "  {:<12} {:<10} {}",
                    e["id"].as_str().unwrap_or(""),
                    e["type"].as_str().unwrap_or(""),
                    e["name"]
                        .as_str()
                        .or_else(|| e["text"].as_str())
                        .unwrap_or("")
                );
            }
        }
        "board.snapshot" => {
            println!("{}", v["svg"].as_str().unwrap_or(""));
        }
        "timeline.show" => {
            let tl = &v["timeline"];
            let fps = format!("{}/{}", tl["fps"]["num"], tl["fps"]["den"]);
            println!(
                "{}x{} @ {fps} fps · {} frames",
                tl["width"], tl["height"], tl["duration"]
            );
            let mut tracks: Vec<&Value> = tl["tracks"]
                .as_array()
                .map(|a| a.iter().collect())
                .unwrap_or_default();
            tracks.reverse();
            for t in tracks {
                let id = t["id"].as_str().unwrap_or("");
                let flags = format!(
                    "{}{}",
                    if t["muted"] == json!(true) {
                        " [muted]"
                    } else {
                        ""
                    },
                    if t["locked"] == json!(true) {
                        " [locked]"
                    } else {
                        ""
                    }
                );
                println!(
                    "  {:<10} {:<9} {}{flags}",
                    id,
                    t["kind"].as_str().unwrap_or(""),
                    t["name"].as_str().unwrap_or("")
                );
                let mut clips: Vec<(&String, &Value)> = tl["clips"]
                    .as_object()
                    .map(|o| o.iter().filter(|(_, c)| c["track"] == json!(id)).collect())
                    .unwrap_or_default();
                clips.sort_by_key(|(_, c)| c["start"].as_i64());
                for (cid, c) in clips {
                    let len = c["out"].as_i64().unwrap_or(0) - c["in"].as_i64().unwrap_or(0);
                    let what = c["component"]
                        .as_str()
                        .map(|x| format!("<{x}>"))
                        .or(c["asset"].as_str().map(String::from))
                        .unwrap_or_default();
                    let start = c["start"].as_i64().unwrap_or(0);
                    println!(
                        "      {:<12} {:>6}–{:<6} {what}  in={}",
                        cid,
                        start,
                        start + len,
                        c["in"]
                    );
                }
            }
            for (id, m) in tl["markers"].as_object().cloned().unwrap_or_default() {
                println!("  ◆ {id} @{} {}", m["t"], m["label"].as_str().unwrap_or(""));
            }
            for i in v["issues"].as_array().cloned().unwrap_or_default() {
                println!(
                    "{} {}",
                    if i["level"] == "error" { "✗" } else { "!" },
                    i["msg"].as_str().unwrap_or("")
                );
            }
        }
        "lib.search" | "lib.add" => {
            for a in v.as_array().cloned().unwrap_or_default() {
                let dims = match (a["width"].as_i64(), a["height"].as_i64()) {
                    (Some(w), Some(h)) => format!("{w}x{h}"),
                    _ => String::new(),
                };
                let dur = a["duration_s"]
                    .as_f64()
                    .map(|d| format!("{d:.2}s"))
                    .unwrap_or_default();
                let tags = a["tags"]
                    .as_array()
                    .map(|t| {
                        t.iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(",")
                    })
                    .unwrap_or_default();
                println!(
                    "{}  {:<6} {:<28} {:<10} {:<8} {}{}",
                    a["id"].as_str().unwrap_or(""),
                    a["kind"].as_str().unwrap_or(""),
                    a["name"].as_str().unwrap_or(""),
                    dims,
                    dur,
                    tags,
                    if a["deprecated"] == json!(true) {
                        " [deprecated]"
                    } else {
                        ""
                    }
                );
            }
        }
        "modules.list" => {
            for m in v.as_array().cloned().unwrap_or_default() {
                println!(
                    "{:<18} {:<8} {:<10} {}",
                    m["id"].as_str().unwrap_or(""),
                    m["version"].as_str().unwrap_or(""),
                    m["kind"].as_str().unwrap_or(""),
                    m["description"].as_str().unwrap_or("")
                );
            }
        }
        "log" => {
            for r in v.as_array().cloned().unwrap_or_default() {
                println!(
                    "{} {:<16} {:<16} {}",
                    r["ts"].as_str().unwrap_or(""),
                    format!(
                        "{}:{}",
                        r["actor"]["type"].as_str().unwrap_or(""),
                        r["actor"]["name"].as_str().unwrap_or("")
                    ),
                    r["type"].as_str().unwrap_or(""),
                    r["args"]
                );
            }
        }
        "op.apply" => match v["status"].as_str() {
            Some("pending") => println!(
                "pending {} — {}\n  wait: kurn op wait {0}",
                v["op"].as_str().unwrap_or(""),
                v["reason"].as_str().unwrap_or("")
            ),
            _ => {
                let r = &v["result"];
                println!(
                    "applied {}{}",
                    v["op"].as_str().unwrap_or(""),
                    if r.as_object().is_some_and(|o| !o.is_empty()) {
                        format!("  {r}")
                    } else {
                        String::new()
                    }
                );
                for w in v["warnings"].as_array().cloned().unwrap_or_default() {
                    println!("! {}", w.as_str().unwrap_or(""));
                }
            }
        },
        "run" | "lib.variant" => {
            for (name, a) in v["assets"].as_object().cloned().unwrap_or_default() {
                println!(
                    "{name}: {}  {}{}",
                    a["id"].as_str().or(a["path"].as_str()).unwrap_or(""),
                    a["name"].as_str().unwrap_or(""),
                    if v["cached"] == json!(true) {
                        "  (cached)"
                    } else {
                        ""
                    }
                );
            }
            for n in v["notes"].as_array().cloned().unwrap_or_default() {
                println!("! {}", n.as_str().unwrap_or(""));
            }
        }
        _ => println!("{}", serde_json::to_string_pretty(v).unwrap_or_default()),
    }
}

/// `kurn run m --text hi --json`: clap's trailing args swallow global flags that
/// come after the module's inputs. Pull them back out.
fn hoist_run_flags(cli: &mut Cli) {
    if let Cmd::Run {
        params,
        seed,
        cached,
        ..
    } = &mut cli.cmd
    {
        let mut rest = vec![];
        let mut it = std::mem::take(params).into_iter();
        while let Some(a) = it.next() {
            match a.as_str() {
                "--json" => cli.json = true,
                "--cached" => *cached = true,
                "--project" => cli.project = it.next().map(PathBuf::from),
                "--actor" => cli.actor = it.next(),
                "--seed" => *seed = it.next().and_then(|v| v.parse().ok()),
                _ => rest.push(a),
            }
        }
        *params = rest;
    }
}

fn parse_pair(s: &str, what: &str) -> Result<(f64, f64)> {
    let (a, b) = s
        .split_once(',')
        .ok_or_else(|| anyhow!("{what} must look like `x,y`"))?;
    Ok((
        a.trim().parse().with_context(|| format!("bad {what} x"))?,
        b.trim().parse().with_context(|| format!("bad {what} y"))?,
    ))
}

/// `board add --json '<json>'` per spec §13, while `--json` globally means
/// machine-readable output. Only rewrite when the value is clearly JSON.
fn rewrite_board_json(args: Vec<std::ffi::OsString>) -> Vec<std::ffi::OsString> {
    let mut out: Vec<std::ffi::OsString> = Vec::with_capacity(args.len());
    let mut i = 0;
    while i < args.len() {
        let s = args[i].to_string_lossy();
        if s == "board" && i + 1 < args.len() && args[i + 1] == "add" {
            out.push(args[i].clone());
            out.push(args[i + 1].clone());
            i += 2;
            while i < args.len() {
                let a = args[i].to_string_lossy();
                if a == "--json" && i + 1 < args.len() {
                    let v = args[i + 1].to_string_lossy();
                    if v.trim_start().starts_with('[') || v.trim_start().starts_with('{') {
                        out.push("--elements".into());
                        out.push(args[i + 1].clone());
                        i += 2;
                        continue;
                    }
                }
                out.push(args[i].clone());
                i += 1;
            }
            continue;
        }
        out.push(args[i].clone());
        i += 1;
    }
    out
}

fn svg_to_png(svg: &Path, png: &Path) -> Result<()> {
    if let Some(dir) = png.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // macOS `sips` rasterizes SVG via its WebKit/ImageIO path and exits.
    // (qlmanage can hang waiting on a Quick Look host.)
    let out = Command::new("sips")
        .args(["-s", "format", "png"])
        .arg(svg)
        .arg("--out")
        .arg(png)
        .output()
        .context("converting SVG to PNG with sips (macOS only)")?;
    if !out.status.success() || !png.exists() {
        bail!(
            "sips failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

fn run() -> Result<i32> {
    let mut cli = Cli::parse_from(rewrite_board_json(std::env::args_os().collect()));
    hoist_run_flags(&mut cli);
    if let Cmd::Daemon = cli.cmd {
        kurn_core::core::serve()?;
        return Ok(0);
    }
    let actor = cli
        .actor
        .clone()
        .or_else(|| std::env::var("KURN_ACTOR").ok())
        .unwrap_or_else(|| {
            format!(
                "human:{}",
                std::env::var("USER").unwrap_or_else(|_| "user".into())
            )
        });
    kurn_core::project::Actor::parse(&actor)?;

    if let Cmd::Stop = cli.cmd {
        // `kurn stop` must not start a core just to stop it.
        // SAFETY: single-threaded at this point.
        unsafe { std::env::set_var("KURN_NO_AUTOSTART", "1") };
        let Ok(mut c) = Client::connect(actor) else {
            println!("core not running");
            return Ok(0);
        };
        c.call("shutdown", json!({}), |_| {})?;
        println!("core stopped");
        return Ok(0);
    }

    let mut c = Client::connect(actor)?;
    let proj = || find_project(&cli.project);
    let mut post_png: Option<(PathBuf, PathBuf)> = None;
    let (method, params): (&str, Value) = match &cli.cmd {
        Cmd::Daemon | Cmd::Stop => unreachable!(),
        Cmd::Status => ("status", json!({})),
        Cmd::Project(ProjectCmd::New {
            dir,
            name,
            fps,
            width,
            height,
            no_git,
        }) => {
            std::fs::create_dir_all(dir)?;
            (
                "project.new",
                json!({ "dir": dir.canonicalize()?, "name": name, "fps": fps, "width": width, "height": height, "git": !no_git }),
            )
        }
        Cmd::Project(ProjectCmd::Show) => ("project.open", json!({ "project": proj()? })),
        Cmd::Project(ProjectCmd::List) => ("project.list", json!({})),
        Cmd::AgentsMd => ("agents.md", json!({ "project": proj()? })),
        Cmd::Timeline(TimelineCmd::Show) => ("timeline.show", json!({ "project": proj()? })),
        Cmd::Scene(SceneCmd::List) => ("scenes.list", json!({ "project": proj()? })),
        Cmd::Scene(SceneCmd::New { name, template }) => ("scene.new", json!({ "project": proj()?, "name": name, "template": template })),
        Cmd::Board(b) => match b {
            BoardCmd::Add { elements, file } => {
                let txt = match (elements, file) {
                    (Some(s), _) => s.clone(),
                    (None, Some(f)) => std::fs::read_to_string(f)?,
                    (None, None) => bail!("board add needs --json '<elements>' or --file <path>"),
                };
                let v: Value =
                    serde_json::from_str(&txt).context("board elements are not valid JSON")?;
                let specs = match v {
                    Value::Array(a) => a,
                    v @ Value::Object(_) => vec![v],
                    _ => bail!("board add expects a JSON object or array"),
                };
                if specs.is_empty() {
                    bail!("board add: no elements");
                }
                let ops: Vec<Value> = specs
                    .into_iter()
                    .map(|e| json!({ "type": "el.add", "args": { "element": e } }))
                    .collect();
                let op = if ops.len() == 1 {
                    ops[0].clone()
                } else {
                    json!({ "type": "batch", "args": { "ops": ops } })
                };
                ("board.apply", json!({ "project": proj()?, "op": op }))
            }
            BoardCmd::Frame {
                name,
                at,
                size,
                scene,
                id,
            } => {
                let (x, y) = parse_pair(at, "--at")?;
                let (w, h) = match size {
                    Some(s) => parse_pair(s, "--size")?,
                    None => (400.0, 240.0),
                };
                (
                    "board.apply",
                    json!({ "project": proj()?, "op": { "type": "frame.add", "args": {
                        "name": name, "x": x, "y": y, "width": w, "height": h,
                        "id": id, "scene": scene
                    } } }),
                )
            }
            BoardCmd::Snapshot { out, png } => {
                let project = proj()?;
                let target = png
                    .clone()
                    .or_else(|| out.clone())
                    .map(|p| std::env::current_dir().map(|cwd| cwd.join(&p)).unwrap_or(p))
                    .unwrap_or_else(|| Path::new(&project).join("renders/board.svg"));
                let svg = if png.is_some() {
                    target.with_extension("svg")
                } else {
                    target.clone()
                };
                if png.is_some() {
                    post_png = Some((svg.clone(), target));
                }
                ("board.snapshot", json!({ "project": project, "out": svg }))
            }
            BoardCmd::ToTimeline => ("board.to-timeline", json!({ "project": proj()? })),
            BoardCmd::Elements => ("board.elements", json!({ "project": proj()? })),
        },
        Cmd::Undo => ("undo", json!({ "project": proj()? })),
        Cmd::Redo => ("redo", json!({ "project": proj()? })),
        Cmd::Presence { cmd } => match cmd {
            PresenceCmd::Set { playhead, select } => (
                "presence.set",
                json!({ "project": proj()?, "playhead": playhead, "select": select }),
            ),
            PresenceCmd::Clear => ("presence.clear", json!({ "project": proj()? })),
            PresenceCmd::List => ("presence.list", json!({ "project": proj()? })),
        },
        Cmd::Log {
            actor_filter,
            limit,
        } => (
            "log",
            json!({ "project": proj()?, "actor": actor_filter, "limit": limit }),
        ),
        Cmd::Budget => ("project.budget", json!({ "project": proj()? })),
        Cmd::Modules => ("modules.list", json!({ "project": proj().ok() })),
        Cmd::Op {
            what,
            id,
            args,
            file,
            timeout,
        } => match what.as_str() {
            "list" => ("ops.catalog", json!({})),
            "pending" => ("op.pending", json!({ "project": proj()? })),
            "status" => (
                "op.status",
                json!({ "project": proj()?, "op": id.clone().ok_or_else(|| anyhow!("op status <id>"))? }),
            ),
            "approve" | "reject" => (
                "op.decide",
                json!({ "project": proj()?, "op": id.clone().ok_or_else(|| anyhow!("op {what} <id>"))?, "approve": what == "approve" }),
            ),
            "wait" => {
                let id = id.clone().ok_or_else(|| anyhow!("op wait <id>"))?;
                let t0 = Instant::now();
                let project = proj()?;
                loop {
                    let (code, v) =
                        c.call("op.status", json!({ "project": project, "op": id }), |_| {})?;
                    if code != 0 {
                        return finish(cli.json, "op.status", code, v);
                    }
                    if v["state"] != json!("pending") {
                        let ok = v["state"] == json!("applied");
                        if cli.json {
                            println!("{v}")
                        } else {
                            println!("{} {id}", v["state"].as_str().unwrap_or(""));
                        }
                        return Ok(if ok { 0 } else { 1 });
                    }
                    if t0.elapsed() > Duration::from_secs(*timeout) {
                        if cli.json {
                            println!("{v}")
                        } else {
                            println!("still pending after {timeout}s")
                        }
                        return Ok(3);
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
            }
            kind => {
                let a: Value = match (args, file) {
                    (Some(a), _) => serde_json::from_str(a).context("--args is not valid JSON")?,
                    (None, Some(f)) => serde_json::from_str(&std::fs::read_to_string(f)?)
                        .context("--file is not valid JSON")?,
                    (None, None) => json!({}),
                };
                // `op batch` accepts a bare array too.
                let a = if kind == "batch" && a.is_array() {
                    json!({ "ops": a })
                } else {
                    a
                };
                (
                    "op.apply",
                    json!({ "project": proj()?, "op": { "type": kind, "args": a } }),
                )
            }
        },
        Cmd::Lib(l) => match l {
            LibCmd::Add {
                paths,
                tag,
                name,
                license,
            } => {
                let abs: Vec<String> = paths
                    .iter()
                    .map(|p| {
                        p.canonicalize()
                            .map(|p| p.to_string_lossy().into_owned())
                            .with_context(|| format!("{} not found", p.display()))
                    })
                    .collect::<Result<_>>()?;
                (
                    "lib.add",
                    json!({ "paths": abs, "tags": tag, "name": name, "license": license }),
                )
            }
            LibCmd::Search {
                query,
                kind,
                tag,
                all,
                semantic,
                limit,
            } => (
                "lib.search",
                json!({ "query": query.join(" "), "kind": kind, "tag": tag, "all": all, "semantic": semantic, "limit": limit }),
            ),
            LibCmd::Get { id } => ("lib.get", json!({ "id": id })),
            LibCmd::Tag { id, tags } => ("lib.tag", json!({ "id": id, "tags": tags })),
            LibCmd::Untag { id, tags } => ("lib.untag", json!({ "id": id, "tags": tags })),
            LibCmd::Deprecate { id } => ("lib.deprecate", json!({ "id": id })),
            LibCmd::Variant { id, set, seed } => (
                "lib.variant",
                json!({ "id": id, "set": set, "seed": seed, "project": proj().ok() }),
            ),
        },
        Cmd::Run {
            module,
            seed,
            cached,
            params,
        } => (
            "run",
            json!({ "module": module, "params": parse_kv(params)?, "seed": seed, "cached": cached, "project": proj().ok() }),
        ),
        Cmd::Render {
            mode,
            frame,
            range,
            scale,
            out,
        } => {
            if !["still", "draft", "final"].contains(&mode.as_str()) {
                bail!("render mode must be still|draft|final");
            }
            let project = proj()?;
            let f = frame.unwrap_or(0);
            let default = match mode.as_str() {
                "still" => format!("renders/f{f}.png"),
                "draft" => "renders/draft.mp4".into(),
                _ => "renders/final.mp4".into(),
            };
            let out = match out {
                Some(o) => std::env::current_dir()?.join(o),
                None => Path::new(&project).join(default),
            };
            std::fs::create_dir_all(out.parent().unwrap())?;
            (
                "render",
                json!({ "project": project, "mode": mode, "frame": f, "range": range, "scale": scale, "out": out }),
            )
        }
    };

    let mut params = params;
    if matches!(method, "run" | "lib.variant" | "render") {
        params["env"] = forwarded_env();
    }
    if matches!(method, "run" | "lib.variant") && std::env::var_os("KURN_BUDGET_OVERRIDE").is_some()
    {
        params["budget_override"] = json!(true);
    }
    let t0 = Instant::now();
    let (code, v) = c.call(method, params, progress_printer(cli.json))?;
    if method == "render" && code == 0 {
        let file = v["outputs"]["file"].as_str().unwrap_or("");
        let rel = Path::new(file)
            .strip_prefix(std::env::current_dir()?)
            .map(|p| p.display().to_string())
            .unwrap_or(file.into());
        if cli.json {
            println!(
                "{}",
                json!({ "ok": true, "out": file, "ms": t0.elapsed().as_millis() })
            );
        } else {
            println!("{rel}  ({:.1}s)", t0.elapsed().as_secs_f64());
        }
        return Ok(0);
    }
    if let Some((svg, png)) = post_png {
        if code != 0 {
            return finish(cli.json, method, code, v);
        }
        svg_to_png(&svg, &png)?;
        if cli.json {
            println!("{}", json!({ "ok": true, "png": png, "svg": svg }));
        } else {
            println!("{}", png.display());
        }
        return Ok(0);
    }
    finish(cli.json, method, code, v)
}

/// Env the caller passes to modules: API keys and Kurnace settings (same user, local socket).
fn forwarded_env() -> Value {
    let extra: Vec<String> = std::env::var("KURN_FORWARD_ENV")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .collect();
    Value::Object(
        std::env::vars()
            .filter(|(k, _)| k.ends_with("_API_KEY") || k.starts_with("KURN_") || extra.contains(k))
            .filter(|(k, _)| k != "KURN_SOCKET" && k != "KURN_ACTOR")
            .map(|(k, v)| (k, json!(v)))
            .collect(),
    )
}

fn finish(json_mode: bool, method: &str, code: i32, v: Value) -> Result<i32> {
    if code == 0 || code == 3 {
        if json_mode {
            println!("{v}")
        } else {
            print_human(method, &v)
        }
        return Ok(code);
    }
    if json_mode {
        println!("{v}");
    } else {
        eprintln!("kurn: {}", v["error"].as_str().unwrap_or("error"));
        for i in v["issues"].as_array().cloned().unwrap_or_default() {
            eprintln!("  ✗ {}", i["msg"].as_str().unwrap_or(""));
        }
    }
    Ok(code)
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code as u8),
        Err(e) => {
            let down = e.downcast_ref::<CoreDown>().is_some();
            if std::env::args().any(|a| a == "--json") {
                println!(
                    "{}",
                    json!({ "ok": false, "error": format!("{e:#}"), "code": if down { 4 } else { 1 } })
                );
            } else {
                eprintln!("kurn: {e:#}");
            }
            ExitCode::from(if down { 4 } else { 1 })
        }
    }
}
