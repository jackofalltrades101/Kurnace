//! Board document (spec §13). The live doc is a Yjs array of
//! `{ el, pos }` maps plus a `assets` map, matching the layout `y-excalidraw`
//! expects. `board.excalidraw` is the export-only snapshot.
use crate::ops::{self, Op};
use crate::timeline::Timeline;
use crate::ydoc::{any_to_json, get_json, json_to_any};
use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use yrs::{Any, Array, ArrayRef, Doc, Map, MapPrelim, Out, ReadTxn, Transact, TransactionMut};

pub const ELEMENTS: &str = "elements";
pub const ASSETS: &str = "assets";

pub fn elements(doc: &Doc) -> Result<Vec<Value>> {
    let arr = doc.get_or_insert_array(ELEMENTS);
    let txn = doc.transact();
    Ok(read_items(&txn, &arr)
        .into_iter()
        .map(|(_, el)| el)
        .collect())
}

fn read_items<T: ReadTxn>(txn: &T, arr: &ArrayRef) -> Vec<(String, Value)> {
    let mut out = vec![];
    for item in arr.iter(txn) {
        if let Out::YMap(m) = item {
            let pos = get_json(txn, &m, "pos")
                .as_str()
                .unwrap_or_default()
                .to_string();
            out.push((pos, get_json(txn, &m, "el")));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn find_index<T: ReadTxn>(txn: &T, arr: &ArrayRef, id: &str) -> Option<u32> {
    arr.iter(txn).enumerate().find_map(|(i, item)| match item {
        Out::YMap(m) if get_json(txn, &m, "el")["id"] == json!(id) => Some(i as u32),
        _ => None,
    })
}

fn last_pos<T: ReadTxn>(txn: &T, arr: &ArrayRef) -> Option<String> {
    read_items(txn, arr).last().map(|(pos, _)| pos.clone())
}

fn insert_element(txn: &mut TransactionMut, arr: &ArrayRef, el: &Value) -> Result<()> {
    let pos = match last_pos(txn, arr) {
        Some(p) => next_pos(Some(&p)),
        None => "a0".to_string(),
    };
    let idx = arr.len(txn);
    arr.insert(
        txn,
        idx,
        MapPrelim::from_iter([
            ("el", json_to_any(el)),
            ("pos", Any::String(Arc::from(pos.as_str()))),
        ]),
    );
    Ok(())
}

fn set_element(txn: &mut TransactionMut, arr: &ArrayRef, index: u32, el: &Value) {
    if let Some(Out::YMap(m)) = arr.get(txn, index) {
        m.insert(txn, "el", json_to_any(el));
    }
}

fn bump(el: &mut Value) {
    if let Some(o) = el.as_object_mut() {
        let v = o.get("version").and_then(Value::as_u64).unwrap_or(0);
        o.insert("version".into(), json!(v + 1));
        o.insert("versionNonce".into(), json!(nonce()));
        o.insert("updated".into(), json!(now_millis()));
    }
}

fn nonce() -> u32 {
    ulid::Ulid::generate().0 as u32
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Apply a board op. Board edits are loose (spec R8): no timeline-style
/// validation, just enough shape to keep Excalidraw happy.
pub fn apply(txn: &mut TransactionMut, arr: &ArrayRef, op: &Op) -> Result<Value> {
    let a = &op.args;
    match op.kind.as_str() {
        "batch" => {
            let ops: Vec<Op> = serde_json::from_value(a.get("ops").cloned().unwrap_or(json!([])))?;
            let mut results = vec![];
            for o in &ops {
                results.push(apply(txn, arr, o)?);
            }
            Ok(json!({ "applied": results.len(), "results": results }))
        }
        "el.add" => {
            let spec = a.get("element").cloned().unwrap_or_else(|| a.clone());
            let (el, label) = normalize(spec)?;
            let id = el["id"].as_str().unwrap_or_default().to_string();
            insert_element(txn, arr, &el)?;
            if let Some(text) = label {
                // A standalone text element survives Excalidraw's reconciliation;
                // bound text needs measured container geometry we don't have yet.
                let text_el = text_element(&el, &text);
                insert_element(txn, arr, &text_el)?;
            }
            Ok(json!({ "element": id }))
        }
        "el.update" => {
            let id = a
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("el.update needs `id`"))?;
            let patch = a
                .get("patch")
                .and_then(Value::as_object)
                .ok_or_else(|| anyhow!("el.update needs `patch` object"))?;
            let idx = find_index(txn, arr, id).ok_or_else(|| anyhow!("unknown element {id}"))?;
            let mut el = get_element(txn, arr, id)?;
            let o = el
                .as_object_mut()
                .ok_or_else(|| anyhow!("element {id} is not an object"))?;
            for (k, v) in patch {
                o.insert(k.clone(), v.clone());
            }
            bump(&mut el);
            set_element(txn, arr, idx, &el);
            Ok(json!({ "element": id }))
        }
        "el.remove" => {
            let id = a
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("el.remove needs `id`"))?;
            let idx = find_index(txn, arr, id).ok_or_else(|| anyhow!("unknown element {id}"))?;
            arr.remove(txn, idx);
            Ok(json!({ "removed": id }))
        }
        "frame.add" => {
            let name = a.get("name").and_then(Value::as_str).unwrap_or("Scene");
            let mut spec = a.clone();
            if let Some(o) = spec.as_object_mut() {
                o.insert("type".into(), json!("frame"));
                o.insert("name".into(), json!(name));
                o.entry("width").or_insert(json!(400));
                o.entry("height").or_insert(json!(240));
            }
            let (el, _) = normalize(spec)?;
            let id = el["id"].as_str().unwrap_or_default().to_string();
            insert_element(txn, arr, &el)?;
            if let Some(scene) = a.get("scene").and_then(Value::as_str) {
                link(txn, arr, &id, "scene", scene)?;
            }
            Ok(json!({ "frame": id }))
        }
        "link.asset" | "link.scene" => {
            let id = a
                .get("element")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("{} needs `element`", op.kind))?;
            let value = a
                .get("asset")
                .or_else(|| a.get("scene"))
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("{} needs a value", op.kind))?;
            let key = if op.kind == "link.asset" {
                "asset"
            } else {
                "scene"
            };
            link(txn, arr, id, key, value)?;
            Ok(json!({ "element": id, key: value }))
        }
        other => bail!("unknown board op `{other}`"),
    }
}

fn get_element<T: ReadTxn>(txn: &T, arr: &ArrayRef, id: &str) -> Result<Value> {
    arr.iter(txn)
        .find_map(|item| match item {
            Out::YMap(m) if get_json(txn, &m, "el")["id"] == json!(id) => {
                Some(get_json(txn, &m, "el"))
            }
            _ => None,
        })
        .ok_or_else(|| anyhow!("unknown element {id}"))
}

fn link(txn: &mut TransactionMut, arr: &ArrayRef, id: &str, key: &str, value: &str) -> Result<()> {
    let idx = find_index(txn, arr, id).ok_or_else(|| anyhow!("unknown element {id}"))?;
    let mut el = get_element(txn, arr, id)?;
    let o = el
        .as_object_mut()
        .ok_or_else(|| anyhow!("element {id} is not an object"))?;
    let cd = o
        .entry("customData")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| anyhow!("element {id} customData is not an object"))?;
    cd.insert(key.into(), json!(value));
    bump(&mut el);
    set_element(txn, arr, idx, &el);
    Ok(())
}

fn text_element(container: &Value, text: &str) -> Value {
    let x = container["x"].as_f64().unwrap_or(0.0);
    let y = container["y"].as_f64().unwrap_or(0.0);
    let w = container["width"].as_f64().unwrap_or(120.0);
    let h = container["height"].as_f64().unwrap_or(40.0);
    let font = container["fontSize"].as_f64().unwrap_or(20.0);
    // Non-zero measured box: Excalidraw drops zero-sized standalone text.
    let tw = (text.chars().count() as f64 * font * 0.6).max(20.0);
    let th = font * 1.25;
    json!({
        "id": ops::gen_id("txt"), "type": "text",
        "x": x + (w - tw) / 2.0, "y": y + (h - th) / 2.0,
        "width": tw, "height": th, "angle": 0,
        "strokeColor": "#e6e7ea", "backgroundColor": "transparent",
        "fillStyle": "solid", "strokeWidth": 2, "strokeStyle": "solid",
        "roughness": 1, "opacity": 100, "groupIds": [], "frameId": null,
        "roundness": null, "seed": nonce(), "version": 1, "versionNonce": nonce(),
        "updated": now_millis(), "isDeleted": false, "boundElements": null,
        "link": null, "locked": false, "index": null,
        "originalText": text, "text": text, "fontSize": font, "fontFamily": 1,
        "textAlign": "center", "verticalAlign": "middle", "containerId": null,
        "lineHeight": 1.25, "autoResize": true,
    })
}

/// Fill the fields Excalidraw expects. Returns the element and an optional label.
fn normalize(spec: Value) -> Result<(Value, Option<String>)> {
    let mut o = spec
        .as_object()
        .cloned()
        .ok_or_else(|| anyhow!("element must be a JSON object"))?;
    let label = match o.remove("label") {
        Some(Value::String(s)) => Some(s),
        Some(Value::Object(m)) => m.get("text").and_then(Value::as_str).map(|s| s.to_string()),
        _ => None,
    };
    let mut ty = o
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("rectangle")
        .to_string();
    if ty == "rect" || ty == "box" {
        ty = "rectangle".into();
    }
    o.insert("type".into(), json!(ty));
    o.entry("id").or_insert(json!(ops::gen_id("el")));
    o.entry("x").or_insert(json!(0.0));
    o.entry("y").or_insert(json!(0.0));
    let (dw, dh) = match ty.as_str() {
        "frame" => (400.0, 240.0),
        "text" => (200.0, 40.0),
        "arrow" => (120.0, 0.0),
        _ => (160.0, 100.0),
    };
    o.entry("width").or_insert(json!(dw));
    o.entry("height").or_insert(json!(dh));
    o.entry("angle").or_insert(json!(0));
    o.entry("strokeColor").or_insert(json!("#e6e7ea"));
    o.entry("backgroundColor").or_insert(json!("transparent"));
    o.entry("fillStyle").or_insert(json!("solid"));
    o.entry("strokeWidth").or_insert(json!(2));
    o.entry("strokeStyle").or_insert(json!("solid"));
    o.entry("roughness").or_insert(json!(1));
    o.entry("opacity").or_insert(json!(100));
    o.entry("groupIds").or_insert(json!([]));
    o.entry("frameId").or_insert(Value::Null);
    o.entry("roundness").or_insert(Value::Null);
    o.entry("seed").or_insert(json!(nonce()));
    o.entry("version").or_insert(json!(1));
    o.entry("versionNonce").or_insert(json!(nonce()));
    o.entry("updated").or_insert(json!(now_millis()));
    o.entry("isDeleted").or_insert(json!(false));
    o.entry("boundElements").or_insert(Value::Null);
    o.entry("link").or_insert(Value::Null);
    o.entry("locked").or_insert(json!(false));
    o.entry("index").or_insert(Value::Null);
    match ty.as_str() {
        "text" => {
            let text = o
                .get("text")
                .and_then(Value::as_str)
                .map(|s| s.to_string())
                .or_else(|| label.clone())
                .unwrap_or_default();
            o.insert("text".into(), json!(text));
            o.entry("originalText").or_insert(json!(text));
            o.entry("fontSize").or_insert(json!(20));
            o.entry("fontFamily").or_insert(json!(1));
            o.entry("textAlign").or_insert(json!("left"));
            o.entry("verticalAlign").or_insert(json!("top"));
            o.entry("containerId").or_insert(Value::Null);
            o.entry("lineHeight").or_insert(json!(1.25));
            o.entry("autoResize").or_insert(json!(true));
        }
        "frame" => {
            let name = o
                .get("name")
                .and_then(Value::as_str)
                .map(|s| s.to_string())
                .or_else(|| label.clone())
                .unwrap_or_else(|| "Scene".into());
            o.insert("name".into(), json!(name));
        }
        "arrow" => {
            o.entry("points").or_insert(json!([[0, 0], [100, 0]]));
            o.entry("lastCommittedPoint").or_insert(Value::Null);
            o.entry("startBinding").or_insert(Value::Null);
            o.entry("endBinding").or_insert(Value::Null);
            o.entry("startArrowhead").or_insert(Value::Null);
            o.entry("endArrowhead").or_insert(Value::Null);
        }
        _ => {}
    }
    Ok((Value::Object(o), label))
}

/// Minimal CC0 fractional-indexing `generateKeyBetween(a, null)` port, so
/// programmatic additions and y-excalidraw's UI additions stay mutually valid.
pub fn next_pos(a: Option<&str>) -> String {
    const DIGITS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let a = match a {
        None => return "a0".into(),
        Some(a) if a.is_empty() => return "a0".into(),
        Some(a) => a,
    };
    let head = a.as_bytes()[0];
    let ilen = match head {
        b'a'..=b'z' => (head - b'a') as usize + 2,
        b'A'..=b'Z' => (b'Z' - head) as usize + 2,
        _ => return "a0".into(),
    };
    if ilen > a.len() {
        return "a0".into();
    }
    let (int, frac) = a.split_at(ilen);
    if let Some(next) = inc_int(int, DIGITS) {
        return next;
    }
    format!("{int}{}", midpoint(frac, None, DIGITS))
}

fn inc_int(x: &str, digits: &[u8]) -> Option<String> {
    let head = *x.as_bytes().first()?;
    let mut digs: Vec<u8> = x.as_bytes()[1..].to_vec();
    let mut carry = true;
    for d in digs.iter_mut().rev() {
        let di = digits.iter().position(|c| c == d)?;
        if di + 1 == digits.len() {
            *d = digits[0];
        } else {
            *d = digits[di + 1];
            carry = false;
            break;
        }
    }
    if carry {
        if head == b'Z' {
            return Some(format!("a{}", digits[0] as char));
        }
        if head == b'z' {
            return None;
        }
        let h = head + 1;
        if h > b'a' {
            digs.push(digits[0]);
        } else {
            digs.pop();
        }
        digs.insert(0, h);
        return Some(String::from_utf8(digs).ok()?);
    }
    digs.insert(0, head);
    Some(String::from_utf8(digs).ok()?)
}

fn midpoint(a: &str, b: Option<&str>, digits: &[u8]) -> String {
    let zero = digits[0];
    if let Some(b) = b {
        if a >= b {
            return a.to_string();
        }
        let mut n = 0;
        while (a.as_bytes().get(n).copied().unwrap_or(zero)) == b.as_bytes()[n] {
            n += 1;
        }
        if n > 0 {
            return format!("{}{}", &b[..n], midpoint(&a[n..], Some(&b[n..]), digits));
        }
    }
    let da = a
        .as_bytes()
        .first()
        .and_then(|c| digits.iter().position(|d| d == c))
        .unwrap_or(0);
    let db = match b {
        Some(b) => b
            .as_bytes()
            .first()
            .and_then(|c| digits.iter().position(|d| d == c))
            .unwrap_or(digits.len()),
        None => digits.len(),
    };
    if db - da > 1 {
        (digits[(da + db) / 2] as char).to_string()
    } else if let Some(b) = b {
        if b.len() > 1 {
            b[..1].to_string()
        } else {
            format!("{}{}", digits[da] as char, midpoint(&a[1..], None, digits))
        }
    } else {
        format!("{}{}", digits[da] as char, midpoint(&a[1..], None, digits))
    }
}

// ---------- snapshots ----------

pub fn snapshot(doc: &Doc) -> Value {
    let elements = elements(doc).unwrap_or_default();
    let assets = doc.get_or_insert_map(ASSETS);
    let files = {
        let txn = doc.transact();
        let mut m = serde_json::Map::new();
        for (k, v) in assets.iter(&txn) {
            if let Out::Any(a) = v {
                m.insert(k.to_string(), any_to_json(&a));
            }
        }
        Value::Object(m)
    };
    json!({
        "type": "excalidraw",
        "version": 2,
        "source": "https://kurnace.local",
        "elements": elements,
        "appState": { "viewBackgroundColor": "#17181b", "theme": "dark" },
        "files": files,
    })
}

pub fn import_snapshot(doc: &Doc, snap: &Value) -> Result<()> {
    let arr = doc.get_or_insert_array(ELEMENTS);
    let assets = doc.get_or_insert_map(ASSETS);
    let els = snap
        .get("elements")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut txn = doc.transact_mut_with("import");
    if let Some(files) = snap.get("files").and_then(Value::as_object) {
        for (k, v) in files {
            assets.insert(&mut txn, k.as_str(), json_to_any(v));
        }
    }
    for spec in els {
        let (el, label) = normalize(spec)?;
        insert_element(&mut txn, &arr, &el)?;
        if let Some(text) = label {
            insert_element(&mut txn, &arr, &text_element(&el, &text))?;
        }
    }
    Ok(())
}

pub fn to_svg(doc: &Doc, max_w: f64, max_h: f64) -> Result<String> {
    let els = elements(doc)?;
    let live: Vec<&Value> = els
        .iter()
        .filter(|e| e["isDeleted"].as_bool() != Some(true))
        .collect();
    let mut minx = f64::INFINITY;
    let mut miny = f64::INFINITY;
    let mut maxx = f64::NEG_INFINITY;
    let mut maxy = f64::NEG_INFINITY;
    for e in &live {
        let x = e["x"].as_f64().unwrap_or(0.0);
        let y = e["y"].as_f64().unwrap_or(0.0);
        let w = e["width"].as_f64().unwrap_or(0.0);
        let h = e["height"].as_f64().unwrap_or(0.0);
        minx = minx.min(x);
        miny = miny.min(y);
        maxx = maxx.max(x + w);
        maxy = maxy.max(y + h);
    }
    if !minx.is_finite() {
        minx = 0.0;
        miny = 0.0;
        maxx = 640.0;
        maxy = 360.0;
    }
    let pad = 40.0;
    let (w, h) = (maxx - minx + pad * 2.0, maxy - miny + pad * 2.0);
    let scale = (max_w / w).min(max_h / h).min(2.0);
    let mut s = String::new();
    s.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{:.0}\" height=\"{:.0}\" viewBox=\"0 0 {w:.0} {h:.0}\">\n",
        w * scale,
        h * scale
    ));
    s.push_str(&format!(
        "<rect x=\"0\" y=\"0\" width=\"{w:.0}\" height=\"{h:.0}\" fill=\"#111214\"/>\n"
    ));
    s.push_str(&format!(
        "<g transform=\"translate({:.1},{:.1}) scale({scale:.4})\">\n",
        pad - minx,
        pad - miny
    ));
    for e in live {
        let ty = e["type"].as_str().unwrap_or("rectangle");
        let x = e["x"].as_f64().unwrap_or(0.0);
        let y = e["y"].as_f64().unwrap_or(0.0);
        let ew = e["width"].as_f64().unwrap_or(0.0);
        let eh = e["height"].as_f64().unwrap_or(0.0);
        let stroke = e["strokeColor"].as_str().unwrap_or("#e6e7ea");
        let fill = e["backgroundColor"].as_str().unwrap_or("transparent");
        let fill = if fill == "transparent" { "none" } else { fill };
        match ty {
            "frame" => {
                s.push_str(&format!(
                    "<rect x=\"{x:.1}\" y=\"{y:.1}\" width=\"{ew:.1}\" height=\"{eh:.1}\" fill=\"none\" stroke=\"{stroke}\" stroke-dasharray=\"8 6\"/>\n"
                ));
                if let Some(name) = e["name"].as_str() {
                    s.push_str(&format!(
                        "<text x=\"{:.1}\" y=\"{:.1}\" fill=\"{stroke}\" font-family=\"sans-serif\" font-size=\"18\">{}</text>\n",
                        x + 6.0,
                        y - 8.0,
                        esc(name)
                    ));
                }
            }
            "ellipse" => {
                s.push_str(&format!(
                    "<ellipse cx=\"{:.1}\" cy=\"{:.1}\" rx=\"{:.1}\" ry=\"{:.1}\" fill=\"{fill}\" stroke=\"{stroke}\" stroke-width=\"2\"/>\n",
                    x + ew / 2.0,
                    y + eh / 2.0,
                    ew / 2.0,
                    eh / 2.0
                ));
            }
            "diamond" => {
                s.push_str(&format!(
                    "<polygon points=\"{:.1},{:.1} {:.1},{:.1} {:.1},{:.1} {:.1},{:.1}\" fill=\"{fill}\" stroke=\"{stroke}\" stroke-width=\"2\"/>\n",
                    x + ew / 2.0, y, x + ew, y + eh / 2.0, x + ew / 2.0, y + eh, x, y + eh / 2.0
                ));
            }
            "arrow" | "line" => {
                let pts = e["points"].as_array().cloned().unwrap_or_default();
                let pts: Vec<String> = pts
                    .iter()
                    .filter_map(|p| {
                        let a = p.get(0)?.as_f64()?;
                        let b = p.get(1)?.as_f64()?;
                        Some(format!("{:.1},{:.1}", x + a, y + b))
                    })
                    .collect();
                s.push_str(&format!(
                    "<polyline points=\"{}\" fill=\"none\" stroke=\"{stroke}\" stroke-width=\"2\"/>\n",
                    pts.join(" ")
                ));
            }
            "text" => {
                let text = e["text"].as_str().unwrap_or("");
                let size = e["fontSize"].as_f64().unwrap_or(20.0);
                s.push_str(&format!(
                    "<text x=\"{x:.1}\" y=\"{:.1}\" fill=\"{stroke}\" font-family=\"sans-serif\" font-size=\"{size}\" text-anchor=\"middle\">{}</text>\n",
                    y + size,
                    esc(text)
                ));
            }
            _ => {
                s.push_str(&format!(
                    "<rect x=\"{x:.1}\" y=\"{y:.1}\" width=\"{ew:.1}\" height=\"{eh:.1}\" fill=\"{fill}\" stroke=\"{stroke}\" stroke-width=\"2\" rx=\"4\"/>\n"
                ));
                if let Some(a) = e["customData"]["asset"].as_str() {
                    s.push_str(&format!(
                        "<text x=\"{:.1}\" y=\"{:.1}\" fill=\"{stroke}\" font-family=\"sans-serif\" font-size=\"16\">{}</text>\n",
                        x + 10.0,
                        y + 24.0,
                        esc(a)
                    ));
                }
            }
        }
    }
    s.push_str("</g></svg>\n");
    Ok(s)
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

// ---------- board → timeline ----------

#[derive(Debug, Clone)]
pub struct AssetMeta {
    pub kind: String,
    pub duration_s: f64,
}

/// Build a single `batch` of timeline ops from board frames (spec §13.1).
pub fn to_timeline_ops(
    board: &[Value],
    tl: &Timeline,
    fps: f64,
    asset: &dyn Fn(&str) -> Option<AssetMeta>,
) -> Result<(Vec<Op>, Value)> {
    let mut frames: Vec<&Value> = board
        .iter()
        .filter(|e| e["type"] == json!("frame") && e["isDeleted"].as_bool() != Some(true))
        .collect();
    if frames.is_empty() {
        bail!("board has no frames; add one with `kurn board frame`");
    }
    frames.sort_by(|a, b| {
        let ax = a["x"].as_f64().unwrap_or(0.0);
        let bx = b["x"].as_f64().unwrap_or(0.0);
        ax.partial_cmp(&bx)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                let ay = a["y"].as_f64().unwrap_or(0.0);
                let by = b["y"].as_f64().unwrap_or(0.0);
                ay.partial_cmp(&by).unwrap_or(std::cmp::Ordering::Equal)
            })
    });

    let have: std::collections::HashSet<String> = tl.tracks.iter().map(|t| t.id.clone()).collect();
    let mut ops = vec![];
    for (id, kind, name) in [
        ("v_board", "video", "Board"),
        ("a_board", "audio", "Board VO"),
        ("t_board", "text", "Board titles"),
    ] {
        if !have.contains(id) {
            ops.push(Op::new(
                "track.add",
                json!({ "id": id, "kind": kind, "name": name }),
            ));
        }
    }
    // Re-running "to timeline" replaces the previous rough cut, not duplicating it.
    for id in tl.clips.keys().filter(|id| id.starts_with("board_")) {
        ops.push(Op::new("clip.remove", json!({ "clip": id })));
    }
    for id in tl.markers.keys().filter(|id| id.starts_with("board_")) {
        ops.push(Op::new("marker.remove", json!({ "marker": id })));
    }

    let sec = |s: f64| ((s.max(0.0) * fps).round() as i64).max(1);
    let default_len = sec(3.0);
    let mut cursor = 0i64;
    let mut clip_count = 0;
    let mut scene_count = 0;

    for (si, frame) in frames.iter().enumerate() {
        let fx = frame["x"].as_f64().unwrap_or(0.0);
        let fy = frame["y"].as_f64().unwrap_or(0.0);
        let fw = frame["width"].as_f64().unwrap_or(400.0);
        let fh = frame["height"].as_f64().unwrap_or(240.0);
        let name = frame["name"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("Scene {}", si + 1));

        let members: Vec<&Value> = board
            .iter()
            .filter(|e| {
                if e["isDeleted"].as_bool() == Some(true) || std::ptr::eq(*e, *frame) {
                    return false;
                }
                if e["type"] == json!("frame") {
                    return false;
                }
                if let Some(fid) = e["frameId"].as_str() {
                    if fid == frame["id"].as_str().unwrap_or_default() {
                        return true;
                    }
                }
                let x = e["x"].as_f64().unwrap_or(0.0);
                let y = e["y"].as_f64().unwrap_or(0.0);
                let w = e["width"].as_f64().unwrap_or(0.0);
                let h = e["height"].as_f64().unwrap_or(0.0);
                let cx = x + w / 2.0;
                let cy = y + h / 2.0;
                cx >= fx && cx <= fx + fw && cy >= fy && cy <= fy + fh
            })
            .collect();

        let mut visuals: Vec<(String, i64)> = vec![];
        let mut voices: Vec<(String, i64)> = vec![];
        for e in &members {
            if let Some(a) = e["customData"]["asset"].as_str() {
                let len = asset(a)
                    .map(|m| {
                        if m.kind == "image" || m.duration_s <= 0.0 {
                            default_len
                        } else {
                            sec(m.duration_s)
                        }
                    })
                    .unwrap_or(default_len);
                visuals.push((a.to_string(), len));
            }
            if let Some(v) = e["customData"]["voice"].as_str() {
                let len = asset(v)
                    .map(|m| {
                        if m.duration_s > 0.0 {
                            sec(m.duration_s)
                        } else {
                            default_len
                        }
                    })
                    .unwrap_or(default_len);
                voices.push((v.to_string(), len));
            }
        }
        // Frame-level links apply to the whole scene.
        if let Some(a) = frame["customData"]["asset"].as_str() {
            visuals.push((a.to_string(), default_len));
        }
        if let Some(v) = frame["customData"]["voice"].as_str() {
            voices.push((v.to_string(), default_len));
        }

        let visual_total: i64 = visuals.iter().map(|x| x.1).sum();
        let voice_total: i64 = voices.iter().map(|x| x.1).sum();
        let mut scene_len = visual_total.max(voice_total).max(default_len);
        if visuals.len() == 1 {
            // A single still should hold for the whole scene (e.g. its VO).
            visuals[0].1 = scene_len;
        } else if visuals.len() > 1 && visual_total < scene_len {
            if let Some(last) = visuals.last_mut() {
                last.1 += scene_len - visual_total;
            }
        }
        scene_len = scene_len.max(1);

        let mut vx = cursor;
        for (i, (a, len)) in visuals.iter().enumerate() {
            let len = (*len).max(1);
            ops.push(Op::new(
                "clip.add",
                json!({
                    "id": format!("board_s{}_v{}", si, i),
                    "track": "v_board", "asset": a, "start": vx, "in": 0, "out": len,
                }),
            ));
            clip_count += 1;
            vx += len;
        }
        let mut ax = cursor;
        for (i, (a, len)) in voices.iter().enumerate() {
            let len = (*len).max(1);
            ops.push(Op::new(
                "clip.add",
                json!({
                    "id": format!("board_s{}_a{}", si, i),
                    "track": "a_board", "asset": a, "start": ax, "in": 0, "out": len,
                }),
            ));
            clip_count += 1;
            ax += len;
        }
        ops.push(Op::new(
            "clip.add",
            json!({
                "id": format!("board_s{}_title", si),
                "track": "t_board", "component": "text", "start": cursor, "out": scene_len,
                "props": { "text": name, "fontSize": 72, "y": -420, "fill": "#ffffff" },
            }),
        ));
        clip_count += 1;
        ops.push(Op::new(
            "marker.add",
            json!({ "id": format!("board_s{}_marker", si), "t": cursor, "label": name }),
        ));
        cursor += scene_len;
        scene_count += 1;
    }

    Ok((
        ops,
        json!({ "scenes": scene_count, "clips": clip_count, "duration": cursor }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeline::{Rational, Timeline};

    fn add(doc: &Doc, spec: Value) -> String {
        let arr = doc.get_or_insert_array(ELEMENTS);
        let mut txn = doc.transact_mut();
        let r = apply(
            &mut txn,
            &arr,
            &Op::new("el.add", json!({ "element": spec })),
        )
        .unwrap();
        r["element"].as_str().unwrap().to_string()
    }

    #[test]
    fn add_update_link_remove_and_snapshot() {
        let doc = Doc::new();
        let id = add(
            &doc,
            json!({ "type": "rectangle", "x": 10, "y": 20, "width": 100, "height": 50, "label": "Hook" }),
        );
        let arr = doc.get_or_insert_array(ELEMENTS);
        assert_eq!(elements(&doc).unwrap().len(), 2, "container + label text");

        let mut txn = doc.transact_mut();
        apply(
            &mut txn,
            &arr,
            &Op::new("el.update", json!({ "id": id, "patch": { "x": 40 } })),
        )
        .unwrap();
        apply(
            &mut txn,
            &arr,
            &Op::new("link.asset", json!({ "element": id, "asset": "ast_x" })),
        )
        .unwrap();
        drop(txn);
        let els = elements(&doc).unwrap();
        let rect = els.iter().find(|e| e["id"] == json!(id)).unwrap();
        assert_eq!(rect["x"], 40);
        assert_eq!(rect["customData"]["asset"], "ast_x");

        let snap = snapshot(&doc);
        assert_eq!(snap["type"], "excalidraw");
        assert_eq!(snap["elements"].as_array().unwrap().len(), 2);

        let mut txn = doc.transact_mut();
        apply(&mut txn, &arr, &Op::new("el.remove", json!({ "id": id }))).unwrap();
        drop(txn);
        assert_eq!(elements(&doc).unwrap().len(), 1, "label text remains");
    }

    #[test]
    fn snapshot_includes_assets() {
        let doc = Doc::new();
        add(
            &doc,
            json!({ "type": "image", "x": 0, "y": 0, "width": 10, "height": 10, "fileId": "f1" }),
        );
        let assets = doc.get_or_insert_map(ASSETS);
        let mut txn = doc.transact_mut();
        assets.insert(
            &mut txn,
            "f1",
            json_to_any(&json!({ "id": "f1", "dataURL": "data:image/png;base64,AA==" })),
        );
        drop(txn);
        let snap = snapshot(&doc);
        assert_eq!(snap["files"]["f1"]["id"], "f1");
    }

    #[test]
    fn append_positions_are_valid_fractional_keys() {
        let mut p: Option<String> = None;
        let mut seen = vec![];
        for _ in 0..20 {
            let n = next_pos(p.as_deref());
            if let Some(prev) = &p {
                assert!(prev < &n, "{prev} !< {n}");
            }
            p = Some(n.clone());
            seen.push(n);
        }
        assert_eq!(&seen[..3], &["a0", "a1", "a2"]);
    }

    #[test]
    fn board_frames_become_one_timeline_batch() {
        let doc = Doc::new();
        add(
            &doc,
            json!({ "type": "frame", "x": 0, "y": 0, "width": 400, "height": 300,
                    "name": "Scene 1", "customData": { "asset": "ast_a", "voice": "ast_vo" } }),
        );
        add(
            &doc,
            json!({ "type": "frame", "x": 500, "y": 0, "width": 400, "height": 300, "name": "Scene 2" }),
        );
        let tl = Timeline::empty(Rational { num: 30, den: 1 }, 640, 360);
        let (ops, details) = to_timeline_ops(&elements(&doc).unwrap(), &tl, 30.0, &|id| {
            Some(AssetMeta {
                kind: if id == "ast_vo" {
                    "audio".into()
                } else {
                    "image".into()
                },
                duration_s: if id == "ast_vo" { 2.0 } else { 0.0 },
            })
        })
        .unwrap();
        assert_eq!(details["scenes"], 2);
        assert!(ops.iter().any(|o| o.kind == "track.add"));
        assert_eq!(ops.iter().filter(|o| o.kind == "clip.add").count(), 4);
        assert_eq!(ops.iter().filter(|o| o.kind == "marker.add").count(), 2);
    }
}
