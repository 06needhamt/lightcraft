//! Mask effect presets: named sets of local adjustment sliders picked from the Masking panel's
//! Effect menu (Lightroom's local adjustment / brush presets), saved with the library preferences.
//! Imported from Lightroom `.lrtemplate` local presets (`type = "LocalizedAdjustmentPreset"`)
//! and our own `.lceffect` JSON files.

use lightcraft_develop::LocalAdjustments;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd, has_active, str_param};
use crate::preset_import::{Lua, parse_lua};
use crate::{Result, Session};

/// File extension of exported effect presets (JSON).
pub const EFFECT_EXT: &str = "lceffect";
/// `format` tag of an exported effect-preset file.
const FILE_FORMAT: &str = "lightcraft.maskEffects";
/// The Lua `type` of a Lightroom local adjustment preset.
const LR_LOCAL_TYPE: &str = "LocalizedAdjustmentPreset";

/// A named set of local adjustment sliders. Applying one sets every slider of the mask (those the
/// preset doesn't name go to 0); the mask's Amount stays.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MaskEffect {
    pub name: String,
    pub adjust: LocalAdjustments,
}

impl MaskEffect {
    /// `a` with this preset's sliders and `a`'s Amount.
    pub fn apply(&self, a: &LocalAdjustments) -> LocalAdjustments {
        LocalAdjustments { amount: a.amount, ..self.adjust }
    }
    /// Does `a` carry exactly this preset's sliders (Amount aside)?
    pub fn matches(&self, a: &LocalAdjustments) -> bool {
        let strip = |a: &LocalAdjustments| LocalAdjustments { amount: 100.0, ..*a };
        let (x, y) = (serde_json::to_value(strip(&self.adjust)).unwrap_or_default(), serde_json::to_value(strip(a)).unwrap_or_default());
        match (x.as_object(), y.as_object()) {
            (Some(x), Some(y)) => x.iter().all(|(k, v)| {
                let (p, q) = (v.as_f64().unwrap_or(0.0), y.get(k).and_then(Value::as_f64).unwrap_or(0.0));
                (p - q).abs() < 1e-6
            }),
            _ => false,
        }
    }
}

/// Name of the effect the mask adjustments `a` match, if any.
pub fn matching_effect(s: &Session, a: &LocalAdjustments) -> Option<String> {
    s.mask_effects.iter().find(|e| e.matches(a)).map(|e| e.name.clone())
}

/// Add or replace (same name, ignoring case) an effect.
fn upsert(s: &mut Session, e: MaskEffect) {
    match s.mask_effects.iter_mut().find(|x| x.name.eq_ignore_ascii_case(&e.name)) {
        Some(x) => *x = e,
        None => s.mask_effects.push(e),
    }
}

fn name_param(p: &Value, c: &str) -> Result<String> {
    str_param(p, "name").map(str::trim).filter(|n| !n.is_empty()).map(str::to_string).ok_or_else(|| bad(c, "missing `name`"))
}

/// The selected mask's adjustments on the active photo.
fn selected_adjust(s: &Session, p: &Value, c: &str) -> Result<(u32, LocalAdjustments)> {
    let id = s.active().ok_or_else(|| bad(c, "no active photo"))?;
    let mid = p.get("id").and_then(Value::as_u64).map(|v| v as u32).or(s.active_mask).ok_or_else(|| bad(c, "no mask selected (give `id`)"))?;
    let d = s.develop_of(id).unwrap_or_default();
    let m = d.masks.iter().find(|m| m.id == mid).ok_or_else(|| bad(c, format!("no mask {mid}")))?;
    Ok((mid, m.adjust))
}

fn list(s: &Session) -> Value {
    let current = s
        .active()
        .and_then(|id| s.develop_of(id))
        .and_then(|d| s.active_mask.and_then(|mid| d.masks.iter().find(|m| m.id == mid).map(|m| m.adjust)))
        .and_then(|a| matching_effect(s, &a));
    json!({"effects": s.mask_effects, "current": current})
}

fn apply_effect(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "mask.applyEffect";
    let name = name_param(p, C)?;
    let effect = s.mask_effects.iter().find(|x| x.name.eq_ignore_ascii_case(&name)).cloned().ok_or_else(|| bad(C, format!("no effect `{name}`")))?;
    let (mid, _) = selected_adjust(s, p, C)?;
    let id = s.active().ok_or_else(|| bad(C, "no active photo"))?;
    let mut d = (*s.develop_of(id).unwrap_or_default()).clone();
    let m = d.masks.iter_mut().find(|m| m.id == mid).ok_or_else(|| bad(C, format!("no mask {mid}")))?;
    m.adjust = super::masks::clamp_local(effect.apply(&m.adjust));
    s.set_develop(id, d, &format!("Mask Effect: {}", effect.name))?;
    Ok(json!({"name": effect.name, "mask": mid}))
}

fn save_effect(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "mask.saveEffect";
    let name = name_param(p, C)?;
    let (_, adjust) = selected_adjust(s, p, C)?;
    upsert(s, MaskEffect { name, adjust: LocalAdjustments { amount: 100.0, ..adjust } });
    s.save_prefs()?;
    Ok(list(s))
}

fn delete_effect(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "mask.deleteEffect";
    let name = name_param(p, C)?;
    let n = s.mask_effects.len();
    s.mask_effects.retain(|x| !x.name.eq_ignore_ascii_case(&name));
    if n == s.mask_effects.len() {
        return Err(bad(C, format!("no effect `{name}`")));
    }
    s.save_prefs()?;
    Ok(list(s))
}

/// Is `text` a Lightroom local adjustment preset (`.lrtemplate`)?
pub fn is_lr_local_template(text: &str) -> bool {
    parse_lua(text.trim_start_matches('\u{feff}'))
        .ok()
        .and_then(|root| root.get("type").and_then(Lua::str).map(|t| t == LR_LOCAL_TYPE))
        .unwrap_or(false)
}

/// A Lightroom local adjustment preset → an effect named after its title (else `stem`), and the
/// non-zero sliders we have no equivalent for (e.g. `toningLuminance`, `grain`).
pub fn parse_lr_local_template(text: &str, stem: &str) -> std::result::Result<(MaskEffect, Vec<String>), String> {
    let root = parse_lua(text.trim_start_matches('\u{feff}'))?;
    if root.get("type").and_then(Lua::str) != Some(LR_LOCAL_TYPE) {
        return Err("not a local adjustment preset".into());
    }
    let Some(Lua::Table(_, fields)) = root.get("value") else { return Err("no adjustments in this local preset".into()) };
    // `exposure2012` → `LocalExposure2012`: the template's keys are the XMP `Local*` names, lower camel case
    let crs = |k: &str| {
        let mut c = k.chars();
        c.next().map(|f| format!("Local{}{}", f.to_ascii_uppercase(), c.as_str())).unwrap_or_default()
    };
    let num = |k: &str| fields.iter().find(|(f, _)| crs(f) == k).and_then(|(_, v)| if let Lua::Num(n) = v { Some(*n) } else { None });
    let values = crate::crs_masks::local_values(num);
    let unmapped: Vec<String> = fields
        .iter()
        .filter(|(k, v)| matches!(v, Lua::Num(n) if n.abs() > 1e-9) && !crate::crs_masks::is_local_field(&crs(k)))
        // a colour tint's hue without saturation does nothing
        .filter(|(k, _)| k != "toningHue")
        .map(|(k, _)| k.clone())
        .collect();
    let adjust: LocalAdjustments = serde_json::from_value(Value::Object(values)).map_err(|e| format!("unreadable local preset: {e}"))?;
    let title = root.get("title").or_else(|| root.get("internalName")).and_then(Lua::str).map(str::trim).filter(|t| !t.is_empty());
    let name =
        title.map(|t| t.strip_prefix("$$$/").and_then(|r| r.split_once('=')).map_or(t, |(_, d)| d).to_string()).unwrap_or_else(|| stem.to_string());
    Ok((MaskEffect { name, adjust: super::masks::clamp_local(LocalAdjustments { amount: 100.0, ..adjust }) }, unmapped))
}

/// Effects in an `.lceffect` file: `{format, version, effects: [...]}`, a bare array, or one effect.
pub fn parse_file(bytes: &[u8]) -> std::result::Result<Vec<MaskEffect>, String> {
    let v: Value = serde_json::from_slice(bytes).map_err(|e| format!("not an effect preset file: {e}"))?;
    let items = match &v {
        Value::Object(o) if o.contains_key("effects") => o["effects"].clone(),
        Value::Array(_) => v.clone(),
        Value::Object(_) => Value::Array(vec![v.clone()]),
        _ => return Err("not an effect preset file".into()),
    };
    let list: Vec<MaskEffect> = serde_json::from_value(items).map_err(|e| format!("not an effect preset file: {e}"))?;
    let list: Vec<MaskEffect> = list
        .into_iter()
        .filter(|e| !e.name.trim().is_empty())
        .map(|e| MaskEffect { name: e.name.trim().to_string(), adjust: super::masks::clamp_local(LocalAdjustments { amount: 100.0, ..e.adjust }) })
        .collect();
    if list.is_empty() {
        return Err("no effect presets in the file".into());
    }
    Ok(list)
}

/// Effects in file `path` (`.lrtemplate` or `.lceffect`) with the sliders each one couldn't keep.
pub fn read_file(path: &str) -> std::result::Result<Vec<(MaskEffect, Vec<String>)>, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let p = std::path::Path::new(path);
    let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "Effect".into());
    match p.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).as_deref() {
        Some("lrtemplate") => parse_lr_local_template(&String::from_utf8_lossy(&bytes), &stem).map(|e| vec![e]),
        _ => parse_file(&bytes).map(|v| v.into_iter().map(|e| (e, Vec::new())).collect()),
    }
}

/// Add `effects` (same-named ones replaced) and save → the import report rows.
pub fn add_effects(s: &mut Session, effects: Vec<(MaskEffect, Vec<String>, String)>) -> Result<Vec<Value>> {
    let rows: Vec<Value> = effects.iter().map(|(e, unmapped, file)| json!({"name": e.name, "file": file, "unmapped": unmapped})).collect();
    let any = !effects.is_empty();
    for (e, _, _) in effects {
        upsert(s, e);
    }
    if any {
        s.save_prefs()?;
    }
    Ok(rows)
}

/// The files under `paths` (folders walked, at most 8 levels) an effect import reads.
fn effect_files(paths: &[String]) -> Vec<String> {
    fn walk(dir: &std::path::Path, depth: usize, out: &mut Vec<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                if depth < 8 {
                    walk(&p, depth + 1, out);
                }
            } else if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("lrtemplate") || e.eq_ignore_ascii_case(EFFECT_EXT)) {
                out.push(p.to_string_lossy().to_string());
            }
        }
    }
    let mut out = Vec::new();
    for x in paths {
        let p = std::path::Path::new(x);
        if p.is_dir() {
            walk(p, 0, &mut out);
        } else {
            out.push(x.clone());
        }
    }
    out
}

fn import_effects(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "mask.importEffects";
    let mut paths: Vec<String> =
        p.get("paths").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
    if let Some(x) = str_param(p, "path") {
        paths.push(x.to_string());
    }
    if paths.is_empty() {
        return Err(bad(C, "missing `path` / `paths`"));
    }
    let (mut ok, mut failed) = (Vec::new(), Vec::new());
    for f in effect_files(&paths) {
        match read_file(&f) {
            Ok(list) => ok.extend(list.into_iter().map(|(e, u)| (e, u, f.clone()))),
            Err(e) => failed.push(json!([f, e])),
        }
    }
    let imported = add_effects(s, ok)?;
    Ok(json!({"imported": imported, "failed": failed}))
}

fn export_effects(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "mask.exportEffects";
    let path = str_param(p, "path").filter(|x| !x.trim().is_empty()).ok_or_else(|| bad(C, "missing `path`"))?;
    let mut path = std::path::PathBuf::from(path);
    if path.extension().is_none() {
        path.set_extension(EFFECT_EXT);
    }
    let names: Option<Vec<String>> =
        p.get("names").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect());
    let chosen: Vec<&MaskEffect> =
        s.mask_effects.iter().filter(|e| names.as_ref().is_none_or(|n| n.iter().any(|x| x.eq_ignore_ascii_case(&e.name)))).collect();
    if chosen.is_empty() {
        return Err(bad(C, "no effect presets to export"));
    }
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| crate::EngineError::Other(format!("{}: {e}", dir.display())))?;
    }
    let text = serde_json::to_string_pretty(&json!({"format": FILE_FORMAT, "version": 1, "effects": chosen})).unwrap_or_default();
    std::fs::write(&path, text).map_err(|e| crate::EngineError::Other(format!("{}: {e}", path.display())))?;
    Ok(json!({"path": path.display().to_string(), "count": chosen.len()}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            query "mask.effects",
            "Mask Effect Presets",
            [],
            None,
            "{} → {effects: [{name, adjust: {exposure, contrast, …}}], current: name of the effect the selected mask's sliders match, or null}",
            always,
            |s, _| Ok(list(s))
        ),
        cmd!(
            "mask.applyEffect",
            "Apply Mask Effect",
            [],
            None,
            "{name, id?} — sets every slider of the selected mask (or mask `id`) from the effect preset; Amount stays",
            has_active,
            apply_effect
        ),
        cmd!(
            "mask.saveEffect",
            "Save Mask Effect",
            [],
            None,
            "{name, id?} — the selected mask's sliders as an effect preset (replaces one of that name), saved with the library",
            has_active,
            save_effect
        ),
        cmd!("mask.deleteEffect", "Delete Mask Effect", [], None, "{name} — removes an effect preset", always, delete_effect),
        cmd!(
            "mask.importEffects",
            "Import Mask Effects",
            [],
            None,
            "{path | paths: [file or folder]} — Lightroom local adjustment presets (.lrtemplate) and .lceffect files; same-named effects are replaced → {imported: [{name, file, unmapped}], failed: [[file, error]]}",
            always,
            import_effects
        ),
        cmd!(
            "mask.exportEffects",
            "Export Mask Effects",
            [],
            None,
            "{path, names?: [name] (default: all)} — writes an .lceffect JSON file → {path, count}",
            always,
            export_effects
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    // A synthetic template in the shape Lightroom writes (our own values).
    const TEMPLATE: &str = r#"s = {
	id = "00000000-0000-0000-0000-000000000001",
	internalName = "Soft Glow",
	title = "Soft Glow",
	type = "LocalizedAdjustmentPreset",
	value = {
		clarity = 0.5,
		clarity2012 = -0.25,
		exposure = 0,
		exposure2012 = 0.125,
		highlights2012 = -0.5,
		temperature = 0.2,
		texture = -0.3,
		toningHue = 240,
		toningLuminance = 0.1,
		toningSaturation = 0,
	},
	version = 0,
}"#;

    #[test]
    fn reads_a_lightroom_local_template() {
        assert!(is_lr_local_template(TEMPLATE));
        let (e, unmapped) = parse_lr_local_template(TEMPLATE, "file").unwrap();
        assert_eq!(e.name, "Soft Glow");
        assert!((e.adjust.exposure - 0.5).abs() < 1e-9, "exposure2012 is a quarter of the stops");
        assert!((e.adjust.clarity + 25.0).abs() < 1e-9, "the 2012 field wins over the old one");
        assert!((e.adjust.highlights + 50.0).abs() < 1e-9);
        assert!((e.adjust.temp - 20.0).abs() < 1e-9);
        assert!((e.adjust.texture + 30.0).abs() < 1e-9);
        assert!((e.adjust.amount - 100.0).abs() < 1e-9);
        assert_eq!(unmapped, vec!["toningLuminance".to_string()]);
    }

    #[test]
    fn develop_templates_are_not_local_ones() {
        let develop = r#"s = { title = "X", type = "Develop", value = { settings = { Exposure2012 = 1 } } }"#;
        assert!(!is_lr_local_template(develop));
        assert!(parse_lr_local_template(develop, "x").is_err());
        assert!(parse_lr_local_template("garbage {", "x").is_err());
        assert!(parse_lr_local_template(r#"s = { type = "LocalizedAdjustmentPreset", value = 3 }"#, "x").is_err());
    }

    #[test]
    fn apply_keeps_amount_and_matches_ignore_it() {
        let e = MaskEffect { name: "E".into(), adjust: LocalAdjustments { exposure: 1.0, ..Default::default() } };
        let current = LocalAdjustments { shadows: 40.0, amount: 60.0, ..Default::default() };
        let out = e.apply(&current);
        assert_eq!((out.exposure, out.shadows, out.amount), (1.0, 0.0, 60.0));
        assert!(e.matches(&out));
        assert!(!e.matches(&current));
    }
}
