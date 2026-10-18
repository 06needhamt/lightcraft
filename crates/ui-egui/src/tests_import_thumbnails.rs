//! Import review sizes change the real grid and its preview resolution, preserving culling.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::import::ImportDialog;
use crate::render::Slot;
use crate::state::{Dialog, UiState};
use crate::{LightcraftApp, Services};
use lightcraft_engine::import::ImportCandidate;

const T: Duration = Duration::from_secs(30);

struct Review {
    h: Headless,
    dir: std::path::PathBuf,
}

impl Review {
    fn new(scale: f32) -> Self {
        let dir = std::env::temp_dir().join(format!("lc-import-thumbnails-{}-{}", std::process::id(), lightcraft_preview::next_tick()));
        std::fs::create_dir_all(&dir).unwrap();
        let image = lightcraft_raster::Rgba8 {
            width: 1600,
            height: 1200,
            data: (0..1600 * 1200).map(|i| [((i % 1600) / 7) as u8, ((i / 1600) / 5) as u8, 100, 255]).collect(),
        };
        let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&image), &Default::default()).unwrap();
        let path = dir.join("original.png");
        std::fs::write(&path, &bytes).unwrap();
        let mut candidates: Vec<_> = (0..60)
            .map(|i| ImportCandidate {
                path: path.to_string_lossy().into(),
                name: format!("a-long-photo-name-{i:02}.png"),
                format: "PNG".into(),
                width: 1600,
                height: 1200,
                file_size: bytes.len() as u64,
                ..Default::default()
            })
            .collect();
        candidates[1].duplicate = Some("content".into());
        candidates[2].error = Some("Unreadable test candidate".into());
        let mut app = LightcraftApp::new(lightcraft_engine::Session::new().with_fs(), Services::default());
        app.ui.dialog = Some(Dialog::Import { opts: Box::new(ImportDialog::new(candidates)) });
        let mut h = Headless::new(app, [1366.0, 1000.0], scale);
        assert!(h.settle(T));
        Self { h, dir }
    }

    fn rect(&self, id: &str) -> egui::Rect {
        self.h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("missing {id}"))
    }

    fn checks(&self) -> Vec<bool> {
        let Some(Dialog::Import { opts }) = &self.h.app.ui.dialog else { panic!("import review closed") };
        opts.checked.clone()
    }

    fn request(&mut self, method: &str, params: serde_json::Value) {
        let response = self.h.request(method, params, T);
        assert_eq!(response["ok"], true, "{method}: {response}");
    }

    fn size(&mut self, size: f32) {
        self.request("engine.execute", json!({"command": "view.importThumbSize", "params": {"size": size}}));
        assert!(self.h.settle(T));
    }
}

impl Drop for Review {
    fn drop(&mut self) {
        self.h.app.ui.dialog = None;
        self.h.app.renderer.forget_imports();
        self.h.settle(T);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn import_thumbnail_slider_reflows_without_changing_selection_or_badges() {
    let mut review = Review::new(1.0);
    assert_eq!(review.rect("import:0").width(), 116.0);
    review.request("ui.clickWidget", json!({"id": "import:0"}));
    let checked = review.checks();
    assert!(!checked[0] && !checked[1] && !checked[2]);
    // Real pointer input on the slider, including its end stops.
    review.request("ui.clickWidget", json!({"id": "slider:importThumbSize", "fx": 0.0}));
    assert!(review.h.settle(T));
    assert_eq!(review.rect("import:0").width(), 80.0);
    let slider = review.rect("slider:importThumbSize");
    review.request("ui.dragWidget", json!({"id": "slider:importThumbSize", "toX": slider.right(), "toY": slider.center().y}));
    assert!(review.h.settle(T));
    assert_eq!(review.rect("import:0").width(), 320.0);
    assert_eq!(review.checks(), checked);
    let text = review.h.painted_text().join("\n");
    assert!(text.contains("Duplicate") && text.contains("Unreadable"), "badges remain visible: {text}");
    assert!(review.rect("import:3").top() > review.rect("import:0").top(), "larger tiles wrap into more rows");
    review.size(80.0);
    assert_eq!(review.rect("import:3").top(), review.rect("import:0").top());
    assert_eq!(review.checks(), checked);
    // The preference survives closing/reopening the review and a settings round trip.
    let saved = serde_json::to_value(&review.h.app.ui).unwrap();
    let restored: UiState = serde_json::from_value(saved).unwrap();
    assert_eq!(restored.sanitized().import_thumb_size, 80.0);
}

#[test]
fn import_thumbnail_previews_upgrade_for_density_and_stay_bounded_when_scrolling() {
    let mut review = Review::new(2.0);
    assert_eq!(review.h.app.renderer.textures[&Slot::Import(0)].size, [384, 288]);
    review.size(320.0);
    assert_eq!(review.h.app.renderer.textures[&Slot::Import(0)].size, [768, 576]);
    // Shrinking keeps the sharp preview instead of replacing it with a smaller decode.
    let sharp = review.h.app.renderer.textures[&Slot::Import(0)].key;
    review.size(116.0);
    assert_eq!(review.h.app.renderer.textures[&Slot::Import(0)].key, sharp);
    review.size(320.0);
    // Scrolling a large card retains only the nearby tiles and drops obsolete requests.
    review.request("ui.hoverWidget", json!({"id": "import:0"}));
    review.request("ui.scroll", json!({"dy": -20000.0}));
    assert!(review.h.settle(T));
    assert!(!review.h.app.renderer.textures.contains_key(&Slot::Import(0)));
    let imports: Vec<_> = review.h.app.renderer.textures.keys().filter_map(|s| if let Slot::Import(i) = s { Some(*i) } else { None }).collect();
    assert!(!imports.is_empty() && imports.len() < 20, "bounded visible previews: {imports:?}");
    for i in imports {
        assert!(review.h.app.widgets.iter().any(|(w, _)| w == &format!("import:{i}")));
    }
}

#[test]
fn import_thumbnail_size_rejects_bad_commands_and_restores_old_preferences() {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), Services::default());
    assert_eq!(app.run("view.importThumbSize", json!({})).unwrap()["size"], 116.0);
    for bad in [json!("large"), json!(null), json!([120]), json!(1e300)] {
        assert!(app.run("view.importThumbSize", json!({"size": bad})).is_err());
        assert_eq!(app.ui.import_thumb_size, 116.0);
    }
    assert_eq!(app.run("view.importThumbSize", json!({"size": -10})).unwrap()["size"], 80.0);
    assert_eq!(app.run("view.importThumbSize", json!({"size": 1e6})).unwrap()["size"], 320.0);
    let legacy: UiState = serde_json::from_value(json!({})).unwrap();
    assert_eq!(legacy.import_thumb_size, 116.0);
    for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let state = UiState { import_thumb_size: bad, ..Default::default() }.sanitized();
        assert_eq!(state.import_thumb_size, 116.0);
    }
}
