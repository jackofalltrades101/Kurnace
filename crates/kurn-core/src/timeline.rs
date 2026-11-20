//! Timeline model (spec §7) — the materialized, serde form of the timeline doc.
//! Mirrors `packages/timeline` (TypeScript); keep the validators in sync.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rational {
    pub num: i64,
    pub den: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub order: f64,
    #[serde(default, skip_serializing_if = "is_false")]
    pub muted: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub locked: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    pub track: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<String>,
    /// HTML scene in the project's `scenes/<name>/` (docs/v2-plan.md §3.1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<String>,
    /// Scene parameters (posted to the scene; also exposed as CSS variables).
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub params: Map<String, Value>,
    pub start: i64,
    #[serde(rename = "in")]
    pub in_: i64,
    pub out: i64,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub props: Map<String, Value>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub keyframes: BTreeMap<String, Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset_samples: Option<i64>,
}

impl Clip {
    pub fn len(&self) -> i64 {
        self.out - self.in_
    }
    pub fn end(&self) -> i64 {
        self.start + self.len()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Marker {
    pub t: i64,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transition {
    pub between: [String; 2],
    #[serde(rename = "type")]
    pub kind: String,
    pub dur: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Timeline {
    pub version: u32,
    pub fps: Rational,
    pub width: i64,
    pub height: i64,
    pub duration: i64,
    pub tracks: Vec<Track>,
    pub clips: BTreeMap<String, Clip>,
    #[serde(default)]
    pub markers: BTreeMap<String, Marker>,
    #[serde(default)]
    pub transitions: BTreeMap<String, Transition>,
}

impl Timeline {
    pub fn empty(fps: Rational, width: i64, height: i64) -> Self {
        Timeline {
            version: 2,
            fps,
            width,
            height,
            duration: 0,
            tracks: vec![],
            clips: BTreeMap::new(),
            markers: BTreeMap::new(),
            transitions: BTreeMap::new(),
        }
    }

    pub fn track(&self, id: &str) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }

    /// End of the last clip, in frames.
    pub fn content_end(&self) -> i64 {
        self.clips.values().map(Clip::end).max().unwrap_or(0)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Issue {
    pub level: Level,
    pub msg: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Error,
    Warn,
}

/// Structural validation (spec §6.3). `known_assets = None` skips asset checks.
pub fn validate(tl: &Timeline, known_assets: Option<&HashSet<String>>) -> Vec<Issue> {
    let mut out = vec![];
    let mut err = |m: String| {
        out.push(Issue {
            level: Level::Error,
            msg: m,
        })
    };
    if tl.version != 2 {
        err(format!("timeline.version must be 2 (got {})", tl.version));
    }
    if tl.fps.num <= 0 || tl.fps.den <= 0 {
        err("fps must be a positive rational {num, den}".into());
    }
    if tl.duration < 0 {
        err("duration must be >= 0".into());
    }
    if tl.width <= 0 || tl.height <= 0 {
        err("width/height must be positive".into());
    }
    let mut ids = HashSet::new();
    for t in &tl.tracks {
        if !ids.insert(t.id.as_str()) {
            err(format!("duplicate track id {}", t.id));
        }
    }
    let mut by_track: BTreeMap<&str, Vec<(&String, &Clip)>> = BTreeMap::new();
    for (id, c) in &tl.clips {
        if c.in_ < 0 {
            err(format!("clip {id}: in < 0"));
        }
        if c.out <= c.in_ {
            err(format!("clip {id}: out must be > in"));
        }
        if c.start < 0 {
            err(format!("clip {id}: start < 0"));
        }
        if !ids.contains(c.track.as_str()) {
            err(format!("clip {id}: unknown track {}", c.track));
        }
        let sources = [c.asset.is_some(), c.component.is_some(), c.scene.is_some()].iter().filter(|x| **x).count();
        if sources > 1 {
            err(format!("clip {id}: has more than one of asset/component/scene"));
        }
        if sources == 0 {
            err(format!("clip {id}: needs an asset, component or scene"));
        }
        if let (Some(a), Some(known)) = (&c.asset, known_assets) {
            if !known.contains(a) {
                err(format!("clip {id}: unknown asset {a}"));
            }
        }
        by_track.entry(c.track.as_str()).or_default().push((id, c));
    }
    for (track, mut clips) in by_track {
        clips.sort_by_key(|(_, c)| c.start);
        for w in clips.windows(2) {
            let ((pid, p), (cid, c)) = (w[0], w[1]);
            if c.start < p.end() {
                err(format!("track {track}: clips {pid} and {cid} overlap"));
            }
        }
    }
    for (id, tr) in &tl.transitions {
        for c in &tr.between {
            if !tl.clips.contains_key(c) {
                err(format!("transition {id}: unknown clip {c}"));
            }
        }
    }
    if tl.content_end() > tl.duration {
        out.push(Issue {
            level: Level::Warn,
            msg: format!(
                "clips extend past duration ({} > {})",
                tl.content_end(),
                tl.duration
            ),
        });
    }
    out
}

pub fn has_errors(issues: &[Issue]) -> bool {
    issues.iter().any(|i| i.level == Level::Error)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Timeline {
        let mut t = Timeline::empty(Rational { num: 30, den: 1 }, 1920, 1080);
        t.duration = 300;
        t.tracks.push(Track {
            id: "v1".into(),
            kind: "video".into(),
            name: None,
            order: 0.0,
            muted: false,
            locked: false,
        });
        t
    }

    fn clip(start: i64, len: i64) -> Clip {
        Clip {
            track: "v1".into(),
            asset: Some("x".into()),
            component: None,
            scene: None,
            params: Map::new(),
            start,
            in_: 0,
            out: len,
            props: Map::new(),
            keyframes: BTreeMap::new(),
            offset_samples: None,
        }
    }

    #[test]
    fn overlap_and_refs() {
        let mut t = base();
        t.clips.insert("a".into(), clip(0, 100));
        assert!(validate(&t, None).is_empty());
        t.clips.insert("b".into(), clip(50, 10));
        let msgs: Vec<_> = validate(&t, None).into_iter().map(|i| i.msg).collect();
        assert!(
            msgs.iter().any(|m| m.contains("a and b overlap")),
            "{msgs:?}"
        );
    }

    #[test]
    fn json_roundtrip_matches_ts_shape() {
        let src = r#"{"version":2,"fps":{"num":30,"den":1},"width":1920,"height":1080,"duration":10,
          "tracks":[{"id":"v1","kind":"video","order":0}],
          "clips":{"c":{"track":"v1","asset":"x","start":0,"in":0,"out":10,"props":{"opacity":1}}}}"#;
        let t: Timeline = serde_json::from_str(src).unwrap();
        assert_eq!(t.clips["c"].in_, 0);
        let back = serde_json::to_value(&t).unwrap();
        assert_eq!(back["clips"]["c"]["in"], 0);
    }
}
