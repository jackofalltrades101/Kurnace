//! The core service (spec §4.1, §4.2): one writer for projects, library and
//! modules, served as newline-delimited JSON over a local Unix socket.
//!
//! Request:  {"id":1,"method":"op.apply","actor":"agent:claude","params":{…}}
//! Stream:   {"id":1,"event":{…module event…}}            (zero or more)
//! Reply:    {"id":1,"ok":true,"result":…}
//!        |  {"id":1,"ok":false,"error":"…","code":2,"issues":[…]}
use crate::library::{AddOptions, Asset, Library, Recipe};
use crate::modules::{self, Event, Manifest};
use crate::ops::{self, Op};
use crate::project::{Actor, ApplyOutcome, Project, ValidationError};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

pub const CODE_ERROR: i32 = 1;
pub const CODE_VALIDATION: i32 = 2;
pub const CODE_PENDING: i32 = 3;

#[derive(Debug)]
pub struct BudgetExceeded(pub String);
impl std::fmt::Display for BudgetExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for BudgetExceeded {}

pub fn home_dir() -> PathBuf {
    std::env::var_os("KURNACE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join("Kurnace")
        })
}

pub fn socket_path() -> PathBuf {
    std::env::var_os("KURN_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let name = if cfg!(windows) {
                "core.port"
            } else {
                "core.sock"
            };
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".kurnace")
                .join(name)
        })
}

fn recent_path() -> PathBuf {
    home_dir().join(".recent.json")
}

fn read_recent() -> Vec<Value> {
    std::fs::read_to_string(recent_path())
        .ok()
        .and_then(|t| serde_json::from_str::<Vec<Value>>(&t).ok())
        .unwrap_or_default()
}

/// Record a project for the dashboard. Most recent first, capped at 24.
fn remember_recent(project: &Path) -> Result<()> {
    let path = project
        .canonicalize()
        .unwrap_or_else(|_| project.to_path_buf());
    let p = path.to_string_lossy().into_owned();
    let now = crate::project::now_secs();
    let mut items = read_recent();
    items.retain(|v| v["path"].as_str() != Some(&p));
    items.insert(0, json!({ "path": p, "at": now }));
    items.truncate(24);
    let f = recent_path();
    if let Some(dir) = f.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = f.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&items)?)?;
    std::fs::rename(&tmp, &f)?;
    Ok(())
}

struct ProjectState {
    project: Project,
    /// Actor counts since the last auto-commit.
    dirty: BTreeMap<String, usize>,
}

pub struct Core {
    pub home: PathBuf,
    library: Library,
    projects: HashMap<PathBuf, ProjectState>,
    pub started: SystemTime,
}

/// Errors carry an exit code so the CLI can map them (Appendix A).
#[derive(Debug)]
pub struct Reply {
    pub ok: bool,
    pub result: Value,
}

impl Core {
    pub fn new(home: &Path) -> Result<Core> {
        let library = Library::open(&home.join("library"))?;
        if let Some(w) = library.location_warning() {
            eprintln!("kurn-core: warning: {w}");
        }
        Ok(Core {
            home: home.to_path_buf(),
            library,
            projects: HashMap::new(),
            started: SystemTime::now(),
        })
    }

    fn project(&mut self, dir: &str) -> Result<&mut ProjectState> {
        let root = PathBuf::from(dir)
            .canonicalize()
            .with_context(|| format!("no project at {dir}"))?;
        if !self.projects.contains_key(&root) {
            let lock = crate::project::ws_dir(&root).join("lock");
            std::fs::create_dir_all(lock.parent().unwrap())?;
            std::fs::write(
                &lock,
                format!(
                    "{{\"pid\":{},\"socket\":{:?}}}",
                    std::process::id(),
                    socket_path()
                ),
            )?;
            let p = Project::open(&root)?;
            self.projects.insert(
                root.clone(),
                ProjectState {
                    project: p,
                    dirty: BTreeMap::new(),
                },
            );
        }
        Ok(self.projects.get_mut(&root).unwrap())
    }

    /// Borrow the library and a project at the same time.
    fn split(&mut self, dir: &str) -> Result<(&Library, &mut ProjectState)> {
        self.project(dir)?;
        let root = PathBuf::from(dir).canonicalize()?;
        Ok((&self.library, self.projects.get_mut(&root).unwrap()))
    }

    fn module_dirs(&self, project: Option<&Path>) -> Vec<PathBuf> {
        modules::module_dirs(&self.home, project)
    }

    /// Asset JSON for a module request / render (path + ext so it can be staged).
    fn asset_ref(&self, id: &str) -> Result<Value> {
        let a = self
            .library
            .get(id)?
            .ok_or_else(|| anyhow!("unknown asset {id}"))?;
        let mut frames = vec![];
        if a.kind == "seq" {
            if let Some(dir) = &a.path {
                let mut imgs: Vec<PathBuf> = std::fs::read_dir(dir)?
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| {
                        p.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                            matches!(
                                e.to_ascii_lowercase().as_str(),
                                "png" | "jpg" | "jpeg" | "webp"
                            )
                        })
                    })
                    .collect();
                imgs.sort();
                frames = imgs
                    .iter()
                    .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                    .collect();
            }
        }
        Ok(json!({
            "id": a.id, "path": a.path, "kind": a.kind, "ext": a.ext,
            "width": a.width, "height": a.height, "duration_s": a.duration_s,
            "sample_rate": a.sample_rate, "name": a.name,
            "fps_num": a.fps_num, "fps_den": a.fps_den,
            "frames": frames,
        }))
    }

    pub fn shutdown_flush(&mut self) {
        for st in self.projects.values_mut() {
            let _ = st.project.compact();
        }
    }

    /// Periodic work: compaction + git auto-commit on idle (spec §6.3).
    pub fn idle_tick(&mut self, idle_after: Duration) {
        for st in self.projects.values_mut() {
            if st.dirty.is_empty() {
                continue;
            }
            if st.project.last_op_at.elapsed().unwrap_or_default() < idle_after {
                continue;
            }
            let _ = st.project.compact();
            let root = &st.project.root;
            if root.join(".git").exists() {
                let summary = st
                    .dirty
                    .iter()
                    .map(|(a, n)| format!("{a} ×{n}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let add = Command::new("git")
                    .arg("-C")
                    .arg(root)
                    .args(["add", "timeline.json", "board.excalidraw"])
                    .status();
                let commit = Command::new("git")
                    .arg("-C")
                    .arg(root)
                    .args([
                        "commit",
                        "-q",
                        "-m",
                        &format!("kurn: snapshots ({summary})"),
                        "--",
                        "timeline.json",
                        "board.excalidraw",
                    ])
                    .status();
                if !(add.is_ok_and(|s| s.success()) && commit.is_ok_and(|s| s.success())) {
                    eprintln!(
                        "kurn-core: auto-commit in {} skipped (nothing to commit or git error)",
                        root.display()
                    );
                }
            }
            st.dirty.clear();
        }
    }
}

fn s<'a>(p: &'a Value, k: &str) -> Result<&'a str> {
    p.get(k)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing param `{k}`"))
}

/// What a long-running module call needs once the lock is released.
struct Job {
    manifest: Manifest,
    request: Value,
    cwd: Option<PathBuf>,
    work: Option<tempdir::Dir>,
    finish: Finish,
}

enum Finish {
    /// Ingest asset outputs with this recipe.
    Generate {
        recipe: Recipe,
        created_by: String,
        parent: Option<String>,
        notes: Vec<String>,
    },
    /// Return output file paths as-is.
    Files,
}

mod tempdir {
    use std::path::{Path, PathBuf};
    pub struct Dir(PathBuf);
    impl Dir {
        pub fn new(prefix: &str) -> std::io::Result<Dir> {
            let p = std::env::temp_dir().join(format!("{prefix}-{}", ulid::Ulid::generate()));
            std::fs::create_dir_all(&p)?;
            Ok(Dir(p))
        }
        pub fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

pub type Shared = Arc<Mutex<Core>>;

/// Dispatch one request. `emit` streams module events back to the client.
pub fn handle(
    core: &Shared,
    method: &str,
    actor: &Actor,
    p: &Value,
    emit: &mut dyn FnMut(Value),
) -> Result<Value> {
    if method == "run.decide" {
        return decide_run(core, actor, p, emit);
    }
    // Methods that run modules release the lock while the module runs.
    if matches!(method, "run" | "lib.variant" | "render") {
        let job = {
            let mut c = core.lock().unwrap();
            match method {
                "run" => prepare_run(&mut c, actor, p)?,
                "lib.variant" => prepare_variant(&mut c, actor, p)?,
                _ => prepare_render(&mut c, p)?,
            }
        };
        let job = match job {
            Ok(j) => j,
            Err(cached) => return Ok(cached), // recipe cache hit / pending budget
        };
        return execute_job(core, job, p, emit);
    }

    let mut c = core.lock().unwrap();
    let c = &mut *c;
    Ok(match method {
        "ping" => json!({ "pong": true, "pid": std::process::id() }),
        "status" => json!({
            "pid": std::process::id(),
            "home": c.home,
            "library": c.library.root,
            "socket": socket_path(),
            "projects": c.projects.keys().collect::<Vec<_>>(),
            "uptime_s": c.started.elapsed().unwrap_or_default().as_secs(),
            "library_warning": c.library.location_warning(),
        }),

        "project.new" => {
            let dir = PathBuf::from(s(p, "dir")?);
            let name = p
                .get("name")
                .and_then(Value::as_str)
                .map(String::from)
                .unwrap_or_else(|| {
                    dir.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or("project".into())
                });
            Project::create(
                &dir,
                &name,
                p.get("fps").and_then(Value::as_str).unwrap_or("30/1"),
                p.get("width").and_then(Value::as_i64).unwrap_or(1920),
                p.get("height").and_then(Value::as_i64).unwrap_or(1080),
            )?;
            if p.get("git").and_then(Value::as_bool).unwrap_or(true) {
                let _ = Command::new("git")
                    .arg("-C")
                    .arg(&dir)
                    .args(["init", "-q"])
                    .status();
            }
            let root = dir.canonicalize()?;
            let dirs = c.module_dirs(Some(&root));
            write_agents_md(&root, &dirs)?;
            write_claude_md(&root)?;
            write_agent_settings(&root, &c.library.root.canonicalize()?)?;
            let st = c.project(&root.to_string_lossy())?;
            remember_recent(&st.project.root)?;
            json!({ "project": st.project.root, "name": name })
        }

        "project.open" => {
            let st = c.project(s(p, "project")?)?;
            let root = st.project.root.clone();
            let config = st.project.config.clone();
            let dirs = c.module_dirs(Some(&root));
            write_agents_md(&root, &dirs)?;
            write_claude_md(&root)?;
            write_agent_settings(&root, &c.library.root.canonicalize()?)?;
            remember_recent(&root)?;
            json!({ "project": root, "config": config })
        }

        "project.remember" => {
            let st = c.project(s(p, "project")?)?;
            remember_recent(&st.project.root)?;
            json!({ "ok": true })
        }

        "project.list" => {
            let root = c.home.join("projects");
            let mut paths: Vec<PathBuf> = vec![];
            if let Ok(rd) = std::fs::read_dir(&root) {
                for e in rd.flatten() {
                    if e.path().join("project.toml").exists() {
                        paths.push(e.path());
                    }
                }
            }
            let recent = read_recent();
            let mut at: HashMap<String, u64> = HashMap::new();
            // Migrate the old app-level “last project” into the dashboard list.
            if let Some(last) = std::fs::read_to_string(socket_path().with_file_name("app.json"))
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                .and_then(|v| v["last_project"].as_str().map(String::from))
            {
                let pb = PathBuf::from(last);
                if pb.join("project.toml").exists() && !paths.iter().any(|x| x == &pb) {
                    paths.push(pb);
                }
            }
            for r in &recent {
                if let (Some(p), Some(t)) = (r["path"].as_str(), r["at"].as_u64()) {
                    at.insert(p.to_string(), t);
                    let pb = PathBuf::from(p);
                    if pb.join("project.toml").exists() && !paths.iter().any(|x| x == &pb) {
                        paths.push(pb);
                    }
                }
            }
            let mut projects = vec![];
            let mut seen = HashSet::new();
            for path in paths {
                let Ok(path) = path.canonicalize() else {
                    continue;
                };
                let Ok(cfg) = Project::load_config(&path) else {
                    continue;
                };
                let p = path.to_string_lossy().into_owned();
                if !seen.insert(p.clone()) {
                    continue;
                }
                let modified = std::fs::metadata(path.join("project.toml"))
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                projects.push(json!({
                    "path": p,
                    "name": cfg.name,
                    "fps": cfg.fps,
                    "width": cfg.width,
                    "height": cfg.height,
                    "modified_at": modified,
                    "recent_at": at.get(&p).copied().unwrap_or(0),
                }));
            }
            projects.sort_by(|a, b| {
                let ra = a["recent_at"].as_u64().unwrap_or(0);
                let rb = b["recent_at"].as_u64().unwrap_or(0);
                rb.cmp(&ra).then(
                    b["modified_at"]
                        .as_u64()
                        .unwrap_or(0)
                        .cmp(&a["modified_at"].as_u64().unwrap_or(0)),
                )
            });
            json!({ "root": root.to_string_lossy(), "projects": projects })
        }

        "project.budget" => {
            let st = c.project(s(p, "project")?)?;
            st.project.budget()
        }

        "agents.md" => {
            let root = PathBuf::from(s(p, "project")?).canonicalize()?;
            let dirs = c.module_dirs(Some(&root));
            write_agents_md(&root, &dirs)?;
            write_claude_md(&root)?;
            json!({ "path": root.join("AGENTS.md"), "claude": root.join("CLAUDE.md") })
        }

        "timeline.show" => {
            let st = c.project(s(p, "project")?)?;
            let tl = st.project.timeline()?;
            let lib = &c.library;
            let refs: std::collections::HashSet<String> = tl
                .clips
                .values()
                .filter_map(|c| c.asset.clone())
                .filter(|a| lib.exists(a))
                .collect();
            let issues = crate::timeline::validate(&tl, Some(&refs));
            json!({ "timeline": tl, "issues": issues })
        }

        "board.elements" => {
            let st = c.project(s(p, "project")?)?;
            serde_json::to_value(st.project.board_elements()?)?
        }

        "board.apply" => {
            let op: Op = serde_json::from_value(p.get("op").cloned().unwrap_or(Value::Null))
                .map_err(|e| anyhow!("bad board op: {e}"))?;
            let st = c.project(s(p, "project")?)?;
            let out = st.project.apply_board(actor, &op)?;
            if matches!(out, ApplyOutcome::Applied { .. }) {
                *st.dirty.entry(actor.key()).or_default() += 1;
            }
            serde_json::to_value(out)?
        }

        "board.derived" => {
            // Webview edits are already applied to its replica; the update is
            // authoritative and only logged for attribution (spec §6.1).
            let b64 = s(p, "update")?;
            use base64::Engine;
            let update = base64::engine::general_purpose::STANDARD
                .decode(b64.as_bytes())
                .context("board.derived: update is not base64")?;
            let op: Option<Op> = p
                .get("op")
                .filter(|v| !v.is_null())
                .cloned()
                .map(serde_json::from_value)
                .transpose()?;
            let st = c.project(s(p, "project")?)?;
            st.project
                .apply_board_derived(actor, &update, op.as_ref())?;
            *st.dirty.entry(actor.key()).or_default() += 1;
            json!({ "ok": true })
        }

        "board.to-timeline" => {
            let (lib, st) = c.split(s(p, "project")?)?;
            let meta = |id: &str| {
                lib.get(id).ok().flatten().map(|a| crate::board::AssetMeta {
                    kind: a.kind,
                    duration_s: a.duration_s.unwrap_or(0.0),
                })
            };
            st.project
                .board_to_timeline(actor, &meta, &|id| lib.exists(id))?
        }

        "board.snapshot" => {
            let st = c.project(s(p, "project")?)?;
            let svg = crate::board::to_svg(&st.project.board_doc, 1600.0, 1000.0)?;
            if let Some(out) = p.get("out").and_then(Value::as_str) {
                let path = PathBuf::from(out);
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                let svg_path = if path
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("svg"))
                {
                    path.clone()
                } else {
                    path.with_extension("svg")
                };
                std::fs::write(&svg_path, &svg)?;
                json!({ "svg": svg_path.to_string_lossy(), "png": path.to_string_lossy() })
            } else {
                json!({ "svg": svg })
            }
        }

        "op.apply" => {
            let op: Op = serde_json::from_value(p.get("op").cloned().unwrap_or(Value::Null))
                .map_err(|e| anyhow!("bad op: {e}"))?;
            let (lib, st) = c.split(s(p, "project")?)?;
            let out = st.project.apply(actor, &op, &|id| lib.exists(id), false)?;
            if matches!(out, ApplyOutcome::Applied { .. }) {
                *st.dirty.entry(actor.key()).or_default() += 1;
            }
            serde_json::to_value(out)?
        }

        "op.pending" => {
            let st = c.project(s(p, "project")?)?;
            serde_json::to_value(st.project.pending())?
        }

        "op.status" => {
            let id = s(p, "op")?;
            let st = c.project(s(p, "project")?)?;
            match st.project.pending_op(id) {
                Some(x) => serde_json::to_value(x)?,
                None => {
                    let applied = st
                        .project
                        .read_log(None, usize::MAX)?
                        .into_iter()
                        .any(|r| r.id == id);
                    if !applied {
                        bail!("unknown op {id}");
                    }
                    json!({ "id": id, "state": "applied" })
                }
            }
        }

        "op.decide" => {
            let approve = p.get("approve").and_then(Value::as_bool).unwrap_or(false);
            let (lib, st) = c.split(s(p, "project")?)?;
            let res = st
                .project
                .decide(s(p, "op")?, approve, actor, &|id| lib.exists(id))?;
            if res.state == "applied" {
                *st.dirty.entry(res.actor.key()).or_default() += 1;
            }
            serde_json::to_value(res)?
        }

        "undo" | "redo" => {
            let st = c.project(s(p, "project")?)?;
            let did = st.project.undo(actor, method == "redo")?;
            if did {
                *st.dirty.entry(actor.key()).or_default() += 1;
            }
            json!({ "done": did })
        }

        "log" => {
            let st = c.project(s(p, "project")?)?;
            let limit = p.get("limit").and_then(Value::as_u64).unwrap_or(50) as usize;
            serde_json::to_value(
                st.project
                    .read_log(p.get("actor").and_then(Value::as_str), limit)?,
            )?
        }

        "presence.list" => {
            let st = c.project(s(p, "project")?)?;
            serde_json::to_value(st.project.presence())?
        }

        "presence.set" => {
            let st = c.project(s(p, "project")?)?;
            let selection: Vec<String> =
                serde_json::from_value(p.get("select").cloned().unwrap_or(json!([])))
                    .unwrap_or_default();
            let playhead = p.get("playhead").and_then(Value::as_i64);
            serde_json::to_value(st.project.set_presence(actor, playhead, selection))?
        }

        "presence.clear" => {
            let st = c.project(s(p, "project")?)?;
            serde_json::to_value(st.project.clear_presence(actor))?
        }

        "ops.catalog" => json!(
            ops::CATALOG
                .iter()
                .map(|(k, v)| json!({ "type": k, "args": v }))
                .collect::<Vec<_>>()
        ),

        "lib.add" => {
            let paths: Vec<String> =
                serde_json::from_value(p.get("paths").cloned().unwrap_or(json!([])))?;
            let tags: Vec<String> =
                serde_json::from_value(p.get("tags").cloned().unwrap_or(json!([])))?;
            let mut added = vec![];
            for path in &paths {
                let a = c.library.add_path(
                    Path::new(path),
                    &AddOptions {
                        tags: &tags,
                        name: p.get("name").and_then(Value::as_str),
                        license: p.get("license").and_then(Value::as_str),
                        created_by: &actor.key(),
                        recipe: None,
                        parent_id: None,
                    },
                )?;
                added.push(a);
            }
            serde_json::to_value(added)?
        }

        "lib.get" => serde_json::to_value(
            c.library
                .get(s(p, "id")?)?
                .ok_or_else(|| anyhow!("unknown asset {}", s(p, "id").unwrap_or("")))?,
        )?,

        // Derived media for the timeline canvas (spec §7.4). The app's asset
        // server proxies these; they are not part of the agent-facing CLI.
        "lib.thumb" => {
            let (path, mime) = c.library.thumb(s(p, "id")?)?;
            json!({ "path": path.to_string_lossy(), "mime": mime })
        }
        "lib.peaks" => c.library.peaks(s(p, "id")?)?,

        "lib.search" => {
            let q = p.get("query").and_then(Value::as_str).unwrap_or("");
            let kind = p.get("kind").and_then(Value::as_str);
            let tag = p.get("tag").and_then(Value::as_str);
            let all = p.get("all").and_then(Value::as_bool).unwrap_or(false);
            let limit = p.get("limit").and_then(Value::as_u64).unwrap_or(50) as usize;
            if p.get("semantic").and_then(Value::as_bool).unwrap_or(false) {
                serde_json::to_value(c.library.semantic_search(q, kind, tag, all, limit)?)?
            } else {
                serde_json::to_value(c.library.search(q, kind, tag, all, limit)?)?
            }
        }

        "lib.tag" | "lib.untag" => {
            let id = s(p, "id")?;
            if !c.library.exists(id) {
                bail!("unknown asset {id}");
            }
            for t in p
                .get("tags")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
            {
                let t = t.as_str().unwrap_or_default();
                if method == "lib.tag" {
                    c.library.tag(id, t)?
                } else {
                    c.library.untag(id, t)?
                }
            }
            serde_json::to_value(c.library.get(id)?)?
        }

        "lib.deprecate" => {
            // Spec §14.5: agents can deprecate, never delete.
            c.library.deprecate(s(p, "id")?)?;
            json!({ "deprecated": s(p, "id")? })
        }

        "modules.list" => {
            let dirs = c.module_dirs(p.get("project").and_then(Value::as_str).map(Path::new));
            serde_json::to_value(modules::list(&dirs)?)?
        }

        "shutdown" => {
            c.shutdown_flush();
            json!({ "bye": true })
        }

        other => bail!("unknown method `{other}`"),
    })
}

/// Human decision on an over-budget module run (spec §14.5). Approval runs
/// the module synchronously, then marks the pending request applied.
fn decide_run(
    core: &Shared,
    actor: &Actor,
    p: &Value,
    emit: &mut dyn FnMut(Value),
) -> Result<Value> {
    if actor.is_agent() {
        bail!("only a human can approve a budget run");
    }
    let project = s(p, "project")?;
    let id = s(p, "op")?;
    let approve = p.get("approve").and_then(Value::as_bool).unwrap_or(false);
    let pending = {
        let mut c = core.lock().unwrap();
        let st = c.project(project)?;
        let pending = st
            .project
            .pending_op(id)
            .ok_or_else(|| anyhow!("no pending run {id}"))?;
        if pending.state != "pending" {
            bail!("run {id} is already {}", pending.state);
        }
        pending
    };
    if !approve {
        let mut c = core.lock().unwrap();
        let st = c.project(project)?;
        let out =
            st.project
                .set_pending_state(id, "rejected", Some(json!({ "by": actor.key() })))?;
        return Ok(serde_json::to_value(out)?);
    }

    let args = pending.op.args.clone();
    let runner = pending.actor.clone();
    let result = match pending.op.kind.as_str() {
        "module.run" => {
            let prun = json!({
                "project": project,
                "module": args.get("module"),
                "params": args.get("params"),
                "seed": args.get("seed"),
                "cached": args.get("cached"),
                "budget_override": true,
                "env": p.get("env"),
            });
            let job = {
                let mut c = core.lock().unwrap();
                prepare_run(&mut c, &runner, &prun)?
            };
            match job {
                Ok(job) => execute_job(core, job, &prun, emit),
                Err(cached) => Ok(cached),
            }
        }
        "module.variant" => {
            let prun = json!({
                "project": project,
                "id": args.get("id"),
                "set": args.get("set"),
                "seed": args.get("seed"),
                "budget_override": true,
                "env": p.get("env"),
            });
            let job = {
                let mut c = core.lock().unwrap();
                prepare_variant(&mut c, &runner, &prun)?
            };
            match job {
                Ok(job) => execute_job(core, job, &prun, emit),
                Err(cached) => Ok(cached),
            }
        }
        other => bail!("pending {other} is not a module run"),
    };
    let mut c = core.lock().unwrap();
    let st = c.project(project)?;
    match &result {
        Ok(v) => {
            st.project
                .set_pending_state(id, "applied", Some(v.clone()))?;
            Ok(v.clone())
        }
        Err(e) => {
            st.project.set_pending_state(
                id,
                "stale",
                Some(json!({ "error": format!("{e:#}") })),
            )?;
            Err(anyhow!("approved run failed: {e:#}"))
        }
    }
}

/// Run a prepared module job, ingest outputs, and account for spend.
fn execute_job(core: &Shared, job: Job, p: &Value, emit: &mut dyn FnMut(Value)) -> Result<Value> {
    let env: BTreeMap<String, String> =
        serde_json::from_value(p.get("env").cloned().unwrap_or(json!({}))).unwrap_or_default();
    let out = modules::run(
        &job.manifest,
        &job.request,
        job.cwd.as_deref(),
        &env,
        |e: &Event| emit(serde_json::to_value(e).unwrap_or(Value::Null)),
    )?;
    let mut c = core.lock().unwrap();
    let spent = if out.cost_usd > 0.0 {
        out.cost_usd
    } else if job.manifest.agent.cost_hint.as_deref() == Some("api") {
        job.manifest.agent.cost_estimate_usd.unwrap_or(0.0)
    } else {
        0.0
    };
    let budget = if let Some(project) = p.get("project").and_then(Value::as_str) {
        c.project(project)
            .and_then(|st| st.project.record_cost(spent))
            .ok()
    } else {
        None
    };
    if let Some(b) = &budget {
        if b["warning"] == json!(true) {
            emit(json!({
                "type": "log", "level": "warn",
                "msg": format!("API budget warning: ${:.2} of ${:.2} used", b["spent_usd"].as_f64().unwrap_or(0.0), b["limit_usd"].as_f64().unwrap_or(0.0))
            }));
        }
    }
    let res = match job.finish {
        Finish::Files => json!({
            "outputs": out.outputs.iter().map(|(n, p)| (n.clone(), json!(p))).collect::<Map<_, _>>(),
            "cost_usd": out.cost_usd,
            "budget": budget,
        }),
        Finish::Generate {
            recipe,
            created_by,
            parent,
            notes,
        } => {
            let mut assets = Map::new();
            for (name, path) in &out.outputs {
                let spec = job.manifest.outputs.get(name);
                if spec.is_some_and(|s| s.kind == "file") {
                    assets.insert(name.clone(), json!({ "path": path }));
                    continue;
                }
                let a = c.library.add_path(
                    path,
                    &AddOptions {
                        tags: &[],
                        name: None,
                        license: None,
                        created_by: &created_by,
                        recipe: Some(&recipe),
                        parent_id: parent.as_deref(),
                    },
                )?;
                assets.insert(name.clone(), serde_json::to_value(&a)?);
            }
            json!({ "assets": assets, "cost_usd": out.cost_usd, "cached": false, "notes": notes, "budget": budget })
        }
    };
    drop(job.work);
    Ok(res)
}

fn check_budget(
    c: &mut Core,
    project: Option<&Path>,
    m: &Manifest,
    force: bool,
) -> Result<Option<String>> {
    if force || std::env::var_os("KURN_BUDGET_OVERRIDE").is_some() {
        return Ok(None);
    }
    let Some(dir) = project else { return Ok(None) };
    let st = c.project(dir.to_string_lossy().as_ref())?;
    Ok(st.project.check_budget(
        m.agent.cost_hint.as_deref(),
        m.agent.cost_estimate_usd.unwrap_or(0.0),
    ))
}

fn prepare_run(c: &mut Core, actor: &Actor, p: &Value) -> Result<std::result::Result<Job, Value>> {
    let project = p.get("project").and_then(Value::as_str).map(PathBuf::from);
    let m = modules::find(&c.module_dirs(project.as_deref()), s(p, "module")?)?;
    if m.kind == "renderer" {
        bail!("{} is a renderer; use `kurn render`", m.id);
    }
    if let Some(reason) = check_budget(
        c,
        project.as_deref(),
        &m,
        p.get("budget_override")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    )? {
        let dir = project.as_ref().unwrap().to_string_lossy().into_owned();
        let st = c.project(&dir)?;
        let pending = st.project.request_pending_run(
            actor,
            Op::new(
                "module.run",
                json!({
                    "module": m.id,
                    "params": p.get("params").cloned().unwrap_or(json!({})),
                    "seed": p.get("seed"),
                    "cached": p.get("cached"),
                    "project": dir,
                }),
            ),
            reason,
        )?;
        return Ok(Err(json!({
            "status": "pending", "op": pending.id,
            "reason": pending.reason, "expires_at": pending.expires_at
        })));
    }
    let given = p
        .get("params")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let params = modules::resolve_params(&m, &given)?;
    let seed = p.get("seed").and_then(Value::as_i64);
    build_generate_job(
        c,
        m,
        params,
        seed,
        project,
        actor.key(),
        None,
        vec![],
        p.get("cached").and_then(Value::as_bool).unwrap_or(false),
    )
}

fn prepare_variant(
    c: &mut Core,
    actor: &Actor,
    p: &Value,
) -> Result<std::result::Result<Job, Value>> {
    let id = s(p, "id")?;
    let a: Asset = c
        .library
        .get(id)?
        .ok_or_else(|| anyhow!("unknown asset {id}"))?;
    let rid = a
        .recipe_id
        .ok_or_else(|| anyhow!("{id} was imported, not generated; it has no recipe to vary"))?;
    let r = c
        .library
        .recipe(&rid)?
        .ok_or_else(|| anyhow!("recipe {rid} missing"))?;
    let project = p.get("project").and_then(Value::as_str).map(PathBuf::from);
    let m = modules::find(&c.module_dirs(project.as_deref()), &r.module)?;
    if let Some(reason) = check_budget(
        c,
        project.as_deref(),
        &m,
        p.get("budget_override")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    )? {
        let dir = project.as_ref().unwrap().to_string_lossy().into_owned();
        let st = c.project(&dir)?;
        let pending = st.project.request_pending_run(
            actor,
            Op::new(
                "module.variant",
                json!({
                    "id": id,
                    "set": p.get("set").cloned().unwrap_or(json!([])),
                    "seed": p.get("seed"),
                    "project": dir,
                }),
            ),
            reason,
        )?;
        return Ok(Err(json!({
            "status": "pending", "op": pending.id,
            "reason": pending.reason, "expires_at": pending.expires_at
        })));
    }
    let sets: Vec<String> = serde_json::from_value(p.get("set").cloned().unwrap_or(json!([])))?;
    let base = r.params.as_object().cloned().unwrap_or_default();
    let (params, notes) = modules::apply_set(&m, &base, &sets)?;
    let seed = p.get("seed").and_then(Value::as_i64).or(r.seed);
    build_generate_job(
        c,
        m,
        params,
        seed,
        project,
        actor.key(),
        Some(id.to_string()),
        notes,
        false,
    )
}

#[allow(clippy::too_many_arguments)]
fn build_generate_job(
    c: &mut Core,
    m: Manifest,
    params: Map<String, Value>,
    seed: Option<i64>,
    project: Option<PathBuf>,
    created_by: String,
    parent: Option<String>,
    notes: Vec<String>,
    force_cache: bool,
) -> Result<std::result::Result<Job, Value>> {
    let inputs: Vec<String> = m
        .inputs
        .iter()
        .filter(|(_, s)| s.kind == "asset")
        .filter_map(|(k, _)| params.get(k).and_then(Value::as_str).map(String::from))
        .collect();
    let recipe = Recipe {
        module: m.id.clone(),
        module_version: m.version.clone(),
        params: Value::Object(params.clone()),
        inputs,
        seed,
    };
    if m.deterministic || force_cache {
        if let Some(hit) = c.library.find_by_recipe(&recipe)?.pop() {
            let name = m.outputs.keys().next().cloned().unwrap_or("asset".into());
            return Ok(Err(
                json!({ "assets": { name: hit }, "cost_usd": 0.0, "cached": true, "notes": notes }),
            ));
        }
    }
    // Resolve asset-typed inputs to {id, path, kind, …} for the module.
    let mut req = params.clone();
    for (k, spec) in &m.inputs {
        if spec.kind == "asset" {
            if let Some(id) = params.get(k).and_then(Value::as_str) {
                let r = c.asset_ref(id)?;
                if let Some(want) = &spec.asset_kind {
                    if r["kind"] != json!(want) {
                        bail!("--{k}: {id} is {}, expected {want}", r["kind"]);
                    }
                }
                req.insert(k.clone(), r);
            }
        }
    }
    let work = tempdir::Dir::new("kurn-job")?;
    req.insert(
        "_ctx".into(),
        json!({
            "work_dir": work.path(),
            "project_root": project.as_ref().and_then(|p| p.canonicalize().ok()),
            "seed": seed,
            "module_dir": m.dir,
        }),
    );
    Ok(Ok(Job {
        manifest: m,
        request: Value::Object(req),
        cwd: None,
        work: Some(work),
        finish: Finish::Generate {
            recipe,
            created_by,
            parent,
            notes,
        },
    }))
}

fn prepare_render(c: &mut Core, p: &Value) -> Result<std::result::Result<Job, Value>> {
    let dir = s(p, "project")?.to_string();
    let st = c.project(&dir)?;
    st.project.export_snapshot()?; // spec §10.1: render from a fresh export
    let tl = st.project.timeline()?;
    let root = st.project.root.clone();
    let mut assets = Map::new();
    for id in tl.clips.values().filter_map(|c| c.asset.clone()) {
        if !assets.contains_key(&id) {
            assets.insert(id.clone(), c.asset_ref(&id)?);
        }
    }
    let m = modules::find(
        &c.module_dirs(Some(&root)),
        p.get("module")
            .and_then(Value::as_str)
            .unwrap_or("render.revideo"),
    )?;
    let mode = p.get("mode").and_then(Value::as_str).unwrap_or("final");
    let out = s(p, "out")?;
    if let Some(dir) = Path::new(out).parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut req = json!({ "timeline": tl, "assets": assets, "mode": mode, "out": out });
    for k in ["frame", "range", "scale", "background"] {
        if let Some(v) = p.get(k).filter(|v| !v.is_null()) {
            req[k] = v.clone();
        }
    }
    Ok(Ok(Job {
        cwd: Some(m.dir.clone()),
        manifest: m,
        request: req,
        work: None,
        finish: Finish::Files,
    }))
}

/// Agent permission config (spec §14.2, layer 1): Claude Code deny rules so
/// agents can't edit snapshots, core state, or the library directly. Created
/// once; never overwrites a settings file the user has edited.
pub fn write_agent_settings(root: &Path, library: &Path) -> Result<()> {
    let dir = root.join(".claude");
    let file = dir.join("settings.json");
    if file.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(&dir)?;
    // `/x` is relative to the project; `//abs` is an absolute path. Edit rules also cover Write.
    let lib = format!("Edit(/{}/**)", library.to_string_lossy());
    let settings = json!({
        "permissions": {
            "allow": ["Bash(kurn *)"],
            "deny": ["Edit(/timeline.json)", "Edit(/board.excalidraw)", "Edit(/.ws/**)", lib]
        }
    });
    std::fs::write(&file, serde_json::to_string_pretty(&settings)? + "\n")?;
    Ok(())
}

/// Claude Code reads CLAUDE.md as project memory; importing AGENTS.md gives
/// every new Claude pane the project rules without spending a user prompt.
/// Never overwrites a CLAUDE.md the user wrote.
pub fn write_claude_md(root: &Path) -> Result<()> {
    let file = root.join("CLAUDE.md");
    if !file.exists() {
        std::fs::write(&file, "# Kurnace project\n\n@AGENTS.md\n")?;
    } else {
        let txt = std::fs::read_to_string(&file)?;
        if !txt.contains("@AGENTS.md") {
            std::fs::write(&file, format!("{txt}\n@AGENTS.md\n"))?;
        }
    }
    Ok(())
}

/// Generated per project (spec §14.3).
pub fn write_agents_md(root: &Path, module_dirs: &[PathBuf]) -> Result<()> {
    let mods = modules::list(module_dirs).unwrap_or_default();
    let mut md = String::from(
        "# AGENTS.md — Kurnace project conventions\n\n\
         _Generated by `kurn`. Regenerate with `kurn agents-md`._\n\n\
         ## Rules\n\
         - Change the timeline **only through `kurn`** (`kurn op <type> --args <json>`, `kurn op batch`, `kurn undo`). Never edit `timeline.json`, `board.excalidraw`, `.ws/`, or the library directly; both snapshots are exports and will be overwritten.\n\
         - Storyboard on the board: `kurn board add --json '[…]'`, `kurn board frame <name> --at x,y`, `kurn board snapshot --png renders/board.png`, `kurn board to-timeline`.\n\
         - You may edit `scenes/`, `modules/`, `storyboard.yaml`, `brief.md`.\n\
         - **Look before you claim.** After any visual change run `kurn render still --frame N` and inspect the PNG. Never judge from the preview.\n\
         - Times are **integer frames** at the project fps. Keyframe `t` is clip-local.\n\
         - Group related edits into one `kurn op batch` (atomic, one undo step).\n\
         - Destructive ops (track.remove, ripple.delete, removing >5 clips) return exit code 3 and wait for a human: `kurn op wait <id>`.\n\
         - You can't delete library assets; use `kurn lib deprecate`.\n\
         - Identify yourself: set `KURN_ACTOR=agent:<name>`.\n\
         - Publish your cursor so the human sees where you are: `kurn presence set --playhead <frame> --select c_1,c_2` (repeat or `kurn presence clear` when done).\n\
         - API modules are budgeted by `[agents].api_budget_usd`; check `kurn budget` and never bypass with `KURN_BUDGET_OVERRIDE` unless a human asked.\n\
         - Package a finished render with `kurn run export.mp4 --input renders/final.mp4 --preset youtube --out renders/youtube.mp4`.\n\n\
         ## Rendering\n\
         ```\nkurn render still --frame 240          # → renders/f240.png\nkurn render draft --range 0-300 --scale 0.5\nkurn render final\n```\n\n\
         ## Ops (`kurn op <type> --args '<json>'`)\n",
    );
    for (k, v) in ops::CATALOG {
        md.push_str(&format!("- `{k}` {v}\n"));
    }
    md.push_str("\n## Modules (`kurn run <id> --<input> <value>`)\n");
    for m in mods.iter().filter(|m| m.agent.expose) {
        md.push_str(&format!(
            "\n### `{}` — {}\n{}\n",
            m.id,
            m.kind,
            m.description.clone().unwrap_or_default()
        ));
        for (k, i) in &m.inputs {
            let mut bits = vec![i.kind.clone()];
            if i.required {
                bits.push("required".into());
            }
            if let Some(d) = &i.default {
                bits.push(format!("default {d}"));
            }
            if let (Some(lo), Some(hi)) = (i.min, i.max) {
                bits.push(format!("{lo}–{hi}"));
            }
            md.push_str(&format!("- `--{k}` ({})\n", bits.join(", ")));
        }
        if let Some(h) = &m.agent.cost_hint {
            md.push_str(&format!("- cost: {h}\n"));
        }
    }
    std::fs::write(root.join("AGENTS.md"), md)?;
    Ok(())
}

// ---------- server ----------

fn error_reply(id: &Value, e: &anyhow::Error) -> Value {
    if let Some(v) = e.downcast_ref::<ValidationError>() {
        return json!({ "id": id, "ok": false, "code": CODE_VALIDATION, "error": e.to_string(), "issues": v.0 });
    }
    if let Some(v) = e.downcast_ref::<BudgetExceeded>() {
        return json!({ "id": id, "ok": false, "code": CODE_PENDING, "error": v.0 });
    }
    json!({ "id": id, "ok": false, "code": CODE_ERROR, "error": format!("{e:#}") })
}

fn subscribe_stream(
    core: &Shared,
    params: &Value,
    id: &Value,
    w: &mut crate::transport::Conn,
) -> Result<()> {
    use crate::project::Push;
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD;
    let (state, board_state, presence, pending, rx, root) = {
        let mut c = core.lock().unwrap();
        match s(params, "project").and_then(|p| c.project(p)) {
            Ok(st) => {
                let (state, board_state, rx) = st.project.subscribe();
                let presence = st.project.presence();
                let pending = st.project.pending();
                (
                    state,
                    board_state,
                    presence,
                    pending,
                    rx,
                    st.project.root.clone(),
                )
            }
            Err(e) => {
                writeln!(w, "{}", error_reply(id, &e))?;
                return Ok(());
            }
        }
    };
    writeln!(
        w,
        "{}",
        json!({ "id": id, "ok": true, "result": { "project": root, "state": b64.encode(&state), "board_state": b64.encode(&board_state), "presence": presence, "pending": pending } })
    )?;
    for msg in rx {
        let ev = match msg {
            Push::Update {
                doc,
                update,
                origin,
            } => {
                json!({ "type": "update", "doc": doc, "update": b64.encode(&update), "origin": origin })
            }
            Push::Op(rec) => json!({ "type": "op", "record": rec }),
            Push::Presence(presence) => json!({ "type": "presence", "presence": presence }),
            Push::Pending(pending) => json!({ "type": "pending", "pending": pending }),
        };
        if writeln!(w, "{}", json!({ "id": id, "event": ev })).is_err() {
            break; // client gone; the sender is dropped on next fan-out
        }
    }
    Ok(())
}

fn serve_conn(core: Shared, stream: crate::transport::Conn) -> Result<()> {
    let mut w = stream.try_clone()?;
    for line in BufReader::new(stream).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                writeln!(
                    w,
                    "{}",
                    json!({ "ok": false, "code": 1, "error": format!("bad request: {e}") })
                )?;
                continue;
            }
        };
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let method = req.get("method").and_then(Value::as_str).unwrap_or("");
        let params = req.get("params").cloned().unwrap_or(json!({}));
        let actor = req
            .get("actor")
            .and_then(Value::as_str)
            .map(Actor::parse)
            .transpose();
        if method == "doc.subscribe" {
            // Long-lived: reply with full state, then stream until the client hangs up.
            return subscribe_stream(&core, &params, &id, &mut w);
        }
        let reply = match actor {
            Err(e) => error_reply(&id, &e),
            Ok(actor) => {
                let actor = actor.unwrap_or(Actor {
                    kind: "human".into(),
                    name: "unknown".into(),
                    session: None,
                });
                let mut w2 = w.try_clone()?;
                let mut emit = |ev: Value| {
                    let _ = writeln!(w2, "{}", json!({ "id": id, "event": ev }));
                };
                match handle(&core, method, &actor, &params, &mut emit) {
                    Ok(result) => {
                        let pending =
                            result.get("status").and_then(Value::as_str) == Some("pending");
                        json!({ "id": id, "ok": true, "result": result, "code": if pending { CODE_PENDING } else { 0 } })
                    }
                    Err(e) => error_reply(&id, &e),
                }
            }
        };
        writeln!(w, "{reply}")?;
        if method == "shutdown" {
            crate::transport::cleanup(&socket_path());
            std::process::exit(0);
        }
    }
    Ok(())
}

/// Run the daemon in the foreground. Returns only on fatal error.
pub fn serve() -> Result<()> {
    let sock = socket_path();
    #[cfg(unix)]
    if sock.as_os_str().len() >= 104 {
        bail!(
            "socket path {} is too long for a Unix socket (max 103 bytes); set KURN_SOCKET to a shorter path",
            sock.display()
        );
    }
    std::fs::create_dir_all(sock.parent().unwrap())?;
    let listener = crate::transport::bind(&sock)?;
    let core: Shared = Arc::new(Mutex::new(Core::new(&home_dir())?));
    eprintln!(
        "kurn-core: listening on {} (home {})",
        sock.display(),
        home_dir().display()
    );

    let ticker = core.clone();
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(5));
            let idle = std::env::var("KURN_IDLE_COMMIT_S")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(30);
            ticker.lock().unwrap().idle_tick(Duration::from_secs(idle));
        }
    });

    for conn in listener.incoming() {
        let Ok(conn) = conn else { continue };
        let core = core.clone();
        std::thread::spawn(move || {
            if let Err(e) = serve_conn(core, conn) {
                eprintln!("kurn-core: connection error: {e}");
            }
        });
    }
    Ok(())
}
