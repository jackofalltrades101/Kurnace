//! Timeline ⇄ Yjs layout (spec §7.3).
//!
//! One root map `timeline`:
//!   meta        Map { version, fps_num, fps_den, width, height, duration }
//!   tracks      Map<id, Map { kind, name, order, muted, locked }>
//!   clips       Map<id, Map { track, asset, component, start, in, out, offset_samples,
//!                             props: Map, keyframes: Map<prop, Array<Any>> }>
//!   markers     Map<id, Map { t, label }>
//!   transitions Map<id, Map { between, type, dur }>
//!
//! Numbers are written as integers when integral (lib0 varint → JS number), so
//! the webview's Yjs replica sees plain numbers.
use crate::timeline::{Clip, Marker, Rational, Timeline, Track, Transition};
use anyhow::{Context, Result, anyhow};
use serde_json::{Map as JMap, Value};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use yrs::{
    Any, Array, ArrayPrelim, ArrayRef, Map, MapPrelim, MapRef, Out, ReadTxn, TransactionMut,
    types::ToJson,
};

pub const ROOT: &str = "timeline";

// ---------- value conversion ----------

pub fn json_to_any(v: &Value) -> Any {
    match v {
        Value::Null => Any::Null,
        Value::Bool(b) => Any::Bool(*b),
        Value::Number(n) => match n.as_i64() {
            Some(i) => Any::from(i),
            None => Any::from(n.as_f64().unwrap_or(0.0)),
        },
        Value::String(s) => Any::String(Arc::from(s.as_str())),
        Value::Array(a) => Any::Array(a.iter().map(json_to_any).collect::<Vec<_>>().into()),
        Value::Object(o) => Any::Map(Arc::new(
            o.iter()
                .map(|(k, v)| (k.clone(), json_to_any(v)))
                .collect::<HashMap<_, _>>(),
        )),
    }
}

pub fn any_to_json(a: &Any) -> Value {
    match a {
        Any::Null | Any::Undefined => Value::Null,
        Any::Bool(b) => Value::Bool(*b),
        Any::Number(_) => {
            let f: f64 = a.clone().cast().unwrap_or(0.0);
            num_json(f)
        }
        Any::String(s) => Value::String(s.to_string()),
        Any::Buffer(b) => Value::Array(b.iter().map(|x| Value::from(*x)).collect()),
        Any::Array(xs) => Value::Array(xs.iter().map(any_to_json).collect()),
        Any::Map(m) => {
            // Stable key order for deterministic snapshots.
            let sorted: BTreeMap<_, _> = m.iter().collect();
            Value::Object(
                sorted
                    .into_iter()
                    .map(|(k, v)| (k.clone(), any_to_json(v)))
                    .collect(),
            )
        }
    }
}

fn num_json(f: f64) -> Value {
    if f.fract() == 0.0 && f.abs() < 9.007_199_254_740_991e15 {
        Value::from(f as i64)
    } else {
        Value::from(f)
    }
}

// ---------- shared-type helpers ----------

pub fn root(doc: &yrs::Doc) -> MapRef {
    doc.get_or_insert_map(ROOT)
}

pub fn child_map<T: ReadTxn>(txn: &T, parent: &MapRef, key: &str) -> Option<MapRef> {
    match parent.get(txn, key) {
        Some(Out::YMap(m)) => Some(m),
        _ => None,
    }
}

pub fn ensure_map(txn: &mut TransactionMut, parent: &MapRef, key: &str) -> MapRef {
    if let Some(m) = child_map(txn, parent, key) {
        return m;
    }
    parent.insert(txn, key, MapPrelim::default())
}

pub fn set(txn: &mut TransactionMut, m: &MapRef, key: &str, v: &Value) {
    if v.is_null() {
        m.remove(txn, key);
    } else {
        m.insert(txn, key, json_to_any(v));
    }
}

pub fn get_json<T: ReadTxn>(txn: &T, m: &MapRef, key: &str) -> Value {
    match m.get(txn, key) {
        Some(Out::Any(a)) => any_to_json(&a),
        Some(o) => any_to_json(&o.to_json(txn)),
        None => Value::Null,
    }
}

pub fn get_i64<T: ReadTxn>(txn: &T, m: &MapRef, key: &str) -> Option<i64> {
    get_json(txn, m, key).as_f64().map(|f| f as i64)
}

// ---------- write ----------

pub fn write_track(txn: &mut TransactionMut, tracks: &MapRef, t: &Track) {
    let m: MapRef = tracks.insert(txn, t.id.as_str(), MapPrelim::default());
    set(txn, &m, "kind", &Value::from(t.kind.clone()));
    if let Some(n) = &t.name {
        set(txn, &m, "name", &Value::from(n.clone()));
    }
    set(txn, &m, "order", &num_json(t.order));
    set(txn, &m, "muted", &Value::from(t.muted));
    set(txn, &m, "locked", &Value::from(t.locked));
}

pub fn write_clip(txn: &mut TransactionMut, clips: &MapRef, id: &str, c: &Clip) {
    let m: MapRef = clips.insert(txn, id, MapPrelim::default());
    set(txn, &m, "track", &Value::from(c.track.clone()));
    if let Some(a) = &c.asset {
        set(txn, &m, "asset", &Value::from(a.clone()));
    }
    if let Some(comp) = &c.component {
        set(txn, &m, "component", &Value::from(comp.clone()));
    }
    set(txn, &m, "start", &Value::from(c.start));
    set(txn, &m, "in", &Value::from(c.in_));
    set(txn, &m, "out", &Value::from(c.out));
    if let Some(o) = c.offset_samples {
        set(txn, &m, "offset_samples", &Value::from(o));
    }
    let props: MapRef = m.insert(txn, "props", MapPrelim::default());
    for (k, v) in &c.props {
        set(txn, &props, k, v);
    }
    let kfs: MapRef = m.insert(txn, "keyframes", MapPrelim::default());
    for (lane, keys) in &c.keyframes {
        write_lane(txn, &kfs, lane, keys);
    }
}

pub fn write_lane(txn: &mut TransactionMut, kfs: &MapRef, lane: &str, keys: &[Value]) {
    let arr: ArrayRef = kfs.insert(txn, lane, ArrayPrelim::default());
    for k in keys {
        arr.push_back(txn, json_to_any(k));
    }
}

pub fn write_marker(txn: &mut TransactionMut, markers: &MapRef, id: &str, mk: &Marker) {
    let m: MapRef = markers.insert(txn, id, MapPrelim::default());
    set(txn, &m, "t", &Value::from(mk.t));
    set(txn, &m, "label", &Value::from(mk.label.clone()));
}

pub fn write_transition(txn: &mut TransactionMut, trs: &MapRef, id: &str, tr: &Transition) {
    let m: MapRef = trs.insert(txn, id, MapPrelim::default());
    set(txn, &m, "between", &serde_json::json!(tr.between));
    set(txn, &m, "type", &Value::from(tr.kind.clone()));
    set(txn, &m, "dur", &Value::from(tr.dur));
}

pub fn write_meta(txn: &mut TransactionMut, root: &MapRef, tl: &Timeline) {
    let meta = ensure_map(txn, root, "meta");
    set(txn, &meta, "version", &Value::from(tl.version));
    set(txn, &meta, "fps_num", &Value::from(tl.fps.num));
    set(txn, &meta, "fps_den", &Value::from(tl.fps.den));
    set(txn, &meta, "width", &Value::from(tl.width));
    set(txn, &meta, "height", &Value::from(tl.height));
    set(txn, &meta, "duration", &Value::from(tl.duration));
}

/// Populate an empty doc from a materialized timeline (import / project init).
pub fn init_timeline(txn: &mut TransactionMut, root: &MapRef, tl: &Timeline) {
    write_meta(txn, root, tl);
    let tracks = ensure_map(txn, root, "tracks");
    for t in &tl.tracks {
        write_track(txn, &tracks, t);
    }
    let clips = ensure_map(txn, root, "clips");
    for (id, c) in &tl.clips {
        write_clip(txn, &clips, id, c);
    }
    let markers = ensure_map(txn, root, "markers");
    for (id, m) in &tl.markers {
        write_marker(txn, &markers, id, m);
    }
    let trs = ensure_map(txn, root, "transitions");
    for (id, t) in &tl.transitions {
        write_transition(txn, &trs, id, t);
    }
}

// ---------- read ----------

fn obj(v: &Value) -> JMap<String, Value> {
    v.as_object().cloned().unwrap_or_default()
}

fn int(v: &Value, k: &str) -> i64 {
    v.get(k)
        .and_then(Value::as_f64)
        .map(|f| f as i64)
        .unwrap_or(0)
}

/// Materialize the timeline from the doc. Pure read.
pub fn read_timeline<T: ReadTxn>(txn: &T, root: &MapRef) -> Result<Timeline> {
    let j = any_to_json(&root.to_json(txn));
    let meta = j
        .get("meta")
        .ok_or_else(|| anyhow!("timeline doc has no meta (not initialized)"))?;
    let mut tracks: Vec<Track> = obj(j.get("tracks").unwrap_or(&Value::Null))
        .into_iter()
        .map(|(id, t)| Track {
            id,
            kind: t
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("video")
                .to_string(),
            name: t.get("name").and_then(Value::as_str).map(String::from),
            order: t.get("order").and_then(Value::as_f64).unwrap_or(0.0),
            muted: t.get("muted").and_then(Value::as_bool).unwrap_or(false),
            locked: t.get("locked").and_then(Value::as_bool).unwrap_or(false),
        })
        .collect();
    tracks.sort_by(|a, b| {
        a.order
            .partial_cmp(&b.order)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.id.cmp(&b.id))
    });

    let mut clips = BTreeMap::new();
    for (id, c) in obj(j.get("clips").unwrap_or(&Value::Null)) {
        let keyframes = obj(c.get("keyframes").unwrap_or(&Value::Null))
            .into_iter()
            .map(|(lane, ks)| {
                let mut ks = ks.as_array().cloned().unwrap_or_default();
                ks.sort_by(|a, b| int(a, "t").cmp(&int(b, "t")));
                (lane, ks)
            })
            .collect();
        clips.insert(
            id,
            Clip {
                track: c
                    .get("track")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                asset: c.get("asset").and_then(Value::as_str).map(String::from),
                component: c.get("component").and_then(Value::as_str).map(String::from),
                start: int(&c, "start"),
                in_: int(&c, "in"),
                out: int(&c, "out"),
                props: obj(c.get("props").unwrap_or(&Value::Null)),
                keyframes,
                offset_samples: c
                    .get("offset_samples")
                    .and_then(Value::as_f64)
                    .map(|f| f as i64),
            },
        );
    }
    let markers = obj(j.get("markers").unwrap_or(&Value::Null))
        .into_iter()
        .map(|(id, m)| {
            (
                id,
                Marker {
                    t: int(&m, "t"),
                    label: m
                        .get("label")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .into(),
                },
            )
        })
        .collect();
    let mut transitions = BTreeMap::new();
    for (id, t) in obj(j.get("transitions").unwrap_or(&Value::Null)) {
        let between: [String; 2] =
            serde_json::from_value(t.get("between").cloned().unwrap_or(Value::Null))
                .with_context(|| format!("transition {id}: bad `between`"))?;
        transitions.insert(
            id,
            Transition {
                between,
                kind: t
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("crossfade")
                    .into(),
                dur: int(&t, "dur"),
            },
        );
    }
    Ok(Timeline {
        version: int(meta, "version") as u32,
        fps: Rational {
            num: int(meta, "fps_num"),
            den: int(meta, "fps_den"),
        },
        width: int(meta, "width"),
        height: int(meta, "height"),
        duration: int(meta, "duration"),
        tracks,
        clips,
        markers,
        transitions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use yrs::{Doc, Transact};

    #[test]
    fn roundtrip_through_doc() {
        let src = r#"{"version":2,"fps":{"num":30000,"den":1001},"width":1920,"height":1080,"duration":300,
          "tracks":[{"id":"v1","kind":"video","order":0},{"id":"a1","kind":"audio","name":"VO","order":1,"muted":true}],
          "clips":{"c":{"track":"v1","asset":"x","start":0,"in":0,"out":10,"props":{"opacity":0.5,"x":-300},
                        "keyframes":{"opacity":[{"t":15,"v":1},{"t":0,"v":0,"ease":"easeOutCubic"}]}}},
          "markers":{"m":{"t":120,"label":"drop"}}}"#;
        let tl: Timeline = serde_json::from_str(src).unwrap();
        let doc = Doc::new();
        let r = root(&doc);
        init_timeline(&mut doc.transact_mut(), &r, &tl);
        let back = read_timeline(&doc.transact(), &r).unwrap();
        let mut expected = tl.clone();
        expected
            .clips
            .get_mut("c")
            .unwrap()
            .keyframes
            .get_mut("opacity")
            .unwrap()
            .sort_by_key(|k| k["t"].as_i64());
        assert_eq!(back, expected);
    }
}
