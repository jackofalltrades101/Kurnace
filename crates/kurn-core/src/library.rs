//! Asset library (spec §8): content-addressed blobs + SQLite index.
//! Only the core opens `index.sqlite` (WAL). Assets are records (`ast_<ULID>`)
//! pointing at blobs; directories are stored as tree manifests.
use anyhow::{Context, Result, anyhow, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS blobs (
  hash TEXT PRIMARY KEY,
  is_tree INT NOT NULL DEFAULT 0,
  size INT NOT NULL,
  created_at TEXT
);
CREATE TABLE IF NOT EXISTS assets (
  id TEXT PRIMARY KEY,
  blob_hash TEXT NOT NULL REFERENCES blobs(hash),
  kind TEXT NOT NULL,
  name TEXT,
  ext TEXT,
  mime TEXT,
  width INT, height INT,
  duration_frames INT,
  fps_num INT, fps_den INT,
  sample_rate INT,
  duration_s REAL,
  source TEXT NOT NULL,
  recipe_id TEXT,
  parent_id TEXT,
  license TEXT,
  created_by TEXT, created_at TEXT,
  deprecated INT DEFAULT 0
);
CREATE INDEX IF NOT EXISTS assets_blob ON assets(blob_hash);
CREATE TABLE IF NOT EXISTS tags (asset_id TEXT, tag TEXT, PRIMARY KEY (asset_id, tag));
CREATE TABLE IF NOT EXISTS usage (asset_id TEXT, project TEXT, ref TEXT);
CREATE TABLE IF NOT EXISTS recipes (
  id TEXT PRIMARY KEY,
  recipe_hash TEXT NOT NULL,
  module TEXT, module_version TEXT,
  params_json TEXT, inputs_json TEXT,
  seed INT
);
CREATE INDEX IF NOT EXISTS recipes_hash ON recipes(recipe_hash);
CREATE TABLE IF NOT EXISTS embeddings (
  asset_id TEXT PRIMARY KEY,
  model TEXT NOT NULL,
  dim INT NOT NULL,
  vector BLOB NOT NULL
);
"#;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Asset {
    pub id: String,
    pub blob_hash: String,
    pub kind: String,
    pub name: Option<String>,
    pub ext: Option<String>,
    pub mime: Option<String>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub fps_num: Option<i64>,
    pub fps_den: Option<i64>,
    pub sample_rate: Option<i64>,
    pub duration_s: Option<f64>,
    pub source: String,
    pub recipe_id: Option<String>,
    pub parent_id: Option<String>,
    pub license: Option<String>,
    pub created_by: Option<String>,
    pub created_at: Option<String>,
    pub deprecated: bool,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Absolute path to the blob on disk (file, or tree dir materialization).
    #[serde(default)]
    pub path: Option<String>,
    /// Frame-sequence files, sorted, for `seq` assets.
    #[serde(default)]
    pub frames: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recipe {
    pub module: String,
    pub module_version: String,
    pub params: Value,
    pub inputs: Vec<String>,
    pub seed: Option<i64>,
}

impl Recipe {
    pub fn hash(&self) -> String {
        let canon = serde_json::to_string(&json!({
            "module": self.module, "version": self.module_version,
            "params": canonical(&self.params), "inputs": self.inputs, "seed": self.seed,
        }))
        .unwrap();
        hex::encode(Sha256::digest(canon.as_bytes()))
    }
}

/// Sort object keys recursively so equal params hash equally.
fn canonical(v: &Value) -> Value {
    match v {
        Value::Object(o) => {
            let mut keys: Vec<_> = o.keys().collect();
            keys.sort();
            Value::Object(
                keys.into_iter()
                    .map(|k| (k.clone(), canonical(&o[k])))
                    .collect(),
            )
        }
        Value::Array(a) => Value::Array(a.iter().map(canonical).collect()),
        _ => v.clone(),
    }
}

fn image_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = fs::read_dir(dir) else {
        return vec![];
    };
    let mut frames: Vec<PathBuf> = rd
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
    frames.sort();
    frames
}

pub struct Library {
    pub root: PathBuf,
    conn: Connection,
}

// ---------- local similarity search (spec §8.2 v2) ----------
//
// Preferred provider: a local Ollama embedding model. If Ollama or the model
// is unavailable we fall back to deterministic feature-hashed word/trigram
// vectors, so `lib search --semantic` still works offline and with no model.

const EMBED_DIM: usize = 384;

fn hash_embed(text: &str) -> Vec<f32> {
    let mut v = vec![0.0f32; EMBED_DIM];
    let low = text.to_lowercase();
    let mut add = |s: &str| {
        let mut h: u64 = 0xcbf29ce484222325;
        for b in s.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        v[(h as usize) % EMBED_DIM] += 1.0;
    };
    for word in low
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        add(&format!("#{word}"));
        let chars: Vec<char> = word.chars().collect();
        if chars.len() <= 3 {
            add(&format!("@{word}"));
        } else {
            for i in 0..=chars.len() - 3 {
                add(&format!("@{}", chars[i..i + 3].iter().collect::<String>()));
            }
        }
    }
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut v {
            *x /= norm;
        }
    }
    v
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn http_json(path: &str, body: &str) -> Option<Value> {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 11434));
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_millis(600)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(30))).ok()?;
    write!(
        s,
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .ok()?;
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).ok()?;
    let txt = String::from_utf8_lossy(&buf);
    let body = txt.split_once("\r\n\r\n")?.1;
    serde_json::from_str(body).ok()
}

fn ollama_embed(model: &str, text: &str) -> Option<Vec<f32>> {
    let req = json!({ "model": model, "prompt": text }).to_string();
    if let Some(v) = http_json("/api/embeddings", &req) {
        if let Some(a) = v["embedding"].as_array() {
            return Some(
                a.iter()
                    .filter_map(Value::as_f64)
                    .map(|x| x as f32)
                    .collect(),
            );
        }
    }
    // Newer Ollama API
    let req = json!({ "model": model, "input": text }).to_string();
    let v = http_json("/api/embed", &req)?;
    v["embeddings"][0].as_array().map(|a| {
        a.iter()
            .filter_map(Value::as_f64)
            .map(|x| x as f32)
            .collect()
    })
}

fn embed_model() -> Option<String> {
    match std::env::var("KURN_EMBED_MODEL") {
        Ok(v) if v.eq_ignore_ascii_case("off") => None,
        Ok(v) if !v.trim().is_empty() => Some(v),
        _ => Some("nomic-embed-text".into()),
    }
}

const PEAK_BUCKETS: usize = 1024;

/// Waveform amplitudes from mono s16le PCM, max-abs per bucket in 0..1.
/// Kept pure so the bucket maths is testable without ffmpeg.
pub fn pcm_peaks(bytes: &[u8], buckets: usize) -> Vec<f64> {
    if buckets == 0 || bytes.len() < 2 {
        return vec![];
    }
    let samples = bytes.len() / 2;
    let mut peaks = vec![0.0f64; buckets];
    for i in 0..samples {
        let v =
            i16::from_le_bytes([bytes[i * 2], bytes[i * 2 + 1]]).unsigned_abs() as f64 / 32768.0;
        let b = (buckets * i / samples).min(buckets - 1);
        if v > peaks[b] {
            peaks[b] = v;
        }
    }
    peaks
}

/// One horizontal strip of 10 evenly spaced frames, used as a clip thumbnail.
fn make_video_strip(src: &Path, out: &Path, duration_s: f64) -> Result<()> {
    if let Some(dir) = out.parent() {
        fs::create_dir_all(dir)?;
    }
    let dur = duration_s.max(0.1);
    let fps = (10.0 / dur).clamp(0.01, 30.0);
    let vf = format!("fps={fps:.6},scale=160:-2:flags=bilinear,tile=10x1");
    let res = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i"])
        .arg(src)
        .args(["-vf", &vf, "-frames:v", "1", "-q:v", "3"])
        .arg(out)
        .output()
        .with_context(|| "running ffmpeg for thumbnail strip")?;
    if !res.status.success() || !out.exists() {
        bail!(
            "ffmpeg thumbnail failed: {}",
            String::from_utf8_lossy(&res.stderr).trim()
        );
    }
    Ok(())
}

pub fn kind_for_ext(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "png" | "jpg" | "jpeg" | "webp" | "gif" | "svg" | "avif" => "image",
        "mp4" | "mov" | "webm" | "mkv" | "m4v" => "video",
        "wav" | "mp3" | "flac" | "ogg" | "m4a" | "aac" | "opus" => "audio",
        "ttf" | "otf" | "woff" | "woff2" => "font",
        "glb" | "gltf" | "obj" | "fbx" => "model3d",
        _ => return None,
    })
}

fn mime_for_ext(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "wav" => "audio/wav",
        "mp3" => "audio/mpeg",
        "flac" => "audio/flac",
        "ogg" | "opus" => "audio/ogg",
        "m4a" | "aac" => "audio/aac",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "woff2" => "font/woff2",
        _ => return None,
    })
}

fn sha256_file(p: &Path) -> Result<(String, u64)> {
    let mut f = fs::File::open(p).with_context(|| format!("open {}", p.display()))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    let mut n = 0u64;
    loop {
        let r = f.read(&mut buf)?;
        if r == 0 {
            break;
        }
        h.update(&buf[..r]);
        n += r as u64;
    }
    Ok((hex::encode(h.finalize()), n))
}

#[derive(Debug, Default)]
struct Probe {
    width: Option<i64>,
    height: Option<i64>,
    fps: Option<(i64, i64)>,
    sample_rate: Option<i64>,
    duration: Option<f64>,
    has_video: bool,
    has_audio: bool,
}

/// Tree assets can carry `kurn.json` metadata written by render modules
/// (`fps_num`, `fps_den`, `frames`, `width`, `height`). This is how a PNG
/// sequence records its native rate (spec §12).
fn probe_tree(dir: &Path) -> Probe {
    let mut pr = Probe::default();
    let Ok(txt) = fs::read_to_string(dir.join("kurn.json")) else {
        return pr;
    };
    let Ok(v) = serde_json::from_str::<Value>(&txt) else {
        return pr;
    };
    pr.width = v["width"].as_i64();
    pr.height = v["height"].as_i64();
    let n = v["fps_num"].as_i64().unwrap_or(0);
    let d = v["fps_den"].as_i64().unwrap_or(0);
    if n > 0 && d > 0 {
        pr.fps = Some((n, d));
    }
    let frames = v["frames"].as_i64();
    pr.duration = v["duration_s"].as_f64().or_else(|| {
        frames
            .zip(pr.fps)
            .map(|(f, (n, d))| f as f64 * d as f64 / n as f64)
    });
    pr.has_video = frames.is_some() || v["engine"].is_string();
    pr
}

fn probe(p: &Path) -> Probe {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_type,width,height,r_frame_rate,sample_rate:format=duration",
            "-of",
            "json",
        ])
        .arg(p)
        .output();
    let Ok(out) = out else {
        return Probe::default();
    };
    let j: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    let mut pr = Probe {
        duration: j["format"]["duration"]
            .as_str()
            .and_then(|s| s.parse().ok()),
        ..Default::default()
    };
    for s in j["streams"].as_array().cloned().unwrap_or_default() {
        match s["codec_type"].as_str() {
            Some("video") if !pr.has_video => {
                pr.has_video = true;
                pr.width = s["width"].as_i64();
                pr.height = s["height"].as_i64();
                pr.fps = s["r_frame_rate"].as_str().and_then(|r| {
                    let (a, b) = r.split_once('/')?;
                    Some((a.parse().ok()?, b.parse().ok()?))
                        .filter(|&(n, d): &(i64, i64)| n > 0 && d > 0)
                });
            }
            Some("audio") if !pr.has_audio => {
                pr.has_audio = true;
                pr.sample_rate = s["sample_rate"].as_str().and_then(|x| x.parse().ok());
            }
            _ => {}
        }
    }
    pr
}

pub struct AddOptions<'a> {
    pub tags: &'a [String],
    pub name: Option<&'a str>,
    pub license: Option<&'a str>,
    pub created_by: &'a str,
    pub recipe: Option<&'a Recipe>,
    pub parent_id: Option<&'a str>,
}

impl Library {
    pub fn open(root: &Path) -> Result<Library> {
        for d in ["blobs", "recipes", "peaks", "thumbs"] {
            fs::create_dir_all(root.join(d))?;
        }
        let conn = Connection::open(root.join("index.sqlite"))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "busy_timeout", 5000)?;
        conn.execute_batch(SCHEMA)?;
        Ok(Library {
            root: root.to_path_buf(),
            conn,
        })
    }

    /// Spec §5: warn if the library sits in a network or cloud-synced folder.
    pub fn location_warning(&self) -> Option<String> {
        let p = self.root.to_string_lossy();
        let bad = [
            "Library/Mobile Documents",
            "Library/CloudStorage",
            "Dropbox",
            "OneDrive",
            "Google Drive",
            "/Volumes/",
        ];
        bad.iter().find(|b| p.contains(*b)).map(|b| {
            format!("library at {p} looks like a synced or network location ({b}); SQLite locking is unsafe there — move it to a local disk")
        })
    }

    pub fn blob_path(&self, hash: &str) -> PathBuf {
        self.root.join("blobs").join(&hash[..2]).join(hash)
    }

    fn thumb_path(&self, hash: &str) -> PathBuf {
        self.root.join("thumbs").join(format!("{hash}.jpg"))
    }

    fn peaks_path(&self, hash: &str) -> PathBuf {
        self.root.join("peaks").join(format!("{hash}.json"))
    }

    /// Still image or a 10-frame strip for time-based media. Cached by content hash.
    pub fn thumb(&self, id: &str) -> Result<(PathBuf, String)> {
        let a = self.get(id)?.ok_or_else(|| anyhow!("unknown asset {id}"))?;
        let src = PathBuf::from(
            a.path
                .clone()
                .ok_or_else(|| anyhow!("asset {id} has no file path"))?,
        );
        match a.kind.as_str() {
            "image" => {
                let mime = a
                    .ext
                    .as_deref()
                    .and_then(mime_for_ext)
                    .unwrap_or("image/png");
                Ok((src, mime.into()))
            }
            "video" => {
                let out = self.thumb_path(&a.blob_hash);
                if !out.exists() || fs::metadata(&out).map(|m| m.len() == 0).unwrap_or(true) {
                    let _ = fs::remove_file(&out);
                    make_video_strip(&src, &out, a.duration_s.unwrap_or(1.0))?;
                }
                Ok((out, "image/jpeg".into()))
            }
            "seq" => {
                // First frame, for thumbnails.
                let first = image_files(&src)
                    .into_iter()
                    .next()
                    .ok_or_else(|| anyhow!("sequence {id} has no image frames"))?;
                let mime = first
                    .extension()
                    .and_then(|e| e.to_str())
                    .and_then(mime_for_ext)
                    .unwrap_or("image/png");
                Ok((first, mime.into()))
            }
            other => bail!("no thumbnail for {other} asset {id}"),
        }
    }

    /// Waveform peaks (0..1, `PEAK_BUCKETS` buckets) for an audio or video asset.
    /// Cached by content hash; generated lazily.
    pub fn peaks(&self, id: &str) -> Result<Value> {
        let a = self.get(id)?.ok_or_else(|| anyhow!("unknown asset {id}"))?;
        if a.kind != "audio" && a.kind != "video" {
            bail!("no waveform for {} asset {id}", a.kind);
        }
        let cache = self.peaks_path(&a.blob_hash);
        if let Ok(txt) = fs::read_to_string(&cache) {
            if let Ok(v) = serde_json::from_str(&txt) {
                return Ok(v);
            }
        }
        let src = PathBuf::from(
            a.path
                .clone()
                .ok_or_else(|| anyhow!("asset {id} has no file path"))?,
        );
        let out = Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(&src)
            .args(["-vn", "-ac", "1", "-ar", "8000", "-f", "s16le", "-"])
            .output()
            .with_context(|| "running ffmpeg for waveform peaks")?;
        if !out.status.success() {
            bail!(
                "ffmpeg peaks failed for {id}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        let peaks = pcm_peaks(&out.stdout, PEAK_BUCKETS);
        let v = json!({ "sample_rate": 8000, "buckets": PEAK_BUCKETS, "peaks": peaks });
        fs::write(&cache, serde_json::to_string(&v)?)?;
        Ok(v)
    }

    /// Generate thumbnails + peaks at ingest time (spec §7.4). Best-effort:
    /// ffmpeg may be absent, and the UI also generates them lazily.
    fn warm_derived(&self, a: &Asset) {
        if a.kind == "video" || a.kind == "image" || a.kind == "seq" {
            let _ = self.thumb(&a.id);
        }
        if a.kind == "video" || a.kind == "audio" {
            let _ = self.peaks(&a.id);
        }
    }

    fn store_file(&self, src: &Path) -> Result<(String, u64)> {
        let (hash, size) = sha256_file(src)?;
        let dst = self.blob_path(&hash);
        if !dst.exists() {
            fs::create_dir_all(dst.parent().unwrap())?;
            let tmp = dst.with_extension("tmp");
            fs::copy(src, &tmp)?;
            fs::rename(&tmp, &dst)?;
        }
        Ok((hash, size))
    }

    /// Store a directory as a tree manifest blob (spec §8.1). Returns (hash, total size).
    fn store_tree(&self, dir: &Path) -> Result<(String, u64)> {
        let mut files = vec![];
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in fs::read_dir(&d)? {
                let e = e?;
                let p = e.path();
                let name = e.file_name();
                if name.to_string_lossy().starts_with('.') {
                    continue;
                }
                if e.file_type()?.is_dir() {
                    stack.push(p);
                } else {
                    files.push(p);
                }
            }
        }
        let mut entries = vec![];
        let mut total = 0;
        for f in &files {
            let (h, n) = self.store_file(f)?;
            let rel = f.strip_prefix(dir)?.to_string_lossy().replace('\\', "/");
            entries.push(json!({ "path": rel, "sha256": h, "size": n }));
            total += n;
        }
        entries.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
        let manifest = serde_json::to_vec(&json!({ "kurnace_tree": 1, "files": entries }))?;
        let hash = hex::encode(Sha256::digest(&manifest));
        let dst = self.blob_path(&hash);
        if !dst.exists() {
            fs::create_dir_all(dst.parent().unwrap())?;
            fs::write(&dst, &manifest)?;
        }
        Ok((hash, total))
    }

    /// Materialize a tree blob as a directory of hardlinks/copies under `trees/<hash>/`.
    pub fn tree_dir(&self, hash: &str) -> Result<PathBuf> {
        let dir = self.root.join("trees").join(hash);
        if dir.exists() {
            return Ok(dir);
        }
        let m: Value = serde_json::from_slice(&fs::read(self.blob_path(hash))?)?;
        let tmp = self.root.join("trees").join(format!("{hash}.tmp"));
        let _ = fs::remove_dir_all(&tmp);
        for f in m["files"]
            .as_array()
            .ok_or_else(|| anyhow!("not a tree blob"))?
        {
            let rel = f["path"].as_str().unwrap_or_default();
            let dst = tmp.join(rel);
            fs::create_dir_all(dst.parent().unwrap())?;
            let src = self.blob_path(f["sha256"].as_str().unwrap_or_default());
            if fs::hard_link(&src, &dst).is_err() {
                fs::copy(&src, &dst)?;
            }
        }
        fs::rename(&tmp, &dir)?;
        Ok(dir)
    }

    pub fn add_path(&mut self, src: &Path, opts: &AddOptions) -> Result<Asset> {
        let meta = fs::metadata(src).with_context(|| format!("{} not found", src.display()))?;
        let (hash, size, is_tree, kind, ext) = if meta.is_dir() {
            let (h, n) = self.store_tree(src)?;
            let kind = if src.join("project.godot").exists() {
                "godot"
            } else {
                "seq"
            };
            (h, n, true, kind.to_string(), None)
        } else {
            let ext = src
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            let kind = kind_for_ext(&ext)
                .ok_or_else(|| anyhow!("unsupported file type .{ext} ({})", src.display()))?;
            let (h, n) = self.store_file(src)?;
            (h, n, false, kind.to_string(), Some(ext))
        };
        let pr = if is_tree { probe_tree(src) } else { probe(src) };
        let now = crate::project::now_rfc3339();
        self.conn.execute(
            "INSERT OR IGNORE INTO blobs (hash, is_tree, size, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![hash, is_tree as i64, size as i64, now],
        )?;
        let recipe_id = match opts.recipe {
            Some(r) => Some(self.insert_recipe(r)?),
            None => None,
        };
        let id = format!("ast_{}", ulid::Ulid::generate());
        let name = opts
            .name
            .map(String::from)
            .or_else(|| src.file_name().map(|n| n.to_string_lossy().into_owned()));
        // Images report a single "frame"; video and frame-sequence assets carry
        // a native rational rate.
        let fps = if kind == "video" || kind == "seq" || kind == "godot" {
            pr.fps
        } else {
            None
        };
        self.conn.execute(
            "INSERT INTO assets (id, blob_hash, kind, name, ext, mime, width, height, fps_num, fps_den, sample_rate, duration_s,
                                 source, recipe_id, parent_id, license, created_by, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",
            params![
                id, hash, kind, name, ext, ext.as_deref().and_then(mime_for_ext),
                pr.width, pr.height, fps.map(|f| f.0), fps.map(|f| f.1), pr.sample_rate,
                if kind == "image" { None } else { pr.duration },
                if opts.recipe.is_some() { "generated" } else { "imported" },
                recipe_id, opts.parent_id, opts.license, opts.created_by, now
            ],
        )?;
        for t in opts.tags {
            self.tag(&id, t)?;
        }
        let a = self.get(&id)?.ok_or_else(|| anyhow!("asset vanished"))?;
        self.warm_derived(&a);
        Ok(a)
    }

    fn insert_recipe(&mut self, r: &Recipe) -> Result<String> {
        let id = format!("rcp_{}", ulid::Ulid::generate());
        self.conn.execute(
            "INSERT INTO recipes (id, recipe_hash, module, module_version, params_json, inputs_json, seed) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![id, r.hash(), r.module, r.module_version, r.params.to_string(), serde_json::to_string(&r.inputs)?, r.seed],
        )?;
        fs::write(
            self.root.join("recipes").join(format!("{id}.json")),
            serde_json::to_string_pretty(r)?,
        )?;
        Ok(id)
    }

    pub fn recipe(&self, id: &str) -> Result<Option<Recipe>> {
        self.conn
            .query_row("SELECT module, module_version, params_json, inputs_json, seed FROM recipes WHERE id=?1", [id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?, r.get::<_, Option<i64>>(4)?))
            })
            .optional()?
            .map(|(m, v, p, i, s)| Ok(Recipe { module: m, module_version: v, params: serde_json::from_str(&p)?, inputs: serde_json::from_str(&i)?, seed: s }))
            .transpose()
    }

    /// Cache lookup for deterministic modules (spec §8.3): latest non-deprecated asset per recipe hash.
    pub fn find_by_recipe(&self, r: &Recipe) -> Result<Vec<Asset>> {
        let mut st = self.conn.prepare(
            "SELECT a.id FROM assets a JOIN recipes r ON a.recipe_id = r.id WHERE r.recipe_hash = ?1 AND a.deprecated = 0 ORDER BY a.created_at",
        )?;
        let ids: Vec<String> = st
            .query_map([r.hash()], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        ids.iter().filter_map(|i| self.get(i).transpose()).collect()
    }

    pub fn tag(&mut self, id: &str, tag: &str) -> Result<()> {
        let t = tag.trim().to_lowercase();
        if !t.is_empty() {
            self.conn.execute(
                "INSERT OR IGNORE INTO tags (asset_id, tag) VALUES (?1, ?2)",
                params![id, t],
            )?;
        }
        Ok(())
    }

    pub fn untag(&mut self, id: &str, tag: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM tags WHERE asset_id=?1 AND tag=?2",
            params![id, tag.trim().to_lowercase()],
        )?;
        Ok(())
    }

    pub fn deprecate(&mut self, id: &str) -> Result<()> {
        let n = self
            .conn
            .execute("UPDATE assets SET deprecated=1 WHERE id=?1", [id])?;
        if n == 0 {
            bail!("unknown asset {id}");
        }
        Ok(())
    }

    pub fn exists(&self, id: &str) -> bool {
        self.conn
            .query_row("SELECT 1 FROM assets WHERE id=?1", [id], |_| Ok(()))
            .optional()
            .ok()
            .flatten()
            .is_some()
    }

    fn row_to_asset(&self, r: &rusqlite::Row) -> rusqlite::Result<Asset> {
        Ok(Asset {
            id: r.get("id")?,
            blob_hash: r.get("blob_hash")?,
            kind: r.get("kind")?,
            name: r.get("name")?,
            ext: r.get("ext")?,
            mime: r.get("mime")?,
            width: r.get("width")?,
            height: r.get("height")?,
            fps_num: r.get("fps_num")?,
            fps_den: r.get("fps_den")?,
            sample_rate: r.get("sample_rate")?,
            duration_s: r.get("duration_s")?,
            source: r.get("source")?,
            recipe_id: r.get("recipe_id")?,
            parent_id: r.get("parent_id")?,
            license: r.get("license")?,
            created_by: r.get("created_by")?,
            created_at: r.get("created_at")?,
            deprecated: r.get::<_, i64>("deprecated")? != 0,
            tags: vec![],
            path: None,
            frames: vec![],
        })
    }

    fn fill(&self, mut a: Asset) -> Result<Asset> {
        let mut st = self
            .conn
            .prepare("SELECT tag FROM tags WHERE asset_id=?1 ORDER BY tag")?;
        a.tags = st
            .query_map([&a.id], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        let is_tree = matches!(a.kind.as_str(), "seq" | "godot");
        let path = if is_tree {
            self.tree_dir(&a.blob_hash)?
        } else {
            self.blob_path(&a.blob_hash)
        };
        if a.kind == "seq" {
            a.frames = image_files(&path)
                .iter()
                .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                .collect();
        }
        a.path = Some(path.to_string_lossy().into_owned());
        Ok(a)
    }

    pub fn get(&self, id: &str) -> Result<Option<Asset>> {
        let a = self
            .conn
            .query_row("SELECT * FROM assets WHERE id=?1", [id], |r| {
                self.row_to_asset(r)
            })
            .optional()?;
        a.map(|a| self.fill(a)).transpose()
    }

    /// Simple search: every word must match name, kind, or a tag (spec §8.5; semantic search is v2).
    pub fn search(
        &self,
        query: &str,
        kind: Option<&str>,
        tag: Option<&str>,
        include_deprecated: bool,
        limit: usize,
    ) -> Result<Vec<Asset>> {
        let mut st = self
            .conn
            .prepare("SELECT * FROM assets ORDER BY created_at DESC")?;
        let all: Vec<Asset> = st
            .query_map([], |r| self.row_to_asset(r))?
            .collect::<Result<_, _>>()?;
        let words: Vec<String> = query.split_whitespace().map(|w| w.to_lowercase()).collect();
        let mut out = vec![];
        for a in all {
            if (a.deprecated && !include_deprecated) || kind.is_some_and(|k| k != a.kind) {
                continue;
            }
            let a = self.fill(a)?;
            if tag.is_some_and(|t| !a.tags.iter().any(|x| x == &t.to_lowercase())) {
                continue;
            }
            let hay = format!(
                "{} {} {}",
                a.name.clone().unwrap_or_default(),
                a.kind,
                a.tags.join(" ")
            )
            .to_lowercase();
            if words.iter().all(|w| hay.contains(w)) {
                out.push(a);
                if out.len() >= limit {
                    break;
                }
            }
        }
        Ok(out)
    }

    fn cached_embedding(&self, id: &str, model: &str) -> Option<Vec<f32>> {
        let (m, dim, blob): (String, i64, Vec<u8>) = self
            .conn
            .query_row(
                "SELECT model, dim, vector FROM embeddings WHERE asset_id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .ok()?;
        if m != model || dim as usize * 4 != blob.len() {
            return None;
        }
        Some(
            blob.chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect(),
        )
    }

    fn store_embedding(&self, id: &str, model: &str, v: &[f32]) {
        let mut blob = Vec::with_capacity(v.len() * 4);
        for x in v {
            blob.extend_from_slice(&x.to_le_bytes());
        }
        let _ = self.conn.execute(
            "INSERT INTO embeddings (asset_id, model, dim, vector) VALUES (?1,?2,?3,?4)
             ON CONFLICT(asset_id) DO UPDATE SET model=excluded.model, dim=excluded.dim, vector=excluded.vector",
            params![id, model, v.len() as i64, blob],
        );
    }

    /// Local similarity search. Uses an Ollama embedding model when available,
    /// otherwise deterministic hashed n-gram vectors.
    pub fn semantic_search(
        &self,
        query: &str,
        kind: Option<&str>,
        tag: Option<&str>,
        include_deprecated: bool,
        limit: usize,
    ) -> Result<Vec<Asset>> {
        if query.trim().is_empty() {
            return self.search(query, kind, tag, include_deprecated, limit);
        }
        let model = embed_model();
        let (model_name, query_vec) = match model.as_deref().and_then(|m| ollama_embed(m, query)) {
            Some(v) => (model.clone().unwrap_or_default(), v),
            None => ("hash-v1".to_string(), hash_embed(query)),
        };

        let mut st = self
            .conn
            .prepare("SELECT * FROM assets ORDER BY created_at DESC")?;
        let all: Vec<Asset> = st
            .query_map([], |r| self.row_to_asset(r))?
            .collect::<Result<_, _>>()?;
        let mut scored = vec![];
        for a in all {
            if (a.deprecated && !include_deprecated) || kind.is_some_and(|k| k != a.kind) {
                continue;
            }
            let a = self.fill(a)?;
            if tag.is_some_and(|t| !a.tags.iter().any(|x| x == &t.to_lowercase())) {
                continue;
            }
            let text = format!(
                "{} {} {}",
                a.name.clone().unwrap_or_default(),
                a.kind,
                a.tags.join(" ")
            );
            let v = self
                .cached_embedding(&a.id, &model_name)
                .unwrap_or_else(|| {
                    let v = if model_name == "hash-v1" {
                        hash_embed(&text)
                    } else {
                        ollama_embed(&model_name, &text).unwrap_or_else(|| hash_embed(&text))
                    };
                    self.store_embedding(&a.id, &model_name, &v);
                    v
                });
            let score = cosine(&query_vec, &v);
            if score > 0.08 {
                scored.push((score, a));
            }
        }
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        if let Some(best) = scored.first().map(|(s, _)| *s) {
            let cutoff = 0.08f32.max(best * 0.45);
            scored.retain(|(s, _)| *s >= cutoff);
        }
        Ok(scored.into_iter().take(limit).map(|(_, a)| a).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts<'a>(tags: &'a [String]) -> AddOptions<'a> {
        AddOptions {
            tags,
            name: None,
            license: None,
            created_by: "human:test",
            recipe: None,
            parent_id: None,
        }
    }

    #[test]
    fn add_dedupe_search_tree() {
        let lib_dir = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let mut lib = Library::open(lib_dir.path()).unwrap();
        let f = src.path().join("Dusk Gradient.png");
        fs::write(&f, b"not really a png").unwrap();
        let a = lib
            .add_path(&f, &opts(&["bg".into(), "Dusk".into()]))
            .unwrap();
        let b = lib.add_path(&f, &opts(&[])).unwrap();
        assert_ne!(a.id, b.id, "two asset records…");
        assert_eq!(a.blob_hash, b.blob_hash, "…one blob");
        assert_eq!(a.kind, "image");
        assert_eq!(a.tags, vec!["bg", "dusk"]);
        assert_eq!(
            lib.search("dusk gradient", Some("image"), None, false, 10)
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            lib.search("", None, Some("bg"), false, 10).unwrap().len(),
            1
        );
        lib.deprecate(&b.id).unwrap();
        assert_eq!(lib.search("dusk", None, None, false, 10).unwrap().len(), 1);

        let seq = src.path().join("shot");
        fs::create_dir_all(&seq).unwrap();
        for i in 0..3 {
            fs::write(seq.join(format!("{i:04}.png")), format!("frame {i}")).unwrap();
        }
        let s = lib.add_path(&seq, &opts(&[])).unwrap();
        assert_eq!(s.kind, "seq");
        let dir = PathBuf::from(s.path.unwrap());
        assert_eq!(fs::read_to_string(dir.join("0002.png")).unwrap(), "frame 2");
        // Same content → same tree hash.
        assert_eq!(
            lib.add_path(&seq, &opts(&[])).unwrap().blob_hash,
            s.blob_hash
        );
    }

    #[test]
    fn pcm_peaks_buckets_by_max_amplitude() {
        // Four samples → two buckets: [0, 32767] and [-32768, 1000].
        let mut pcm = vec![];
        for s in [0i16, 32767, -32768, 1000] {
            pcm.extend_from_slice(&s.to_le_bytes());
        }
        let p = pcm_peaks(&pcm, 2);
        assert_eq!(p.len(), 2);
        assert!((p[0] - 32767.0 / 32768.0).abs() < 1e-9);
        assert!((p[1] - 1.0).abs() < 1e-9);
        assert!(pcm_peaks(&[], 2).is_empty());
    }

    #[test]
    fn semantic_search_uses_local_similarity() {
        let lib_dir = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let mut lib = Library::open(lib_dir.path()).unwrap();
        for (name, tags) in [
            ("Dusk Gradient.png", vec!["bg".to_string()]),
            ("Bass Loop.wav", vec!["audio".to_string()]),
        ] {
            let f = src.path().join(name);
            fs::write(&f, name.as_bytes()).unwrap();
            lib.add_path(
                &f,
                &AddOptions {
                    tags: &tags,
                    ..opts(&[])
                },
            )
            .unwrap();
        }
        let hits = lib
            .semantic_search("dusky sunset", None, None, false, 5)
            .unwrap();
        assert!(
            hits.iter()
                .any(|a| a.name.as_deref() == Some("Dusk Gradient.png")),
            "{:?}",
            hits.iter().map(|a| &a.name).collect::<Vec<_>>()
        );
        assert_eq!(
            lib.semantic_search("dusk", Some("image"), None, false, 5)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn recipe_cache_lookup() {
        let lib_dir = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let mut lib = Library::open(lib_dir.path()).unwrap();
        let f = src.path().join("g.png");
        fs::write(&f, b"x").unwrap();
        let r = Recipe {
            module: "gen.gradient".into(),
            module_version: "1".into(),
            params: json!({"b":1,"a":2}),
            inputs: vec![],
            seed: Some(7),
        };
        let a = lib
            .add_path(
                &f,
                &AddOptions {
                    recipe: Some(&r),
                    ..opts(&[])
                },
            )
            .unwrap();
        let same = Recipe {
            params: json!({"a":2,"b":1}),
            ..r.clone()
        };
        assert_eq!(
            lib.find_by_recipe(&same).unwrap()[0].id,
            a.id,
            "key order doesn't matter"
        );
        assert!(
            lib.find_by_recipe(&Recipe { seed: Some(8), ..r })
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            lib.recipe(a.recipe_id.as_ref().unwrap())
                .unwrap()
                .unwrap()
                .module,
            "gen.gradient"
        );
    }
}
