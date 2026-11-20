//! A project's live state, hosted by the core (spec §5, §6).
use crate::ops::{self, Op};
use crate::store::DocStore;
use crate::timeline::{self, Issue, Rational, Timeline};
use crate::ydoc;
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use yrs::undo::Options as UndoOptions;
use yrs::updates::decoder::Decode;
use yrs::{Doc, Origin, ReadTxn, StateVector, Transact, UndoManager, Update};

// ---------- actors ----------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct Actor {
    #[serde(rename = "type")]
    pub kind: String, // human | agent
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

impl Actor {
    /// Parse `agent:claude`, `human:night`, or `agent:claude@herdr:pane-3`.
    pub fn parse(s: &str) -> Result<Actor> {
        let (kind, rest) = s
            .split_once(':')
            .ok_or_else(|| anyhow!("actor must look like `agent:name` or `human:name`"))?;
        if kind != "agent" && kind != "human" {
            bail!("actor type must be `agent` or `human`");
        }
        let (name, session) = match rest.split_once('@') {
            Some((n, sess)) => (n, Some(sess.to_string())),
            None => (rest, None),
        };
        if name.is_empty() {
            bail!("actor name is empty");
        }
        Ok(Actor {
            kind: kind.into(),
            name: name.into(),
            session,
        })
    }
    pub fn key(&self) -> String {
        format!("{}:{}", self.kind, self.name)
    }
    pub fn is_agent(&self) -> bool {
        self.kind == "agent"
    }
}

// ---------- config ----------

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProjectConfig {
    pub name: String,
    #[serde(default = "default_fps")]
    pub fps: String,
    #[serde(default = "default_w")]
    pub width: i64,
    #[serde(default = "default_h")]
    pub height: i64,
    #[serde(default)]
    pub agents: AgentsConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AgentsConfig {
    #[serde(default = "yes")]
    pub agent_confirm_destructive: bool,
    #[serde(default = "five")]
    pub destructive_threshold: usize,
    #[serde(default = "ten_min")]
    pub pending_expiry_s: u64,
    /// Module API spend only (spec §14.5). None = no limit.
    #[serde(default)]
    pub api_budget_usd: Option<f64>,
    #[serde(default = "eighty")]
    pub api_budget_warn_pct: f64,
}

impl Default for AgentsConfig {
    fn default() -> Self {
        AgentsConfig {
            agent_confirm_destructive: true,
            destructive_threshold: 5,
            pending_expiry_s: 600,
            api_budget_usd: None,
            api_budget_warn_pct: 80.0,
        }
    }
}

fn default_fps() -> String {
    "30/1".into()
}
fn default_w() -> i64 {
    1920
}
fn default_h() -> i64 {
    1080
}
fn yes() -> bool {
    true
}
fn five() -> usize {
    5
}
fn ten_min() -> u64 {
    600
}
fn eighty() -> f64 {
    80.0
}

pub fn parse_fps(s: &str) -> Result<Rational> {
    let (n, d) = s.split_once('/').unwrap_or((s, "1"));
    let r = Rational {
        num: n.trim().parse()?,
        den: d.trim().parse()?,
    };
    if r.num <= 0 || r.den <= 0 {
        bail!("fps must be positive");
    }
    Ok(r)
}

// ---------- results ----------

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum ApplyOutcome {
    Applied {
        op: String,
        result: Value,
        warnings: Vec<String>,
    },
    Pending {
        op: String,
        reason: String,
        expires_at: u64,
    },
}

#[derive(Debug, thiserror::Error)]
#[error("validation failed: {}", .0.iter().map(|i| i.msg.as_str()).collect::<Vec<_>>().join("; "))]
pub struct ValidationError(pub Vec<Issue>);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingOp {
    pub id: String,
    pub actor: Actor,
    pub op: Op,
    pub created: u64,
    pub expires_at: u64,
    pub reason: String,
    #[serde(default)]
    pub state: String, // pending | applied | rejected | expired | stale
    #[serde(default)]
    pub detail: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpRecord {
    pub id: String,
    pub actor: Actor,
    pub ts: String,
    pub doc: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub args: Value,
    #[serde(default)]
    pub derived: bool,
}

/// Live cursor/selection for an actor (spec §7.4). Ephemeral: kept in memory and
/// expired if the actor stops heartbeating.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Presence {
    pub actor: Actor,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playhead: Option<i64>,
    #[serde(default)]
    pub selection: Vec<String>,
    pub updated_at: u64,
}

pub const PRESENCE_TTL_S: u64 = 60;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BudgetState {
    #[serde(default)]
    pub spent_usd: f64,
    #[serde(default)]
    pub updated_at: String,
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
}

pub fn now_rfc3339() -> String {
    // Minimal UTC RFC 3339 without pulling in chrono.
    let secs = now_secs() as i64;
    let (days, rem) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    let (h, m, s) = (rem / 3600, rem % 3600 / 60, rem % 60);
    // Civil from days (Howard Hinnant).
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

// ---------- project ----------

/// Messages pushed to subscribers.
#[derive(Debug, Clone)]
pub enum Push {
    /// A Yjs v1 update, with the document and transaction origin when known.
    Update {
        doc: &'static str,
        update: Vec<u8>,
        origin: Option<String>,
    },
    Op(OpRecord),
    /// Full live presence snapshot after a change.
    Presence(Vec<Presence>),
    /// Full pending-confirmation snapshot after a change.
    Pending(Vec<PendingOp>),
    /// Files in `scenes/<name>/` changed (hot reload in the preview).
    Scene { name: String },
}

pub type Subscribers = Arc<Mutex<Vec<std::sync::mpsc::Sender<Push>>>>;

/// Watch `scenes/` and push one Push::Scene per changed scene, coalescing the
/// burst of events editors and agents produce for a single save (~80 ms).
fn start_scene_watcher(root: &Path, subs: Subscribers) -> Option<notify::RecommendedWatcher> {
    use notify::Watcher;
    let dir = crate::scenes::scenes_dir(root);
    if fs::create_dir_all(&dir).is_err() {
        return None;
    }
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let base = dir.canonicalize().unwrap_or(dir.clone());
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(ev) = res else { return };
        if matches!(ev.kind, notify::EventKind::Access(_)) {
            return;
        }
        for p in ev.paths {
            let p = p.canonicalize().unwrap_or(p);
            if let Ok(rel) = p.strip_prefix(&base) {
                if let Some(first) = rel.components().next() {
                    let _ = tx.send(first.as_os_str().to_string_lossy().into_owned());
                }
            }
        }
    })
    .ok()?;
    watcher.watch(&dir, notify::RecursiveMode::Recursive).ok()?;
    std::thread::spawn(move || {
        while let Ok(first) = rx.recv() {
            let mut names = std::collections::BTreeSet::from([first]);
            while let Ok(more) = rx.recv_timeout(Duration::from_millis(80)) {
                names.insert(more);
            }
            for name in names {
                fan_out(&subs, Push::Scene { name });
            }
        }
    });
    Some(watcher)
}

fn fan_out(subs: &Subscribers, msg: Push) {
    // Drop subscribers whose receiver is gone.
    subs.lock()
        .unwrap()
        .retain(|tx| tx.send(msg.clone()).is_ok());
}

pub struct Project {
    pub root: PathBuf,
    pub config: ProjectConfig,
    pub doc: Doc,
    pub board_doc: Doc,
    store: Arc<Mutex<DocStore>>,
    board_store: Arc<Mutex<DocStore>>,
    store_err: Arc<Mutex<Option<String>>>,
    board_store_err: Arc<Mutex<Option<String>>>,
    /// Live subscribers (the app's webview replica, activity feeds): doc updates and op records.
    subscribers: Subscribers,
    /// Watches `scenes/` and pushes Push::Scene for hot reload.
    _scene_watcher: Option<notify::RecommendedWatcher>,
    undo: HashMap<String, UndoManager>,
    board_undo: HashMap<String, UndoManager>,
    last_doc: HashMap<String, &'static str>,
    pending: BTreeMap<String, PendingOp>,
    presence: BTreeMap<String, Presence>,
    budget: BudgetState,
    last_export: Option<String>,
    last_board_export: Option<String>,
    pub last_op_at: SystemTime,
}

pub fn ws_dir(root: &Path) -> PathBuf {
    root.join(".ws")
}

impl Project {
    pub fn load_config(root: &Path) -> Result<ProjectConfig> {
        let p = root.join("project.toml");
        let txt = fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
        toml::from_str(&txt).with_context(|| format!("parsing {}", p.display()))
    }

    /// Create a new project directory.
    pub fn create(root: &Path, name: &str, fps: &str, width: i64, height: i64) -> Result<()> {
        if root.join("project.toml").exists() {
            bail!("{} is already a project", root.display());
        }
        parse_fps(fps)?;
        fs::create_dir_all(root.join("scenes"))?;
        fs::create_dir_all(root.join("renders"))?;
        let cfg = ProjectConfig {
            name: name.into(),
            fps: fps.into(),
            width,
            height,
            agents: AgentsConfig::default(),
        };
        fs::write(root.join("project.toml"), toml::to_string_pretty(&cfg)?)?;
        fs::write(
            root.join("brief.md"),
            format!("# {name}\n\nIdea, tone, references.\n"),
        )?;
        fs::write(root.join("storyboard.yaml"), "scenes: []\n")?;
        fs::write(root.join(".gitignore"), ".ws/\nrenders/\n")?;
        Ok(())
    }

    pub fn open(root: &Path) -> Result<Project> {
        let root = root
            .canonicalize()
            .with_context(|| format!("no project at {}", root.display()))?;
        let config = Self::load_config(&root)?;
        let doc = Doc::new();
        let board_doc = Doc::new();
        let docs = ws_dir(&root).join("docs");
        let (store, existed) = DocStore::open(&docs, "timeline", &doc)?;
        let (board_store, board_existed) = DocStore::open(&docs, "board", &board_doc)?;
        let store = Arc::new(Mutex::new(store));
        let board_store = Arc::new(Mutex::new(board_store));
        let store_err = Arc::new(Mutex::new(None));
        let board_store_err = Arc::new(Mutex::new(None));

        let subscribers: Subscribers = Arc::new(Mutex::new(vec![]));
        let (st, se, subs) = (store.clone(), store_err.clone(), subscribers.clone());
        doc.observe_update_v1("kurn-persist", move |txn, e: &yrs::UpdateEvent| {
            if let Err(err) = st.lock().unwrap().append(&e.update) {
                *se.lock().unwrap() = Some(err.to_string());
            }
            let origin = txn
                .origin()
                .map(|o| String::from_utf8_lossy(o.as_ref()).into_owned());
            fan_out(
                &subs,
                Push::Update {
                    doc: "timeline",
                    update: e.update.clone(),
                    origin,
                },
            );
        })
        .map_err(|e| anyhow!("observe timeline: {e}"))?;
        let (bst, bse, bsubs) = (
            board_store.clone(),
            board_store_err.clone(),
            subscribers.clone(),
        );
        board_doc
            .observe_update_v1("kurn-persist", move |txn, e: &yrs::UpdateEvent| {
                if let Err(err) = bst.lock().unwrap().append(&e.update) {
                    *bse.lock().unwrap() = Some(err.to_string());
                }
                let origin = txn
                    .origin()
                    .map(|o| String::from_utf8_lossy(o.as_ref()).into_owned());
                fan_out(
                    &bsubs,
                    Push::Update {
                        doc: "board",
                        update: e.update.clone(),
                        origin,
                    },
                );
            })
            .map_err(|e| anyhow!("observe board: {e}"))?;

        let mut p = Project {
            root: root.clone(),
            config,
            doc,
            board_doc,
            store,
            board_store,
            store_err,
            board_store_err,
            _scene_watcher: start_scene_watcher(&root, subscribers.clone()),
            subscribers,
            undo: HashMap::new(),
            board_undo: HashMap::new(),
            last_doc: HashMap::new(),
            pending: BTreeMap::new(),
            presence: BTreeMap::new(),
            budget: BudgetState::default(),
            last_export: None,
            last_board_export: None,
            last_op_at: SystemTime::now(),
        };

        if !existed {
            // First open: import timeline.json if present, else start empty.
            let snap = root.join("timeline.json");
            let tl = if snap.exists() {
                let tl: Timeline = serde_json::from_str(&fs::read_to_string(&snap)?)
                    .with_context(|| format!("importing {}", snap.display()))?;
                tl
            } else {
                Timeline::empty(parse_fps(&p.config.fps)?, p.config.width, p.config.height)
            };
            let r = ydoc::root(&p.doc);
            ydoc::init_timeline(&mut p.doc.transact_mut_with("import"), &r, &tl);
            p.check_store()?;
        }
        if !board_existed {
            let snap = root.join("board.excalidraw");
            if snap.exists() {
                let v: Value = serde_json::from_str(&fs::read_to_string(&snap)?)
                    .with_context(|| format!("importing {}", snap.display()))?;
                crate::board::import_snapshot(&p.board_doc, &v)?;
                p.check_board_store()?;
            }
        }
        p.pending = Self::read_pending(&root);
        p.budget = Self::read_budget(&root);
        p.last_export = fs::read_to_string(root.join("timeline.json")).ok();
        p.last_board_export = fs::read_to_string(root.join("board.excalidraw")).ok();
        p.export_snapshot()?;
        p.export_board_snapshot()?;
        Ok(p)
    }

    fn check_store(&self) -> Result<()> {
        if let Some(e) = self.store_err.lock().unwrap().take() {
            bail!("persisting update failed: {e}");
        }
        Ok(())
    }

    fn check_board_store(&self) -> Result<()> {
        if let Some(e) = self.board_store_err.lock().unwrap().take() {
            bail!("persisting board update failed: {e}");
        }
        Ok(())
    }

    pub fn timeline(&self) -> Result<Timeline> {
        // Resolve the root BEFORE opening the read txn: get_or_insert_map needs
        // a write txn and would deadlock against our own read txn.
        let root = ydoc::root(&self.doc);
        ydoc::read_timeline(&self.doc.transact(), &root)
    }

    pub fn board_elements(&self) -> Result<Vec<Value>> {
        crate::board::elements(&self.board_doc)
    }

    /// Explicit board op (CLI/agents). Applied on a scratch copy first so a
    /// failing `batch` leaves the live board untouched.
    pub fn apply_board(&mut self, actor: &Actor, op: &Op) -> Result<ApplyOutcome> {
        let id = ops::gen_id("op");
        let scratch = Doc::with_client_id(self.board_doc.client_id().get());
        let full = self
            .board_doc
            .transact()
            .encode_state_as_update_v1(&StateVector::default());
        scratch
            .transact_mut()
            .apply_update(Update::decode_v1(&full)?)?;
        let sarr = scratch.get_or_insert_array(crate::board::ELEMENTS);
        let result = {
            let mut txn = scratch.transact_mut_with(actor.key().as_str());
            crate::board::apply(&mut txn, &sarr, op)?
        };
        let diff = scratch
            .transact()
            .encode_diff_v1(&self.board_doc.transact().state_vector());
        self.board_undo_manager(actor);
        self.last_doc.insert(actor.key(), "board");
        self.board_doc.get_or_insert_array(crate::board::ELEMENTS);
        {
            let mut txn = self.board_doc.transact_mut_with(actor.key().as_str());
            txn.apply_update(Update::decode_v1(&diff)?)?;
        }
        self.check_board_store()?;
        self.log_op(&id, actor, "board", op, false)?;
        self.after_board_change()?;
        Ok(ApplyOutcome::Applied {
            op: id,
            result,
            warnings: vec![],
        })
    }

    /// A webview's direct Yjs edit (spec §6.1 derived ops): apply as-is, log
    /// attribution, and let subscribers see the resulting update.
    pub fn apply_board_derived(
        &mut self,
        actor: &Actor,
        update: &[u8],
        op: Option<&Op>,
    ) -> Result<()> {
        let decoded = Update::decode_v1(update)?;
        self.board_doc.get_or_insert_array(crate::board::ELEMENTS);
        let origin = format!("derived:{}", actor.key());
        {
            let mut txn = self.board_doc.transact_mut_with(origin.as_str());
            txn.apply_update(decoded)?;
        }
        self.check_board_store()?;
        if let Some(op) = op {
            let id = ops::gen_id("op");
            self.log_op(&id, actor, "board", op, true)?;
        }
        self.after_board_change()?;
        Ok(())
    }

    pub fn board_to_timeline(
        &mut self,
        actor: &Actor,
        asset_meta: &dyn Fn(&str) -> Option<crate::board::AssetMeta>,
        asset_exists: &dyn Fn(&str) -> bool,
    ) -> Result<Value> {
        let elements = self.board_elements()?;
        let tl = self.timeline()?;
        let fps = {
            let r = parse_fps(&self.config.fps)?;
            r.num as f64 / r.den as f64
        };
        let (ops, details) = crate::board::to_timeline_ops(&elements, &tl, fps, asset_meta)?;
        let batch = Op::new("batch", json!({ "ops": ops }));
        let out = self.apply(actor, &batch, asset_exists, false)?;
        Ok(json!({ "result": out, "details": details }))
    }

    fn after_board_change(&mut self) -> Result<()> {
        self.last_op_at = SystemTime::now();
        self.export_board_snapshot()?;
        let frames = self.board_store.lock().unwrap().frames_since_compact;
        if frames >= 200 {
            self.compact()?;
        }
        Ok(())
    }

    fn undo_manager(&mut self, actor: &Actor) -> &mut UndoManager {
        let key = actor.key();
        let doc = &self.doc;
        self.undo.entry(key.clone()).or_insert_with(|| {
            let mut opts = UndoOptions::default();
            opts.capture_timeout_millis = 0; // every op is its own undo step
            opts.tracked_origins.insert(Origin::from(key.as_str()));
            let mut um = UndoManager::with_options(opts);
            um.expand_scope(doc, &ydoc::root(doc));
            um
        })
    }

    fn board_undo_manager(&mut self, actor: &Actor) -> &mut UndoManager {
        let key = actor.key();
        let doc = &self.board_doc;
        let elements = doc.get_or_insert_array(crate::board::ELEMENTS);
        self.board_undo.entry(key.clone()).or_insert_with(|| {
            let mut opts = UndoOptions::default();
            opts.capture_timeout_millis = 0;
            opts.tracked_origins.insert(Origin::from(key.as_str()));
            let mut um = UndoManager::with_options(opts);
            um.expand_scope(doc, &elements);
            um
        })
    }

    /// Validate-then-commit. `asset_exists` checks clip asset refs against the library.
    pub fn apply(
        &mut self,
        actor: &Actor,
        op: &Op,
        asset_exists: &dyn Fn(&str) -> bool,
        skip_guard: bool,
    ) -> Result<ApplyOutcome> {
        let id = ops::gen_id("op");
        let cfg = &self.config.agents;
        if !skip_guard
            && actor.is_agent()
            && cfg.agent_confirm_destructive
            && ops::is_destructive(op, cfg.destructive_threshold)
        {
            let now = now_secs();
            let p = PendingOp {
                id: id.clone(),
                actor: actor.clone(),
                op: op.clone(),
                created: now,
                expires_at: now + cfg.pending_expiry_s,
                reason: format!(
                    "destructive op `{}` by an agent needs human confirmation",
                    op.kind
                ),
                state: "pending".into(),
                detail: None,
            };
            let out = ApplyOutcome::Pending {
                op: id.clone(),
                reason: p.reason.clone(),
                expires_at: p.expires_at,
            };
            self.pending.insert(id, p);
            self.write_pending()?;
            let all = self.pending();
            fan_out(&self.subscribers, Push::Pending(all));
            return Ok(out);
        }

        // 1. Scratch copy with the same client id, so the diff continues our clock.
        let scratch = Doc::with_client_id(self.doc.client_id().get());
        let full = self
            .doc
            .transact()
            .encode_state_as_update_v1(&StateVector::default());
        scratch
            .transact_mut()
            .apply_update(Update::decode_v1(&full)?)?;
        let sroot = ydoc::root(&scratch);

        // 2. Apply on scratch.
        let result = {
            let mut txn = scratch.transact_mut();
            let r = ops::apply(&mut txn, &sroot, op)?;
            ops::grow_duration(&mut txn, &sroot)?;
            r
        };

        // 3. Materialize + validate.
        let tl = ydoc::read_timeline(&scratch.transact(), &sroot)?;
        let refs: HashSet<String> = tl.clips.values().filter_map(|c| c.asset.clone()).collect();
        let known: HashSet<String> = refs.into_iter().filter(|a| asset_exists(a)).collect();
        let mut issues = timeline::validate(&tl, Some(&known));
        // Scene clips must point at a scene folder in this project.
        for (id, c) in &tl.clips {
            if let Some(name) = &c.scene {
                if !crate::scenes::exists(&self.root, name) {
                    issues.push(timeline::Issue {
                        level: timeline::Level::Error,
                        msg: format!("clip {id}: unknown scene {name:?} (create it with `kurn scene new {name}`)"),
                    });
                }
            }
        }
        if timeline::has_errors(&issues) {
            return Err(ValidationError(
                issues
                    .into_iter()
                    .filter(|i| i.level == timeline::Level::Error)
                    .collect(),
            )
            .into());
        }

        // 4. Commit the diff to the live doc under the actor's origin.
        self.undo_manager(actor);
        self.last_doc.insert(actor.key(), "timeline");
        let diff = scratch
            .transact()
            .encode_diff_v1(&self.doc.transact().state_vector());
        {
            let mut txn = self.doc.transact_mut_with(actor.key().as_str());
            txn.apply_update(Update::decode_v1(&diff)?)?;
        }
        self.check_store()?;

        self.log_op(&id, actor, "timeline", op, false)?;
        self.after_change()?;
        Ok(ApplyOutcome::Applied {
            op: id,
            result,
            warnings: issues.into_iter().map(|i| i.msg).collect(),
        })
    }

    pub fn undo(&mut self, actor: &Actor, redo: bool) -> Result<bool> {
        let first = *self.last_doc.get(&actor.key()).unwrap_or(&"timeline");
        let mut used = first;
        let mut did = {
            let um = if first == "board" {
                self.board_undo_manager(actor)
            } else {
                self.undo_manager(actor)
            };
            if redo {
                um.redo_blocking()
            } else {
                um.undo_blocking()
            }
        };
        if !did {
            used = if first == "board" {
                "timeline"
            } else {
                "board"
            };
            let um = if used == "board" {
                self.board_undo_manager(actor)
            } else {
                self.undo_manager(actor)
            };
            did = if redo {
                um.redo_blocking()
            } else {
                um.undo_blocking()
            };
        }
        if used == "board" {
            self.check_board_store()?;
        } else {
            self.check_store()?;
        }
        if did {
            self.log_op(
                &ops::gen_id("op"),
                actor,
                used,
                &Op::new(if redo { "redo" } else { "undo" }, json!({})),
                false,
            )?;
            self.last_doc.insert(actor.key(), used);
            if used == "board" {
                self.after_board_change()?;
            } else {
                self.after_change()?;
            }
        }
        Ok(did)
    }

    fn after_change(&mut self) -> Result<()> {
        self.last_op_at = SystemTime::now();
        self.export_snapshot()?;
        let frames = self.store.lock().unwrap().frames_since_compact;
        if frames >= 200 {
            self.compact()?;
        }
        Ok(())
    }

    pub fn compact(&mut self) -> Result<()> {
        self.store.lock().unwrap().compact(&self.doc)?;
        self.board_store.lock().unwrap().compact(&self.board_doc)
    }

    /// Write `timeline.json` from the live doc (spec §14.2: export-only).
    pub fn export_snapshot(&mut self) -> Result<Option<String>> {
        let tl = self.timeline()?;
        let txt = serde_json::to_string_pretty(&tl)? + "\n";
        let path = self.root.join("timeline.json");
        let on_disk = fs::read_to_string(&path).ok();
        let mut warning = None;
        if let (Some(disk), Some(last)) = (&on_disk, &self.last_export) {
            if disk != last && disk != &txt {
                warning = Some(format!(
                    "{} was edited on disk; overwritten from the live doc (edit via ops instead)",
                    path.display()
                ));
                eprintln!("kurn-core: warning: {}", warning.as_ref().unwrap());
            }
        }
        if on_disk.as_deref() != Some(txt.as_str()) {
            let tmp = path.with_extension("json.tmp");
            fs::write(&tmp, &txt)?;
            fs::rename(&tmp, &path)?;
        }
        self.last_export = Some(txt);
        Ok(warning)
    }

    /// Write `board.excalidraw` from the live board doc (export-only, §14.2).
    pub fn export_board_snapshot(&mut self) -> Result<Option<String>> {
        let snap = crate::board::snapshot(&self.board_doc);
        let txt = serde_json::to_string_pretty(&snap)? + "\n";
        let path = self.root.join("board.excalidraw");
        let on_disk = fs::read_to_string(&path).ok();
        let mut warning = None;
        if let (Some(disk), Some(last)) = (&on_disk, &self.last_board_export) {
            if disk != last && disk != &txt {
                warning = Some(format!(
                    "{} was edited on disk; overwritten from the live board (edit with board ops instead)",
                    path.display()
                ));
                eprintln!("kurn-core: warning: {}", warning.as_ref().unwrap());
            }
        }
        if on_disk.as_deref() != Some(txt.as_str()) {
            let tmp = path.with_extension("excalidraw.tmp");
            fs::write(&tmp, &txt)?;
            fs::rename(&tmp, &path)?;
        }
        self.last_board_export = Some(txt);
        Ok(warning)
    }

    fn log_op(&self, id: &str, actor: &Actor, doc: &str, op: &Op, derived: bool) -> Result<()> {
        let rec = OpRecord {
            id: id.into(),
            actor: actor.clone(),
            ts: now_rfc3339(),
            doc: doc.into(),
            kind: op.kind.clone(),
            args: op.args.clone(),
            derived,
        };
        let path = ws_dir(&self.root).join("oplog.jsonl");
        let mut f = OpenOptions::new().create(true).append(true).open(path)?;
        writeln!(f, "{}", serde_json::to_string(&rec)?)?;
        fan_out(&self.subscribers, Push::Op(rec));
        Ok(())
    }

    /// Full current states (v1 updates) plus a receiver for everything after.
    /// Taken under the core lock, so nothing can slip between them.
    pub fn subscribe(&self) -> (Vec<u8>, Vec<u8>, std::sync::mpsc::Receiver<Push>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let state = self
            .doc
            .transact()
            .encode_state_as_update_v1(&StateVector::default());
        let board_state = self
            .board_doc
            .transact()
            .encode_state_as_update_v1(&StateVector::default());
        self.subscribers.lock().unwrap().push(tx);
        (state, board_state, rx)
    }

    // ----- presence (spec §7.4) -----

    fn live_presence(&mut self) -> Vec<Presence> {
        let now = now_secs();
        self.presence
            .retain(|_, p| now.saturating_sub(p.updated_at) <= PRESENCE_TTL_S);
        let mut out: Vec<Presence> = self.presence.values().cloned().collect();
        out.sort_by(|a, b| a.actor.key().cmp(&b.actor.key()));
        out
    }

    pub fn presence(&mut self) -> Vec<Presence> {
        self.live_presence()
    }

    pub fn set_presence(
        &mut self,
        actor: &Actor,
        playhead: Option<i64>,
        selection: Vec<String>,
    ) -> Vec<Presence> {
        self.presence.insert(
            actor.key(),
            Presence {
                actor: actor.clone(),
                playhead,
                selection,
                updated_at: now_secs(),
            },
        );
        let all = self.live_presence();
        fan_out(&self.subscribers, Push::Presence(all.clone()));
        all
    }

    pub fn clear_presence(&mut self, actor: &Actor) -> Vec<Presence> {
        self.presence.remove(&actor.key());
        let all = self.live_presence();
        fan_out(&self.subscribers, Push::Presence(all.clone()));
        all
    }

    pub fn read_log(&self, actor: Option<&str>, limit: usize) -> Result<Vec<OpRecord>> {
        let path = ws_dir(&self.root).join("oplog.jsonl");
        let txt = fs::read_to_string(path).unwrap_or_default();
        let mut recs: Vec<OpRecord> = txt
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .filter(|r: &OpRecord| actor.is_none_or(|a| r.actor.name == a || r.actor.key() == a))
            .collect();
        let n = recs.len();
        Ok(recs.split_off(n.saturating_sub(limit)))
    }

    // ----- budget (spec §14.5) -----

    fn read_budget(root: &Path) -> BudgetState {
        fs::read_to_string(ws_dir(root).join("budget.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    fn write_budget(&self) -> Result<()> {
        fs::write(
            ws_dir(&self.root).join("budget.json"),
            serde_json::to_string_pretty(&self.budget)?,
        )?;
        Ok(())
    }

    pub fn budget(&self) -> Value {
        let cfg = &self.config.agents;
        let limit = cfg.api_budget_usd;
        let spent = self.budget.spent_usd;
        let pct = limit
            .filter(|l| *l > 0.0)
            .map(|l| spent / l * 100.0)
            .unwrap_or(0.0);
        let warning = limit.is_some_and(|l| l > 0.0 && pct >= cfg.api_budget_warn_pct);
        let over = limit.is_some_and(|l| spent >= l);
        json!({
            "spent_usd": spent,
            "limit_usd": limit,
            "warn_pct": cfg.api_budget_warn_pct,
            "used_pct": pct,
            "warning": warning,
            "over": over,
        })
    }

    /// Refuse an over-budget API-module run. `hint` comes from the module manifest.
    pub fn check_budget(&self, hint: Option<&str>, estimate_usd: f64) -> Option<String> {
        if hint != Some("api") {
            return None;
        }
        let limit = self.config.agents.api_budget_usd?;
        if limit > 0.0 && self.budget.spent_usd + estimate_usd.max(0.0) >= limit {
            return Some(format!(
                "API budget reached (${:.2} of ${:.2}); raise [agents].api_budget_usd or set KURN_BUDGET_OVERRIDE=1",
                self.budget.spent_usd, limit
            ));
        }
        None
    }

    pub fn record_cost(&mut self, usd: f64) -> Result<Value> {
        if usd > 0.0 {
            self.budget.spent_usd += usd;
            self.budget.updated_at = now_rfc3339();
            self.write_budget()?;
        }
        Ok(self.budget())
    }

    // ----- pending ops (spec §6.3) -----

    fn read_pending(root: &Path) -> BTreeMap<String, PendingOp> {
        fs::read_to_string(ws_dir(root).join("pending.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    fn write_pending(&self) -> Result<()> {
        fs::write(
            ws_dir(&self.root).join("pending.json"),
            serde_json::to_string_pretty(&self.pending)?,
        )?;
        Ok(())
    }

    fn expire(&mut self) {
        let now = now_secs();
        for p in self.pending.values_mut() {
            if p.state == "pending" && now > p.expires_at {
                p.state = "expired".into();
            }
        }
    }

    pub fn pending(&mut self) -> Vec<PendingOp> {
        self.expire();
        self.pending.values().cloned().collect()
    }

    pub fn pending_op(&mut self, id: &str) -> Option<PendingOp> {
        self.expire();
        self.pending.get(id).cloned()
    }

    /// Queue an over-budget module run for human approval (spec §14.5).
    pub fn request_pending_run(
        &mut self,
        actor: &Actor,
        op: Op,
        reason: String,
    ) -> Result<PendingOp> {
        let id = ops::gen_id("op");
        let now = now_secs();
        let p = PendingOp {
            id: id.clone(),
            actor: actor.clone(),
            op,
            created: now,
            expires_at: now + self.config.agents.pending_expiry_s,
            reason,
            state: "pending".into(),
            detail: None,
        };
        self.pending.insert(id, p.clone());
        self.write_pending()?;
        let all = self.pending();
        fan_out(&self.subscribers, Push::Pending(all));
        Ok(p)
    }

    pub fn set_pending_state(
        &mut self,
        id: &str,
        state: &str,
        detail: Option<Value>,
    ) -> Result<Option<PendingOp>> {
        let Some(entry) = self.pending.get_mut(id) else {
            return Ok(None);
        };
        entry.state = state.into();
        entry.detail = detail;
        let out = entry.clone();
        self.write_pending()?;
        let all = self.pending();
        fan_out(&self.subscribers, Push::Pending(all));
        Ok(Some(out))
    }

    /// Human decision on a pending op. Re-validates against the current doc.
    pub fn decide(
        &mut self,
        id: &str,
        approve: bool,
        by: &Actor,
        asset_exists: &dyn Fn(&str) -> bool,
    ) -> Result<PendingOp> {
        if by.is_agent() {
            bail!("only a human can approve or reject pending ops");
        }
        self.expire();
        let p = self
            .pending
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow!("no pending op {id}"))?;
        if p.state != "pending" {
            bail!("op {id} is already {}", p.state);
        }
        let (state, detail) = if !approve {
            ("rejected".to_string(), Some(json!({ "by": by.key() })))
        } else {
            match self.apply(&p.actor, &p.op, asset_exists, true) {
                Ok(out) => ("applied".into(), Some(serde_json::to_value(out)?)),
                Err(e) => ("stale".into(), Some(json!({ "error": e.to_string() }))),
            }
        };
        let entry = self.pending.get_mut(id).unwrap();
        entry.state = state;
        entry.detail = detail;
        let out = entry.clone();
        self.write_pending()?;
        let all = self.pending();
        fan_out(&self.subscribers, Push::Pending(all));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent() -> Actor {
        Actor::parse("agent:claude").unwrap()
    }
    fn human() -> Actor {
        Actor::parse("human:night").unwrap()
    }
    fn yes(_: &str) -> bool {
        true
    }

    fn setup() -> (tempfile::TempDir, Project) {
        let dir = tempfile::tempdir().unwrap();
        Project::create(dir.path(), "t", "30/1", 1920, 1080).unwrap();
        let p = Project::open(dir.path()).unwrap();
        (dir, p)
    }

    fn op(kind: &str, args: Value) -> Op {
        Op::new(kind, args)
    }

    #[test]
    fn apply_validate_undo_persist() {
        let (dir, mut p) = setup();
        p.apply(
            &agent(),
            &op("track.add", json!({"id":"v1","kind":"video"})),
            &yes,
            false,
        )
        .unwrap();
        p.apply(
            &agent(),
            &op(
                "clip.add",
                json!({"id":"a","track":"v1","asset":"x","start":0,"dur":100}),
            ),
            &yes,
            false,
        )
        .unwrap();
        // Overlap is rejected, doc unchanged.
        let e = p
            .apply(
                &agent(),
                &op(
                    "clip.add",
                    json!({"track":"v1","asset":"x","start":50,"dur":10}),
                ),
                &yes,
                false,
            )
            .unwrap_err();
        assert!(e.to_string().contains("overlap"), "{e}");
        assert_eq!(p.timeline().unwrap().clips.len(), 1);
        assert_eq!(p.timeline().unwrap().duration, 100, "duration grows to fit");

        // Human edit, then agent undo only reverts the agent's last op.
        p.apply(
            &human(),
            &op("marker.add", json!({"t":5,"label":"h"})),
            &yes,
            false,
        )
        .unwrap();
        assert!(p.undo(&agent(), false).unwrap());
        let tl = p.timeline().unwrap();
        assert!(tl.clips.is_empty(), "agent's clip undone");
        assert_eq!(tl.markers.len(), 1, "human's marker kept");
        assert!(p.undo(&agent(), true).unwrap());
        assert_eq!(p.timeline().unwrap().clips.len(), 1, "redo");

        // Reopen from disk (no compaction) → same state.
        let before = p.timeline().unwrap();
        drop(p);
        let p2 = Project::open(dir.path()).unwrap();
        assert_eq!(p2.timeline().unwrap(), before);
        let snap: Timeline =
            serde_json::from_str(&fs::read_to_string(dir.path().join("timeline.json")).unwrap())
                .unwrap();
        assert_eq!(snap, before);
    }

    #[test]
    fn batch_is_atomic_and_one_undo_step() {
        let (_d, mut p) = setup();
        let b = op(
            "batch",
            json!({"ops":[
                {"type":"track.add","args":{"id":"v1","kind":"video"}},
                {"type":"clip.add","args":{"id":"a","track":"v1","asset":"x","start":0,"dur":30}},
                {"type":"clip.add","args":{"id":"b","track":"v1","asset":"x","start":30,"dur":30}}
            ]}),
        );
        p.apply(&agent(), &b, &yes, false).unwrap();
        assert_eq!(p.timeline().unwrap().clips.len(), 2);
        // A failing batch leaves nothing behind.
        let bad = op(
            "batch",
            json!({"ops":[
                {"type":"clip.add","args":{"id":"c","track":"v1","asset":"x","start":60,"dur":30}},
                {"type":"clip.add","args":{"track":"v1","asset":"x","start":70,"dur":5}}
            ]}),
        );
        assert!(p.apply(&agent(), &bad, &yes, false).is_err());
        assert_eq!(p.timeline().unwrap().clips.len(), 2);
        p.undo(&agent(), false).unwrap();
        let tl = p.timeline().unwrap();
        assert!(
            tl.clips.is_empty() && tl.tracks.is_empty(),
            "whole batch undone in one step"
        );
    }

    #[test]
    fn destructive_agent_op_waits_for_human() {
        let (_d, mut p) = setup();
        p.apply(
            &agent(),
            &op("track.add", json!({"id":"v1","kind":"video"})),
            &yes,
            false,
        )
        .unwrap();
        let out = p
            .apply(
                &agent(),
                &op("track.remove", json!({"track":"v1"})),
                &yes,
                false,
            )
            .unwrap();
        let ApplyOutcome::Pending { op: id, .. } = out else {
            panic!("expected pending")
        };
        assert_eq!(p.timeline().unwrap().tracks.len(), 1);
        assert!(
            p.decide(&id, true, &agent(), &yes).is_err(),
            "agents can't approve"
        );
        let d = p.decide(&id, true, &human(), &yes).unwrap();
        assert_eq!(d.state, "applied");
        assert!(p.timeline().unwrap().tracks.is_empty());
    }

    #[test]
    fn presence_snapshot_and_clear() {
        let (_d, mut p) = setup();
        assert!(p.presence().is_empty());
        let list = p.set_presence(&agent(), Some(42), vec!["c1".into(), "c2".into()]);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].playhead, Some(42));
        assert_eq!(list[0].selection, vec!["c1", "c2"]);
        assert!(p.presence().iter().any(|x| x.actor.name == "claude"));
        assert!(p.clear_presence(&agent()).is_empty());
    }

    #[test]
    fn board_ops_persist_and_export() {
        let (dir, mut p) = setup();
        let out = p
            .apply_board(
                &human(),
                &op(
                    "batch",
                    json!({ "ops": [
                        { "type": "frame.add", "args": { "name": "Scene 1", "x": 0, "y": 0 } },
                        { "type": "el.add", "args": { "type": "rectangle", "x": 10, "y": 10, "label": "Hook" } }
                    ] }),
                ),
            )
            .unwrap();
        assert!(matches!(out, ApplyOutcome::Applied { .. }));
        let els = p.board_elements().unwrap();
        assert_eq!(els.len(), 3, "frame + rectangle + label text");
        assert!(els.iter().any(|e| e["type"] == json!("frame")));
        let snap: Value =
            serde_json::from_str(&fs::read_to_string(dir.path().join("board.excalidraw")).unwrap())
                .unwrap();
        assert_eq!(snap["type"], "excalidraw");
        assert_eq!(snap["elements"].as_array().unwrap().len(), 3);

        drop(p);
        let p2 = Project::open(dir.path()).unwrap();
        assert_eq!(
            p2.board_elements().unwrap().len(),
            3,
            "board survives reopen"
        );
    }

    #[test]
    fn budget_warns_blocks_and_persists() {
        let (dir, mut p) = setup();
        p.config.agents.api_budget_usd = Some(0.01);
        assert!(p.check_budget(Some("api"), 0.005).is_none());
        assert!(p.check_budget(Some("api"), 0.02).is_some());
        assert!(p.check_budget(Some("local"), 100.0).is_none());
        p.record_cost(0.008).unwrap();
        let b = p.budget();
        assert!(b["warning"] == json!(true), "{b}");
        assert!(b["over"] == json!(false), "{b}");
        p.record_cost(0.002).unwrap();
        assert_eq!(p.budget()["over"], json!(true));
        drop(p);
        let p2 = Project::open(dir.path()).unwrap();
        assert!((p2.budget()["spent_usd"].as_f64().unwrap() - 0.01).abs() < 1e-9);
    }

    #[test]
    fn board_undo_is_per_actor_and_redoable() {
        let (_d, mut p) = setup();
        p.apply_board(
            &human(),
            &op(
                "el.add",
                json!({ "type": "rectangle", "x": 0, "y": 0, "label": "A" }),
            ),
        )
        .unwrap();
        assert_eq!(p.board_elements().unwrap().len(), 2);
        assert!(
            !p.undo(&agent(), false).unwrap(),
            "agent has no board history"
        );
        assert!(p.undo(&human(), false).unwrap());
        assert!(p.board_elements().unwrap().is_empty());
        assert!(p.undo(&human(), true).unwrap());
        assert_eq!(p.board_elements().unwrap().len(), 2);
    }

    #[test]
    fn pending_and_presence_are_pushed() {
        let (_d, mut p) = setup();
        p.apply(&human(), &op("batch", json!({"ops":[
            {"type":"track.add","args":{"id":"v1","kind":"video"}},
            {"type":"clip.add","args":{"id":"a","track":"v1","asset":"x","start":0,"dur":20}}
        ]})), &yes, false).unwrap();
        let (_state, _board_state, rx) = p.subscribe();
        let out = p
            .apply(
                &agent(),
                &op("ripple.delete", json!({"clip":"a"})),
                &yes,
                false,
            )
            .unwrap();
        assert!(matches!(out, ApplyOutcome::Pending { .. }));
        assert!(
            matches!(rx.try_recv(), Ok(Push::Pending(v)) if v.len() == 1),
            "pending prompt is pushed live"
        );
        p.set_presence(&agent(), Some(7), vec![]);
        assert!(
            matches!(rx.try_recv(), Ok(Push::Presence(v)) if v.len() == 1),
            "presence is pushed live"
        );
    }

    #[test]
    fn split_and_ripple() {
        let (_d, mut p) = setup();
        p.apply(&human(), &op("batch", json!({"ops":[
            {"type":"track.add","args":{"id":"v1","kind":"video"}},
            {"type":"clip.add","args":{"id":"a","track":"v1","asset":"x","start":0,"in":10,"dur":100,
               "keyframes":{"opacity":[{"t":0,"v":0},{"t":60,"v":1}]}}},
            {"type":"clip.add","args":{"id":"b","track":"v1","asset":"x","start":100,"dur":50}}
        ]})), &yes, false).unwrap();
        let out = p
            .apply(
                &human(),
                &op("clip.split", json!({"clip":"a","at":40})),
                &yes,
                false,
            )
            .unwrap();
        let ApplyOutcome::Applied { result, .. } = out else {
            panic!()
        };
        let right = result["right"].as_str().unwrap().to_string();
        let tl = p.timeline().unwrap();
        assert_eq!((tl.clips["a"].in_, tl.clips["a"].out), (10, 50));
        assert_eq!(
            (
                tl.clips[&right].start,
                tl.clips[&right].in_,
                tl.clips[&right].out
            ),
            (40, 50, 110)
        );
        assert_eq!(
            tl.clips[&right].keyframes["opacity"],
            vec![json!({"t":20,"v":1})]
        );
        p.apply(
            &human(),
            &op("ripple.delete", json!({"clip": right})),
            &yes,
            false,
        )
        .unwrap();
        assert_eq!(p.timeline().unwrap().clips["b"].start, 40);
    }
}
