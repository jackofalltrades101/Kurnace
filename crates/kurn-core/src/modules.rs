//! Module system (spec §9): manifests, parameter parsing, and the
//! stdin-JSON / stdout-NDJSON protocol.
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputSpec {
    #[serde(rename = "type")]
    pub kind: String, // string | number | integer | bool | json | asset
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub default: Option<toml::Value>,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    #[serde(default, rename = "kind")]
    pub asset_kind: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputSpec {
    #[serde(rename = "type")]
    pub kind: String, // asset | file
    #[serde(default, rename = "kind")]
    pub asset_kind: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentSpec {
    #[serde(default)]
    pub expose: bool,
    #[serde(default)]
    pub cost_hint: Option<String>,
    #[serde(default)]
    pub cost_estimate_usd: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Requires {
    #[serde(default)]
    pub env: Vec<String>,
    #[serde(default)]
    pub bins: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub id: String,
    pub version: String,
    pub kind: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    pub runtime: String,
    pub entry: String,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub deterministic: bool,
    #[serde(default)]
    pub requires: Requires,
    #[serde(default)]
    pub inputs: BTreeMap<String, InputSpec>,
    #[serde(default)]
    pub outputs: BTreeMap<String, OutputSpec>,
    #[serde(default)]
    pub agent: AgentSpec,
    #[serde(skip_deserializing)]
    pub dir: PathBuf,
}

/// Module search path: $KURNACE_MODULES, <home>/modules, project/modules, and
/// the repo's `modules/` found by walking up from the executable (dev builds).
pub fn module_dirs(home: &Path, project: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var("KURNACE_MODULES")
        .ok()
        .map(|v| std::env::split_paths(&v).collect())
        .unwrap_or_default();
    if let Some(p) = project {
        dirs.push(p.join("modules"));
    }
    dirs.push(home.join("modules"));
    if let Ok(exe) = std::env::current_exe() {
        let mut d = exe.parent().map(Path::to_path_buf);
        while let Some(dir) = d {
            let cand = dir.join("modules");
            if cand.join("render.revideo").join("module.toml").exists() {
                dirs.push(cand);
                break;
            }
            d = dir.parent().map(Path::to_path_buf);
        }
    }
    dirs
}

pub fn list(dirs: &[PathBuf]) -> Result<Vec<Manifest>> {
    let mut out: BTreeMap<String, Manifest> = BTreeMap::new();
    for base in dirs {
        let Ok(rd) = fs::read_dir(base) else { continue };
        for e in rd.flatten() {
            let mf = e.path().join("module.toml");
            if !mf.exists() {
                continue;
            }
            let mut m: Manifest = toml::from_str(&fs::read_to_string(&mf)?)
                .with_context(|| format!("parsing {}", mf.display()))?;
            m.dir = e.path();
            // Earlier dirs win (env > project > home > repo).
            out.entry(m.id.clone()).or_insert(m);
        }
    }
    Ok(out.into_values().collect())
}

pub fn find(dirs: &[PathBuf], id: &str) -> Result<Manifest> {
    list(dirs)?
        .into_iter()
        .find(|m| m.id == id)
        .ok_or_else(|| anyhow!("module not found: {id} (try `kurn modules`)"))
}

fn toml_to_json(v: &toml::Value) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

/// Convert a raw CLI string to the input's type.
pub fn coerce(name: &str, spec: &InputSpec, raw: &Value) -> Result<Value> {
    let v = match (spec.kind.as_str(), raw) {
        ("string" | "asset", Value::String(_)) => raw.clone(),
        ("number", Value::String(s)) => json!(
            s.parse::<f64>()
                .map_err(|_| anyhow!("--{name}: expected a number, got {s:?}"))?
        ),
        ("integer", Value::String(s)) => json!(
            s.parse::<i64>()
                .map_err(|_| anyhow!("--{name}: expected an integer, got {s:?}"))?
        ),
        ("bool", Value::String(s)) => json!(match s.as_str() {
            "true" | "1" | "yes" => true,
            "false" | "0" | "no" => false,
            _ => bail!("--{name}: expected true/false, got {s:?}"),
        }),
        ("json", Value::String(s)) => {
            serde_json::from_str(s).map_err(|e| anyhow!("--{name}: invalid JSON: {e}"))?
        }
        (_, v) => v.clone(),
    };
    if let (Some(n), true) = (v.as_f64(), spec.kind == "number" || spec.kind == "integer") {
        if spec.min.is_some_and(|m| n < m) || spec.max.is_some_and(|m| n > m) {
            bail!(
                "--{name}: {n} is outside [{}, {}]",
                spec.min.map_or("-∞".into(), |m| m.to_string()),
                spec.max.map_or("∞".into(), |m| m.to_string())
            );
        }
    }
    Ok(v)
}

/// Build the full param set: given values (typed or raw strings) + defaults; checks required/unknown.
pub fn resolve_params(m: &Manifest, given: &Map<String, Value>) -> Result<Map<String, Value>> {
    for k in given.keys() {
        if !m.inputs.contains_key(k) {
            bail!(
                "{}: unknown input --{k} (inputs: {})",
                m.id,
                m.inputs.keys().cloned().collect::<Vec<_>>().join(", ")
            );
        }
    }
    let mut out = Map::new();
    for (k, spec) in &m.inputs {
        match given.get(k) {
            Some(v) => {
                out.insert(k.clone(), coerce(k, spec, v)?);
            }
            None => match &spec.default {
                Some(d) => {
                    out.insert(k.clone(), toml_to_json(d));
                }
                None if spec.required => bail!("{}: missing required input --{k}", m.id),
                None => {}
            },
        }
    }
    Ok(out)
}

/// `--set` overrides for variants (spec §8.3). `k=v` always sets an absolute
/// value (so `target_lufs=-6` means -6); numeric inputs also take `k+=n`,
/// `k-=n`, `k*=n`, relative to the parent recipe, clamped to min/max.
pub fn apply_set(
    m: &Manifest,
    base: &Map<String, Value>,
    sets: &[String],
) -> Result<(Map<String, Value>, Vec<String>)> {
    let mut out = base.clone();
    let mut notes = vec![];
    for s in sets {
        let (lhs, raw) = s.split_once('=').ok_or_else(|| {
            anyhow!("--set {s:?}: expected key=value or key+=n / key-=n / key*=n")
        })?;
        let (k, rel) = match lhs.chars().last() {
            Some(c @ ('+' | '-' | '*')) => (&lhs[..lhs.len() - 1], Some(c)),
            _ => (lhs, None),
        };
        let spec = m
            .inputs
            .get(k)
            .ok_or_else(|| anyhow!("--set: {} has no input `{k}`", m.id))?;
        let v = match rel {
            None => coerce(k, spec, &json!(raw))?,
            Some(op) => {
                if spec.kind != "number" && spec.kind != "integer" {
                    bail!("--set {s:?}: `{op}=` only works on numeric inputs");
                }
                let cur = base
                    .get(k)
                    .and_then(Value::as_f64)
                    .or_else(|| {
                        spec.default
                            .as_ref()
                            .and_then(|d| d.as_float().or(d.as_integer().map(|i| i as f64)))
                    })
                    .ok_or_else(|| {
                        anyhow!(
                            "--set {s:?}: parent has no `{k}` to change relatively; use {k}=<value>"
                        )
                    })?;
                let n: f64 = raw
                    .parse()
                    .map_err(|_| anyhow!("--set {s:?}: bad number"))?;
                let mut x = match op {
                    '+' => cur + n,
                    '-' => cur - n,
                    _ => cur * n,
                };
                if let Some(lo) = spec.min.filter(|lo| x < *lo) {
                    notes.push(format!("{k} clamped to min {lo}"));
                    x = lo;
                }
                if let Some(hi) = spec.max.filter(|hi| x > *hi) {
                    notes.push(format!("{k} clamped to max {hi}"));
                    x = hi;
                }
                if spec.kind == "integer" {
                    json!(x.round() as i64)
                } else {
                    json!(x)
                }
            }
        };
        out.insert(k.into(), v);
    }
    Ok((out, notes))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Event {
    Progress {
        #[serde(default)]
        pct: Option<f64>,
        #[serde(default)]
        msg: Option<String>,
    },
    Log {
        level: String,
        msg: String,
    },
    Output {
        name: String,
        path: String,
    },
    Cost {
        usd: f64,
    },
    Done,
    Error {
        msg: String,
    },
}

#[derive(Debug, Default, Serialize)]
pub struct RunOutput {
    pub outputs: Vec<(String, PathBuf)>,
    pub cost_usd: f64,
}

/// Best-effort macOS filesystem sandbox (spec §9.2). Opt-in with
/// `KURN_SANDBOX=1`: reads stay open (modules need blobs), writes are limited
/// to the job workdir, temp, module cache/build dirs and language caches.
/// This is not a security boundary for untrusted modules yet (Q5), but it
/// catches accidental writes into the project/library.
fn maybe_sandbox(cmd: Command, request: &Value, m: &Manifest) -> Command {
    if !cfg!(target_os = "macos")
        || std::env::var_os("KURN_SANDBOX").is_none()
        || !Path::new("/usr/bin/sandbox-exec").exists()
    {
        return cmd;
    }
    let work = request["_ctx"]["work_dir"].as_str().unwrap_or("/tmp");
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let profile = format!(
        "(version 1)\n(allow default)\n(deny file-write*)\n\
         (allow file-write*\n\
           (subpath \"{work}\")\n\
           (subpath \"{module}\")\n\
           (subpath \"{home}/Library/Caches\")\n\
           (subpath \"{home}/Library/Application Support/Godot\")\n\
           (subpath \"{home}/.cache\")\n\
           (subpath \"{home}/.npm\")\n\
           (subpath \"{home}/.nuget\")\n\
           (subpath \"{home}/.dotnet\")\n\
           (subpath \"/private/tmp\")\n\
           (subpath \"/private/var/folders\")\n\
           (literal \"/dev/null\")\n\
           (literal \"/dev/stdout\")\n\
           (literal \"/dev/stderr\"))\n",
        work = work,
        module = m.dir.display(),
        home = home,
    );
    let prog = cmd.get_program().to_os_string();
    let args: Vec<std::ffi::OsString> = cmd.get_args().map(|a| a.to_os_string()).collect();
    let mut s = Command::new("/usr/bin/sandbox-exec");
    s.arg("-p")
        .arg(profile)
        .arg("/bin/sh")
        .arg("-c")
        .arg("exec \"$@\"")
        .arg("kurn-module")
        .arg(prog)
        .args(args);
    // Keep the module dir writable for renderer caches (Vite etc.).
    s
}

fn command_for(m: &Manifest) -> Result<Command> {
    let entry = m.dir.join(&m.entry);
    Ok(match m.runtime.as_str() {
        "node" => {
            let node = std::env::var("KURN_NODE").unwrap_or_else(|_| "node".into());
            let mut c = Command::new(node);
            c.arg("--disable-warning=ExperimentalWarning").arg(&entry);
            c
        }
        "python" => {
            let py = std::env::var("KURN_PYTHON").unwrap_or_else(|_| "python3".into());
            let mut c = Command::new(py);
            c.arg("-I").arg(&entry);
            c
        }
        "bin" => Command::new(&entry),
        other => bail!("runtime `{other}` not supported yet"),
    })
}

pub fn check_requirements(m: &Manifest, extra_env: &BTreeMap<String, String>) -> Result<()> {
    for e in &m.requires.env {
        if std::env::var_os(e).is_none() && !extra_env.contains_key(e) {
            bail!("{} needs environment variable {e}", m.id);
        }
    }
    for b in &m.requires.bins {
        let found = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).any(|d| d.join(b).is_file()))
            .unwrap_or(false);
        if !found {
            bail!("{} needs `{b}` on PATH", m.id);
        }
    }
    Ok(())
}

/// Run a module to completion, streaming events to `on_event`.
/// `extra_env` comes from the calling CLI (API keys, KURN_* settings), so a module
/// sees the caller's environment rather than whatever the daemon started with.
pub fn run(
    m: &Manifest,
    request: &Value,
    cwd: Option<&Path>,
    extra_env: &BTreeMap<String, String>,
    mut on_event: impl FnMut(&Event),
) -> Result<RunOutput> {
    check_requirements(m, extra_env)?;
    let mut cmd = maybe_sandbox(command_for(m)?, request, m);
    cmd.current_dir(cwd.unwrap_or(&m.dir))
        .envs(extra_env)
        .env("DISABLE_TELEMETRY", "true")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawning {} ({})", m.id, m.runtime))?;
    let mut stdin = child.stdin.take().unwrap();
    let req = serde_json::to_vec(request)?;
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&req);
    });
    let stderr = child.stderr.take().unwrap();
    let err_tail = std::thread::spawn(move || {
        let mut tail = std::collections::VecDeque::new();
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if std::env::var_os("KURN_MODULE_STDERR").is_some() {
                eprintln!("{line}");
            }
            tail.push_back(line);
            if tail.len() > 40 {
                tail.pop_front();
            }
        }
        tail.into_iter().collect::<Vec<_>>().join("\n")
    });

    let mut out = RunOutput::default();
    let mut failure = None;
    let mut done = false;
    for line in BufReader::new(child.stdout.take().unwrap()).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let ev: Event = match serde_json::from_str(&line) {
            Ok(e) => e,
            Err(_) => Event::Log {
                level: "stdout".into(),
                msg: line,
            },
        };
        match &ev {
            Event::Output { name, path } => out.outputs.push((name.clone(), PathBuf::from(path))),
            Event::Cost { usd } => out.cost_usd += usd,
            Event::Error { msg } => failure = Some(msg.clone()),
            Event::Done => done = true,
            _ => {}
        }
        on_event(&ev);
    }
    let status = child.wait()?;
    let _ = writer.join();
    let tail = err_tail.join().unwrap_or_default();
    if let Some(f) = failure {
        bail!("{}: {f}", m.id);
    }
    if !status.success() || !done {
        bail!(
            "{} exited with {status} without \"done\"\n--- module stderr (tail) ---\n{tail}",
            m.id
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> Manifest {
        toml::from_str(
            r#"
            id = "gen.test"
            version = "1.0.0"
            kind = "generator"
            runtime = "python"
            entry = "run.py"
            [inputs]
            hue   = { type = "number", default = 10, min = 0, max = 360 }
            label = { type = "string", required = true }
            [outputs]
            image = { type = "asset", kind = "image" }
        "#,
        )
        .unwrap()
    }

    #[test]
    fn params_and_set_syntax() {
        let m = manifest();
        let mut g = Map::new();
        assert!(
            resolve_params(&m, &g)
                .unwrap_err()
                .to_string()
                .contains("missing required input --label")
        );
        g.insert("label".into(), json!("x"));
        let p = resolve_params(&m, &g).unwrap();
        assert_eq!(p["hue"], json!(10));
        let (v, notes) = apply_set(&m, &p, &["hue+=20".into(), "label=+literal".into()]).unwrap();
        assert_eq!(v["hue"], json!(30.0));
        assert_eq!(v["label"], json!("+literal"));
        assert!(notes.is_empty());
        let (v, _) = apply_set(&m, &p, &["hue=5".into()]).unwrap();
        assert_eq!(v["hue"], json!(5.0), "k=v is absolute");
        let (v, notes) = apply_set(&m, &p, &["hue*=100".into()]).unwrap();
        assert_eq!(v["hue"], json!(360.0));
        assert_eq!(notes, vec!["hue clamped to max 360"]);
        assert!(apply_set(&m, &p, &["nope=1".into()]).is_err());
        g.insert("hue".into(), json!("400"));
        assert!(resolve_params(&m, &g).is_err());
    }

    #[test]
    fn runs_protocol() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("run.py"),
            r#"
import json, sys, os
req = json.load(sys.stdin)
print(json.dumps({"type":"progress","pct":50}), flush=True)
p = os.path.join(req["_ctx"]["work_dir"], "out.txt")
open(p, "w").write(req["label"])
print(json.dumps({"type":"cost","usd":0.01}))
print(json.dumps({"type":"output","name":"image","path":p}))
print(json.dumps({"type":"done"}))
"#,
        )
        .unwrap();
        let mut m = manifest();
        m.dir = dir.path().to_path_buf();
        let mut evs = vec![];
        let out = run(
            &m,
            &json!({"label": "hi", "_ctx": {"work_dir": dir.path()}}),
            None,
            &BTreeMap::new(),
            |e| evs.push(format!("{e:?}")),
        )
        .unwrap();
        assert_eq!(out.outputs.len(), 1);
        assert_eq!(fs::read_to_string(&out.outputs[0].1).unwrap(), "hi");
        assert!((out.cost_usd - 0.01).abs() < 1e-9);
        assert_eq!(evs.len(), 4);
    }
}
