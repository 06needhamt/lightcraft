//! A dialog's confirm button names what it does ("Rename", "Create", "Stack"…), not "OK": the
//! button is the last thing read before a change is made.

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};
use serde_json::json;
use std::time::Duration;

const T: Duration = Duration::from_secs(20);

fn demo() -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1300.0, 1000.0], 1.0);
    h.settle(T);
    h
}

fn show(h: &mut Headless, dialog: Dialog) -> Vec<String> {
    h.app.ui.dialog = Some(dialog);
    h.step();
    h.step();
    h.painted_text()
}

fn dialog(value: serde_json::Value) -> Dialog {
    serde_json::from_value(value).unwrap_or_else(|e| panic!("bad dialog json: {e}"))
}

/// Dialogs whose confirm may stay "OK" because nothing more specific is true of them: none today.
/// (A one-field prompt opened with a command the app doesn't know has no verb to name.)
const KEEPS_OK: &[&str] = &[];

#[test]
fn confirm_buttons_name_their_action() {
    let mut h = demo();
    let cases: Vec<(&str, serde_json::Value, &str)> = vec![
        ("new album", json!({"kind": "newAlbum", "name": "", "folder": false}), "Create"),
        ("new folder", json!({"kind": "newAlbum", "name": "", "folder": true}), "Create"),
        ("rename album", json!({"kind": "renameAlbum", "id": 1, "name": "A"}), "Rename"),
        ("new smart album", json!({"kind": "newSmartAlbum", "name": ""}), "Create"),
        ("auto-stack", json!({"kind": "autoStack", "gap": 60.0}), "Stack"),
        ("capture time", json!({"kind": "captureTime", "mode": "set", "time": "", "days": 0, "hours": 0, "minutes": 0, "zone": 0.0}), "Change"),
        ("label names", json!({"kind": "labelNames", "names": [], "save_as": ""}), "Save"),
        ("rename photos", json!({"kind": "rename", "template": "{name}", "start": 1}), "Rename"),
        ("rename keyword", json!({"kind": "renameKeyword", "from": "a", "to": "b"}), "Rename"),
        ("merge keywords", json!({"kind": "mergeKeywords", "from": ["a"], "into": "b"}), "Merge Keywords"),
        ("assisted culling", json!({"kind": "cull", "reject_below": 0.0, "pick_best": true, "burst_gap": 10.0, "similarity": 0.93}), "Cull"),
        ("new preset", json!({"kind": "createPreset", "name": "", "group": ""}), "Create"),
        ("copy settings", json!({"kind": "copySettings", "groups": []}), "Copy"),
        ("paste settings", json!({"kind": "pasteSettings", "groups": []}), "Paste"),
    ];
    for (what, value, want) in cases {
        let text = show(&mut h, dialog(value));
        assert!(text.iter().any(|t| t == want), "{what}: the confirm button reads {want:?}: {text:?}");
        assert!(KEEPS_OK.contains(&what) || !text.iter().any(|t| t == "OK"), "{what}: the confirm button still reads OK: {text:?}");
    }
}

#[test]
fn one_field_prompts_name_what_they_do() {
    let mut h = demo();
    let prompts = [
        ("album.rename", "Rename"),
        ("folder.rename", "Rename"),
        ("folder.create", "Create"),
        ("version.rename", "Rename"),
        ("preset.rename", "Rename"),
        ("mask.renameEffect", "Rename"),
        ("mask.setEffectGroup", "Create"),
        ("preset.move", "Move"),
        ("app.newLibrary", "Create"),
        ("keyword.saveSet", "Save"),
        ("filter.savePreset", "Save"),
        ("curve.savePreset", "Save"),
        ("metadata.savePreset", "Save"),
        ("photo.tagFromTracklog", "Add Locations"),
    ];
    for (command, want) in prompts {
        let prompt = Dialog::TextPrompt {
            title: "Title".into(),
            hint: "Hint".into(),
            value: String::new(),
            command: command.into(),
            params: json!({}),
            key: "name".into(),
        };
        let text = show(&mut h, prompt);
        assert!(text.iter().any(|t| t == want), "{command}: the confirm button reads {want:?}: {text:?}");
        assert!(!text.iter().any(|t| t == "OK"), "{command}: no OK: {text:?}");
    }
}

#[test]
fn export_confirm_says_how_many_photos() {
    let mut h = demo();
    h.app.run("dialog.export", json!({})).unwrap();
    h.step();
    h.step();
    let one = h.painted_text();
    let n = h.app.session.selection.ids.len().max(1);
    let want = if n == 1 { "Export 1 Photo".to_string() } else { format!("Export {n} Photos") };
    assert!(one.contains(&want), "reads {want:?}: {one:?}");
    assert!(!one.iter().any(|t| t == "OK"));
}

#[test]
fn counts_agree_with_their_nouns() {
    let mut h = demo();
    let ids: Vec<u64> = h.app.session.catalog.photos().map(|p| p.id.0).take(2).collect();
    let rows = |h: &mut Headless, n: usize| {
        h.app.session.selection.ids = ids.iter().take(n).map(|i| lightcraft_catalog::PhotoId(*i)).collect();
        let text = show(
            h,
            dialog(json!({"kind": "contactSheet", "options": serde_json::to_value(lightcraft_engine::contact_sheet::Options::default()).unwrap()})),
        );
        text.into_iter().find(|t| t.contains("selected photo") || t.contains("1 photo")).unwrap_or_default()
    };
    let one = rows(&mut h, 1);
    assert!(one.starts_with("1 selected photo ·") && one.ends_with("1 page"), "{one:?}");
    let two = rows(&mut h, 2);
    assert!(two.starts_with("2 selected photos ·") && two.ends_with("1 page"), "{two:?}");
}
