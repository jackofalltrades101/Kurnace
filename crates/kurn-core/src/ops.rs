//! Operation layer (spec §6). Ops are the intent API: each compiles to
//! mutations inside one Yjs transaction. Validation of the *result* happens in
//! `project.rs` (apply on a scratch doc → materialize → validate → commit).
use crate::timeline::Timeline;
use crate::ydoc::{self, child_map, ensure_map, set};
use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use yrs::{Array, ArrayRef, Map, MapRef, Out, TransactionMut};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Op {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub args: Value,
}

impl Op {
    pub fn new(kind: &str, args: Value) -> Self {
        Op {
            kind: kind.into(),
            args,
        }
    }
}

/// Short random id with a type prefix. Uses the random (not time) part of a ULID.
pub fn gen_id(prefix: &str) -> String {
    let u = ulid::Ulid::generate().to_string().to_lowercase();
    format!("{prefix}_{}", &u[18..])
}

fn s<'a>(a: &'a Value, k: &str) -> Result<&'a str> {
    a.get(k)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing string arg `{k}`"))
}
fn i(a: &Value, k: &str) -> Result<i64> {
    let v = a
        .get(k)
        .ok_or_else(|| anyhow!("missing integer arg `{k}`"))?;
    as_frame(v, k)
}
fn oi(a: &Value, k: &str) -> Result<Option<i64>> {
    match a.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => as_frame(v, k).map(Some),
    }
}
fn as_frame(v: &Value, k: &str) -> Result<i64> {
    if let Some(n) = v.as_i64() {
        return Ok(n);
    }
    match v.as_f64() {
        Some(f) if f.fract() == 0.0 => Ok(f as i64),
        Some(_) => bail!("`{k}` must be an integer frame (no fractional time, spec §7.2)"),
        None => bail!("`{k}` must be an integer"),
    }
}

/// The ops this layer understands, for `kurn op --help` and AGENTS.md.
pub const CATALOG: &[(&str, &str)] = &[
    (
        "timeline.set",
        "{duration?, width?, height?, fps?: {num, den}}",
    ),
    ("track.add", "{kind, id?, name?, order?} → {track}"),
    ("track.remove", "{track}  (removes its clips; destructive)"),
    ("track.reorder", "{track, order}"),
    ("track.set", "{track, name?, muted?, locked?}"),
    (
        "clip.add",
        "{track, asset? | scene? (+ params?) | component?, start, in?=0, out? | dur?, props?, keyframes?, id?} → {clip}",
    ),
    ("clip.remove", "{clip}"),
    ("clip.move", "{clip, start? | by?, track?}"),
    ("clip.trim", "{clip, in?, out?, start?}"),
    ("clip.split", "{clip, at}  (timeline frame) → {left, right}"),
    (
        "clip.set",
        "{clip, props?: {k: v|null}, params?: {k: v|null}, asset?, scene?, offset_samples?}",
    ),
    (
        "keyframe.set",
        "{clip, prop, t, v, ease?}  (t is clip-local)",
    ),
    ("keyframe.remove", "{clip, prop, t?}  (no t = whole lane)"),
    ("marker.add", "{t, label, id?} → {marker}"),
    ("marker.remove", "{marker}"),
    (
        "transition.set",
        "{between: [a, b], type, dur} → {transition}",
    ),
    ("transition.remove", "{transition}"),
    (
        "ripple.delete",
        "{clip}  (remove and close the gap on its track; destructive)",
    ),
    ("batch", "{ops: [op, …]}  one transaction, one undo step"),
];

/// Clip/track ids an op touches, for lock checks and destructiveness.
fn clip_arg(op: &Op) -> Option<&str> {
    op.args.get("clip").and_then(Value::as_str)
}

pub fn removes_count(op: &Op) -> usize {
    match op.kind.as_str() {
        "clip.remove" | "ripple.delete" => 1,
        "batch" => op
            .args
            .get("ops")
            .and_then(Value::as_array)
            .map(|ops| {
                ops.iter()
                    .filter_map(|o| serde_json::from_value::<Op>(o.clone()).ok())
                    .map(|o| removes_count(&o))
                    .sum()
            })
            .unwrap_or(0),
        _ => 0,
    }
}

/// Spec §6.3: destructive = removes more than `threshold` items, or track.remove, or ripple.delete.
pub fn is_destructive(op: &Op, threshold: usize) -> bool {
    fn structural(op: &Op) -> bool {
        match op.kind.as_str() {
            "track.remove" | "ripple.delete" => true,
            "batch" => op
                .args
                .get("ops")
                .and_then(Value::as_array)
                .is_some_and(|ops| {
                    ops.iter()
                        .filter_map(|o| serde_json::from_value::<Op>(o.clone()).ok())
                        .any(|o| structural(&o))
                }),
            _ => false,
        }
    }
    structural(op) || removes_count(op) > threshold
}

struct Ctx<'a> {
    root: &'a MapRef,
}

impl Ctx<'_> {
    fn map(&self, txn: &mut TransactionMut, key: &str) -> MapRef {
        ensure_map(txn, self.root, key)
    }
    fn clip(&self, txn: &mut TransactionMut, id: &str) -> Result<MapRef> {
        let clips = self.map(txn, "clips");
        child_map(txn, &clips, id).ok_or_else(|| anyhow!("unknown clip {id}"))
    }
    fn track(&self, txn: &mut TransactionMut, id: &str) -> Result<MapRef> {
        let tracks = self.map(txn, "tracks");
        child_map(txn, &tracks, id).ok_or_else(|| anyhow!("unknown track {id}"))
    }
}

fn check_unlocked(tl: &Timeline, track: &str) -> Result<()> {
    match tl.track(track) {
        Some(t) if t.locked => bail!("track {track} is locked"),
        Some(_) => Ok(()),
        None => bail!("unknown track {track}"),
    }
}

/// Apply one op inside `txn`. Returns the op's result (e.g. created ids).
pub fn apply(txn: &mut TransactionMut, root: &MapRef, op: &Op) -> Result<Value> {
    let a = &op.args;
    let cx = Ctx { root };
    // Current state, for lookups and lock checks (reflects earlier ops in a batch).
    let tl = ydoc::read_timeline(txn, root)?;
    if let Some(c) = clip_arg(op) {
        if let Some(clip) = tl.clips.get(c) {
            check_unlocked(&tl, &clip.track)?;
        }
    }

    let result = match op.kind.as_str() {
        "batch" => {
            let ops: Vec<Op> = serde_json::from_value(a.get("ops").cloned().unwrap_or(json!([])))
                .map_err(|e| anyhow!("batch.ops: {e}"))?;
            let mut results = vec![];
            for (n, o) in ops.iter().enumerate() {
                if o.kind == "batch" {
                    bail!("nested batch is not allowed");
                }
                results.push(
                    apply(txn, root, o).map_err(|e| anyhow!("batch op #{n} ({}): {e}", o.kind))?,
                );
            }
            json!({ "results": results })
        }

        "timeline.set" => {
            let meta = cx.map(txn, "meta");
            for k in ["duration", "width", "height"] {
                if let Some(v) = oi(a, k)? {
                    if v < 0 || (k != "duration" && v == 0) {
                        bail!("{k} must be positive");
                    }
                    set(txn, &meta, k, &json!(v));
                }
            }
            if let Some(f) = a.get("fps") {
                let (num, den) = (i(f, "num")?, i(f, "den")?);
                set(txn, &meta, "fps_num", &json!(num));
                set(txn, &meta, "fps_den", &json!(den));
            }
            json!({})
        }

        "track.add" => {
            let id = a
                .get("id")
                .and_then(Value::as_str)
                .map(String::from)
                .unwrap_or_else(|| gen_id("t"));
            if tl.track(&id).is_some() {
                bail!("track {id} already exists");
            }
            let kind = s(a, "kind")?;
            if !["video", "audio", "component", "text", "3d"].contains(&kind) {
                bail!("unknown track kind {kind}");
            }
            let order = a
                .get("order")
                .and_then(Value::as_f64)
                .unwrap_or_else(|| tl.tracks.iter().map(|t| t.order).fold(-1.0, f64::max) + 1.0);
            let tracks = cx.map(txn, "tracks");
            ydoc::write_track(
                txn,
                &tracks,
                &crate::timeline::Track {
                    id: id.clone(),
                    kind: kind.into(),
                    name: a.get("name").and_then(Value::as_str).map(String::from),
                    order,
                    muted: false,
                    locked: false,
                },
            );
            json!({ "track": id })
        }

        "track.remove" => {
            let id = s(a, "track")?;
            check_unlocked(&tl, id)?;
            let clips = cx.map(txn, "clips");
            let doomed: Vec<_> = tl
                .clips
                .iter()
                .filter(|(_, c)| c.track == id)
                .map(|(k, _)| k.clone())
                .collect();
            for c in &doomed {
                clips.remove(txn, c);
            }
            cx.map(txn, "tracks").remove(txn, id);
            json!({ "removed_clips": doomed })
        }

        "track.reorder" => {
            let t = cx.track(txn, s(a, "track")?)?;
            let order = a
                .get("order")
                .and_then(Value::as_f64)
                .ok_or_else(|| anyhow!("missing number arg `order`"))?;
            set(txn, &t, "order", &json!(order));
            json!({})
        }

        "track.set" => {
            let t = cx.track(txn, s(a, "track")?)?;
            for k in ["name", "muted", "locked"] {
                if let Some(v) = a.get(k) {
                    set(txn, &t, k, v);
                }
            }
            json!({})
        }

        "clip.add" => {
            let id = a
                .get("id")
                .and_then(Value::as_str)
                .map(String::from)
                .unwrap_or_else(|| gen_id("c"));
            if tl.clips.contains_key(&id) {
                bail!("clip {id} already exists");
            }
            let track = s(a, "track")?;
            check_unlocked(&tl, track)?;
            let in_ = oi(a, "in")?.unwrap_or(0);
            let out = match (oi(a, "out")?, oi(a, "dur")?) {
                (Some(o), _) => o,
                (None, Some(d)) => in_ + d,
                (None, None) => bail!("clip.add needs `out` or `dur`"),
            };
            let clip = crate::timeline::Clip {
                track: track.into(),
                asset: a.get("asset").and_then(Value::as_str).map(String::from),
                component: a.get("component").and_then(Value::as_str).map(String::from),
                scene: a.get("scene").and_then(Value::as_str).map(String::from),
                params: a.get("params").and_then(Value::as_object).cloned().unwrap_or_default(),
                start: i(a, "start")?,
                in_,
                out,
                props: a
                    .get("props")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default(),
                keyframes: serde_json::from_value(a.get("keyframes").cloned().unwrap_or(json!({})))
                    .map_err(|e| anyhow!("keyframes: {e}"))?,
                offset_samples: oi(a, "offset_samples")?,
            };
            let clips = cx.map(txn, "clips");
            ydoc::write_clip(txn, &clips, &id, &clip);
            json!({ "clip": id })
        }

        "clip.remove" => {
            let id = s(a, "clip")?;
            cx.clip(txn, id)?;
            cx.map(txn, "clips").remove(txn, id);
            json!({})
        }

        "ripple.delete" => {
            let id = s(a, "clip")?;
            let gone = tl
                .clips
                .get(id)
                .ok_or_else(|| anyhow!("unknown clip {id}"))?
                .clone();
            let clips = cx.map(txn, "clips");
            clips.remove(txn, id);
            for (cid, c) in tl
                .clips
                .iter()
                .filter(|(k, c)| *k != id && c.track == gone.track && c.start >= gone.end())
            {
                let m = cx.clip(txn, cid)?;
                set(txn, &m, "start", &json!(c.start - gone.len()));
            }
            json!({ "shift": -gone.len() })
        }

        "clip.move" => {
            let id = s(a, "clip")?;
            let cur = tl
                .clips
                .get(id)
                .ok_or_else(|| anyhow!("unknown clip {id}"))?;
            let m = cx.clip(txn, id)?;
            let start = match (oi(a, "start")?, oi(a, "by")?) {
                (Some(s), _) => Some(s),
                (None, Some(by)) => Some(cur.start + by),
                _ => None,
            };
            if let Some(st) = start {
                set(txn, &m, "start", &json!(st));
            }
            if let Some(t) = a.get("track").and_then(Value::as_str) {
                check_unlocked(&tl, t)?;
                set(txn, &m, "track", &json!(t));
            }
            json!({})
        }

        "clip.trim" => {
            let m = cx.clip(txn, s(a, "clip")?)?;
            for k in ["in", "out", "start"] {
                if let Some(v) = oi(a, k)? {
                    set(txn, &m, k, &json!(v));
                }
            }
            json!({})
        }

        "clip.split" => {
            let id = s(a, "clip")?;
            let at = i(a, "at")?;
            let c = tl
                .clips
                .get(id)
                .ok_or_else(|| anyhow!("unknown clip {id}"))?
                .clone();
            if at <= c.start || at >= c.end() {
                bail!(
                    "split point {at} must be strictly inside clip {id} ({}–{})",
                    c.start,
                    c.end()
                );
            }
            let cut = c.in_ + (at - c.start);
            let left = cx.clip(txn, id)?;
            set(txn, &left, "out", &json!(cut));
            let rid = gen_id("c");
            let mut right = c.clone();
            right.start = at;
            right.in_ = cut;
            // Keyframes are clip-local: shift the right half's lanes.
            for ks in right.keyframes.values_mut() {
                let shift = at - c.start;
                ks.retain(|k| {
                    k.get("t")
                        .and_then(Value::as_i64)
                        .is_some_and(|t| t >= shift)
                });
                for k in ks.iter_mut() {
                    let t = k["t"].as_i64().unwrap_or(0);
                    k["t"] = json!(t - shift);
                }
            }
            let clips = cx.map(txn, "clips");
            ydoc::write_clip(txn, &clips, &rid, &right);
            json!({ "left": id, "right": rid })
        }

        "clip.set" => {
            let m = cx.clip(txn, s(a, "clip")?)?;
            if let Some(p) = a.get("props").and_then(Value::as_object) {
                let props = ensure_map(txn, &m, "props");
                for (k, v) in p {
                    set(txn, &props, k, v);
                }
            }
            if let Some(p) = a.get("params").and_then(Value::as_object) {
                let params = ensure_map(txn, &m, "params");
                for (k, v) in p {
                    set(txn, &params, k, v);
                }
            }
            for k in ["asset", "component", "scene", "offset_samples"] {
                if let Some(v) = a.get(k) {
                    set(txn, &m, k, v);
                }
            }
            json!({})
        }

        "keyframe.set" => {
            let m = cx.clip(txn, s(a, "clip")?)?;
            let prop = s(a, "prop")?;
            let t = i(a, "t")?;
            let v = a
                .get("v")
                .filter(|v| v.is_number())
                .ok_or_else(|| anyhow!("missing number arg `v`"))?;
            let mut key = json!({ "t": t, "v": v });
            if let Some(e) = a.get("ease").and_then(Value::as_str) {
                key["ease"] = json!(e);
            }
            let kfs = ensure_map(txn, &m, "keyframes");
            let lane: ArrayRef = match kfs.get(txn, prop) {
                Some(Out::YArray(arr)) => arr,
                _ => kfs.insert(txn, prop, yrs::ArrayPrelim::default()),
            };
            // Replace an existing key at t, else insert keeping t-order.
            let existing: Vec<Value> = lane
                .iter(txn)
                .map(|o| match o {
                    Out::Any(x) => ydoc::any_to_json(&x),
                    _ => Value::Null,
                })
                .collect();
            let pos = existing.iter().position(|k| k["t"].as_i64() == Some(t));
            if let Some(p) = pos {
                lane.remove_range(txn, p as u32, 1);
                lane.insert(txn, p as u32, ydoc::json_to_any(&key));
            } else {
                let p = existing
                    .iter()
                    .position(|k| k["t"].as_i64().unwrap_or(0) > t)
                    .unwrap_or(existing.len());
                lane.insert(txn, p as u32, ydoc::json_to_any(&key));
            }
            json!({})
        }

        "keyframe.remove" => {
            let m = cx.clip(txn, s(a, "clip")?)?;
            let prop = s(a, "prop")?;
            let kfs = ensure_map(txn, &m, "keyframes");
            match oi(a, "t")? {
                None => {
                    kfs.remove(txn, prop);
                }
                Some(t) => {
                    let Some(Out::YArray(lane)) = kfs.get(txn, prop) else {
                        bail!("clip has no `{prop}` keyframes")
                    };
                    let pos = lane.iter(txn).position(|o| matches!(&o, Out::Any(x) if ydoc::any_to_json(x)["t"].as_i64() == Some(t)))
                        .ok_or_else(|| anyhow!("no `{prop}` keyframe at t={t}"))?;
                    lane.remove_range(txn, pos as u32, 1);
                }
            }
            json!({})
        }

        "marker.add" => {
            let id = a
                .get("id")
                .and_then(Value::as_str)
                .map(String::from)
                .unwrap_or_else(|| gen_id("m"));
            let markers = cx.map(txn, "markers");
            ydoc::write_marker(
                txn,
                &markers,
                &id,
                &crate::timeline::Marker {
                    t: i(a, "t")?,
                    label: s(a, "label")?.into(),
                },
            );
            json!({ "marker": id })
        }

        "marker.remove" => {
            let id = s(a, "marker")?;
            let markers = cx.map(txn, "markers");
            markers
                .remove(txn, id)
                .ok_or_else(|| anyhow!("unknown marker {id}"))?;
            json!({})
        }

        "transition.set" => {
            let between: [String; 2] =
                serde_json::from_value(a.get("between").cloned().unwrap_or(Value::Null))
                    .map_err(|_| anyhow!("`between` must be [clipA, clipB]"))?;
            let id = tl
                .transitions
                .iter()
                .find(|(_, t)| t.between == between)
                .map(|(k, _)| k.clone())
                .unwrap_or_else(|| gen_id("tr"));
            let trs = cx.map(txn, "transitions");
            ydoc::write_transition(
                txn,
                &trs,
                &id,
                &crate::timeline::Transition {
                    between,
                    kind: s(a, "type")?.into(),
                    dur: i(a, "dur")?,
                },
            );
            json!({ "transition": id })
        }

        "transition.remove" => {
            let id = s(a, "transition")?;
            cx.map(txn, "transitions")
                .remove(txn, id)
                .ok_or_else(|| anyhow!("unknown transition {id}"))?;
            json!({})
        }

        other => bail!("unknown op type `{other}`"),
    };
    Ok(result)
}

/// Grow `duration` to cover all clips (never shrinks; agents shrink explicitly).
pub fn grow_duration(txn: &mut TransactionMut, root: &MapRef) -> Result<()> {
    let tl = ydoc::read_timeline(txn, root)?;
    if tl.content_end() > tl.duration {
        let meta = ensure_map(txn, root, "meta");
        set(txn, &meta, "duration", &json!(tl.content_end()));
    }
    Ok(())
}
