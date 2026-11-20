//! Project scenes (docs/v2-plan.md §3.1): `scenes/<name>/` folders holding an
//! HTML page that follows the Kurnace scene contract (see
//! `packages/compositor/scene-runtime.js`).
use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize)]
pub struct SceneInfo {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub entry: String,
    /// Frames, when the manifest declares one.
    pub duration: Option<i64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    /// Parameter schema from scene.toml `[params]`.
    pub params: Map<String, Value>,
    pub description: Option<String>,
    pub dir: PathBuf,
}

pub fn scenes_dir(root: &Path) -> PathBuf {
    root.join("scenes")
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 64 && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') && !name.starts_with('-')
}

/// Read one scene folder. `None` when the folder isn't a scene (no scene.toml, no index.html).
pub fn read(root: &Path, name: &str) -> Result<Option<SceneInfo>> {
    let dir = scenes_dir(root).join(name);
    if !valid_name(name) || !dir.is_dir() {
        return Ok(None);
    }
    let manifest = dir.join("scene.toml");
    let t: toml::Table = if manifest.exists() {
        toml::from_str(&fs::read_to_string(&manifest)?).with_context(|| format!("parsing {}", manifest.display()))?
    } else if dir.join("index.html").exists() {
        toml::Table::new()
    } else {
        return Ok(None);
    };
    let entry = t.get("entry").and_then(|v| v.as_str()).unwrap_or("index.html").to_string();
    if !dir.join(&entry).exists() {
        bail!("scene {name}: entry {entry} doesn't exist");
    }
    let params = t
        .get("params")
        .map(|p| serde_json::to_value(p).unwrap_or(Value::Null))
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    Ok(Some(SceneInfo {
        name: name.into(),
        kind: t.get("type").and_then(|v| v.as_str()).unwrap_or("html").into(),
        entry,
        duration: t.get("duration").and_then(|v| v.as_integer()),
        width: t.get("width").and_then(|v| v.as_integer()),
        height: t.get("height").and_then(|v| v.as_integer()),
        params,
        description: t.get("description").and_then(|v| v.as_str()).map(String::from),
        dir,
    }))
}

pub fn list(root: &Path) -> Result<Vec<SceneInfo>> {
    let mut out = vec![];
    let Ok(rd) = fs::read_dir(scenes_dir(root)) else { return Ok(out) };
    for e in rd.flatten() {
        if let Some(name) = e.file_name().to_str() {
            // A broken manifest shouldn't hide every other scene.
            match read(root, name) {
                Ok(Some(s)) => out.push(s),
                Ok(None) => {}
                Err(err) => eprintln!("kurn-core: {err:#}"),
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

pub fn exists(root: &Path, name: &str) -> bool {
    matches!(read(root, name), Ok(Some(_)))
}

const HTML_TEMPLATE: &str = r#"<!doctype html>
<html>
<head>
<meta charset="utf-8">
<style>
  /* Scene params arrive as CSS variables (--title, --accent) and in kurnace.params. */
  html, body { margin: 0; width: 1920px; height: 1080px; overflow: hidden; background: #0e1116; }
  .title {
    position: absolute; inset: 0; display: grid; place-items: center;
    font: 800 140px/1 system-ui, sans-serif; color: var(--accent, #ffd866);
    animation: rise 1s cubic-bezier(.2, .8, .2, 1) both;
  }
  @keyframes rise { from { opacity: 0; transform: translateY(60px); } to { opacity: 1; transform: none; } }
</style>
</head>
<body>
  <div class="title" id="title">Title</div>
  <script>
    // CSS animations above are driven by the timeline automatically.
    // For JS, register a scene: render(frame, ctx) must depend only on (frame, ctx.params).
    kurnace.scene({
      render(frame, ctx) {
        document.getElementById('title').textContent = ctx.params.title ?? 'Title';
      },
    });
  </script>
</body>
</html>
"#;

const CANVAS_TEMPLATE: &str = r#"<!doctype html>
<html>
<head>
<meta charset="utf-8">
<style>html, body { margin: 0; background: #000; overflow: hidden; } canvas { display: block; }</style>
</head>
<body>
  <canvas id="c" width="1920" height="1080"></canvas>
  <script>
    const g = document.getElementById('c').getContext('2d');
    kurnace.scene({
      // Pure function of (frame, params): never keep state between frames.
      render(frame, ctx) {
        const t = ctx.time;
        g.fillStyle = '#0e1116';
        g.fillRect(0, 0, 1920, 1080);
        for (let i = 0; i < 40; i++) {
          const x = 960 + Math.cos(t * 1.3 + i * 0.6) * (200 + i * 14);
          const y = 540 + Math.sin(t * 0.9 + i * 0.6) * (120 + i * 8);
          g.fillStyle = `hsl(${(i * 9 + frame * 2) % 360} 80% 65%)`;
          g.beginPath(); g.arc(x, y, 10 + (i % 5) * 3, 0, Math.PI * 2); g.fill();
        }
      },
    });
  </script>
</body>
</html>
"#;

/// Scaffold `scenes/<name>/` from a template (`html` or `canvas`).
pub fn create(root: &Path, name: &str, template: &str, width: i64, height: i64) -> Result<SceneInfo> {
    if !valid_name(name) {
        bail!("scene names use letters, digits, - and _ (got {name:?})");
    }
    let dir = scenes_dir(root).join(name);
    if dir.exists() {
        bail!("scene {name} already exists");
    }
    let (html, params) = match template {
        "html" => (HTML_TEMPLATE, "title = { type = \"string\", default = \"Title\" }\naccent = { type = \"color\", default = \"#ffd866\" }\n"),
        "canvas" => (CANVAS_TEMPLATE, ""),
        other => bail!("unknown scene template {other:?} (html, canvas)"),
    };
    fs::create_dir_all(&dir)?;
    let html = html.replace("1920px", &format!("{width}px")).replace("1080px", &format!("{height}px"))
        .replace("width=\"1920\" height=\"1080\"", &format!("width=\"{width}\" height=\"{height}\""));
    fs::write(dir.join("index.html"), html)?;
    fs::write(
        dir.join("scene.toml"),
        format!("type = \"html\"\nentry = \"index.html\"\ndescription = \"\"\n\n[params]\n{params}"),
    )?;
    read(root, name)?.context("scene vanished")
}

/// JSON for `scenes.list` and the app.
pub fn to_json(s: &SceneInfo) -> Value {
    json!({
        "name": s.name, "type": s.kind, "entry": s.entry, "duration": s.duration,
        "width": s.width, "height": s.height, "params": s.params, "description": s.description,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_list_read() {
        let d = tempfile::tempdir().unwrap();
        create(d.path(), "intro", "html", 1280, 720).unwrap();
        create(d.path(), "dots", "canvas", 1280, 720).unwrap();
        assert!(create(d.path(), "intro", "html", 1280, 720).is_err(), "no overwrite");
        assert!(create(d.path(), "../x", "html", 1280, 720).is_err(), "no traversal");
        // A bare folder with index.html is a scene too (no manifest needed).
        fs::create_dir_all(d.path().join("scenes/bare")).unwrap();
        fs::write(d.path().join("scenes/bare/index.html"), "<html></html>").unwrap();
        fs::create_dir_all(d.path().join("scenes/notascene")).unwrap();
        let names: Vec<_> = list(d.path()).unwrap().into_iter().map(|s| s.name).collect();
        assert_eq!(names, vec!["bare", "dots", "intro"]);
        let intro = read(d.path(), "intro").unwrap().unwrap();
        assert_eq!(intro.params["title"]["default"], "Title");
        assert!(fs::read_to_string(intro.dir.join("index.html")).unwrap().contains("1280px"));
    }
}
