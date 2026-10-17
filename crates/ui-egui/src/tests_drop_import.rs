//! Files dragged from the file manager onto the window: while they hover, the window says what
//! dropping will do; dropping photos or folders opens the import review (unless Settings ▸ Import
//! says to import dropped files directly); a drop while a dialog is open is refused.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// A scratch folder that goes away with the test, however it ends.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("lc-ui-drop-import-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
    fn path(&self, rel: &str) -> String {
        self.0.join(rel).to_string_lossy().to_string()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_png(path: &str, seed: u8) {
    let (w, h) = (24usize, 16usize);
    let data: Vec<[u8; 4]> = (0..w * h).map(|i| [(i % w * 9) as u8, (i / w * 13) as u8, seed, 255]).collect();
    let img = lightcraft_raster::Rgba8 { width: w, height: h, data };
    let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    std::fs::create_dir_all(std::path::Path::new(path).parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// An empty library, a folder `trip` with two photos, a window big enough for the overlay.
fn app(dir: &Scratch) -> Headless {
    write_png(&dir.path("trip/a.png"), 1);
    write_png(&dir.path("trip/b.png"), 2);
    let session = lightcraft_engine::Session::new().with_fs();
    let mut h = Headless::new(LightcraftApp::new(session, Services { png: None, ..Default::default() }), [1300.0, 900.0], 1.0);
    for _ in 0..4 {
        h.step();
    }
    h
}

fn drop_files(h: &mut Headless, paths: &[String], hover: bool) {
    let r = h.request("ui.dropFiles", json!({"paths": paths, "hover": hover}), T);
    assert_eq!(r["ok"], true, "{r}");
    for _ in 0..3 {
        h.step();
    }
}

fn overlay(h: &Headless) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == "dropOverlay")
}

fn said(h: &Headless, word: &str) -> bool {
    h.painted_text().iter().any(|t| t.contains(word))
}

fn photos(h: &Headless) -> usize {
    h.app.session.catalog.photos().count()
}

#[test]
fn hovering_photos_says_they_will_be_reviewed_and_lets_go_when_they_leave() {
    let dir = Scratch::new("hover");
    let mut h = app(&dir);
    assert!(!overlay(&h), "nothing is dragged over the window");
    drop_files(&mut h, &[dir.path("trip/a.png"), dir.path("trip/b.png")], true);
    assert!(overlay(&h), "the window shows where to drop");
    assert!(said(&h, "review 2 items"), "{:?}", h.painted_text());
    drop_files(&mut h, &[], true);
    assert!(!overlay(&h), "the files left the window");
}

#[test]
fn hovering_presets_says_they_will_be_imported_as_presets() {
    let dir = Scratch::new("hover-presets");
    let mut h = app(&dir);
    std::fs::write(dir.path("warm.lcpreset"), "{}").unwrap();
    drop_files(&mut h, &[dir.path("warm.lcpreset")], true);
    assert!(overlay(&h));
    assert!(said(&h, "1 preset"), "{:?}", h.painted_text());
    assert!(!said(&h, "review"), "presets are not reviewed");
}

#[test]
fn hovering_unknown_files_says_only_that_dropping_imports() {
    let dir = Scratch::new("hover-unknown");
    let mut h = app(&dir);
    drop_files(&mut h, &[String::new()], true);
    assert!(overlay(&h));
    assert!(said(&h, "Drop to import"), "{:?}", h.painted_text());
}

#[test]
fn dropping_a_folder_opens_the_import_review_and_imports_nothing_yet() {
    let dir = Scratch::new("review");
    let mut h = app(&dir);
    drop_files(&mut h, &[dir.path("trip")], false);
    assert!(h.step_until(SETTLE, |h| matches!(h.app.ui.dialog, Some(Dialog::Import { .. }))), "the review opens");
    let Some(Dialog::Import { opts }) = &h.app.ui.dialog else { panic!("no import review") };
    assert_eq!(opts.candidates.len(), 2);
    assert_eq!(photos(&h), 0, "nothing is imported until the review is confirmed");
    assert!(!overlay(&h), "the files were let go");
}

#[test]
fn dropping_imports_directly_when_settings_say_so() {
    let dir = Scratch::new("direct");
    let mut h = app(&dir);
    h.app.ui.settings.import_dropped_directly = true;
    drop_files(&mut h, &[dir.path("trip")], false);
    assert!(h.step_until(SETTLE, |h| photos(h) == 2 && h.app.import.is_none()), "both photos arrive");
    assert!(h.app.ui.dialog.is_none(), "no review");
}

#[test]
fn a_drop_while_a_dialog_is_open_leaves_the_dialog_and_imports_nothing() {
    let dir = Scratch::new("dialog");
    let mut h = app(&dir);
    h.app.ui.dialog = Some(Dialog::WhatsNew);
    drop_files(&mut h, &[dir.path("trip/a.png")], true);
    assert!(overlay(&h), "the window still answers the hover");
    assert!(said(&h, "Close the open dialog"), "{:?}", h.painted_text());
    drop_files(&mut h, &[dir.path("trip")], false);
    for _ in 0..30 {
        h.step();
    }
    assert!(matches!(h.app.ui.dialog, Some(Dialog::WhatsNew)), "the open dialog stays");
    assert!(h.app.scan.is_none() && h.app.import.is_none(), "no scan or import started");
    assert_eq!(photos(&h), 0);
    assert!(h.app.ui.toast.is_some(), "the person is told why");
}

/// The scan of a dropped folder finishing while another dialog is open (the face model's licence, Settings…)
/// keeps that dialog; the review opens, with the scan's photos, once it is closed.
#[test]
fn a_finished_scan_waits_for_the_open_dialog_before_opening_the_review() {
    let dir = Scratch::new("waits");
    let mut h = app(&dir);
    crate::import::open(&mut h.app, vec![dir.path("trip")]).unwrap();
    h.app.ui.dialog = Some(Dialog::WhatsNew);
    assert!(h.step_until(SETTLE, |h| h.app.scan.is_none()), "the scan ends");
    for _ in 0..10 {
        h.step();
    }
    assert!(matches!(h.app.ui.dialog, Some(Dialog::WhatsNew)), "the dialog the person had open stays");
    h.app.ui.dialog = None;
    assert!(h.step_until(T, |h| matches!(h.app.ui.dialog, Some(Dialog::Import { .. }))), "the review opens once the dialog is closed");
    let Some(Dialog::Import { opts }) = &h.app.ui.dialog else { panic!("no import review") };
    assert_eq!(opts.candidates.len(), 2, "the scan's photos were kept");
}

/// A drop while an import runs would open a review whose OK replaces the running import: it is
/// refused instead, and the card says so.
#[test]
fn a_drop_while_an_import_runs_is_refused() {
    let dir = Scratch::new("running");
    let mut h = app(&dir);
    write_png(&dir.path("more/c.png"), 3);
    let _scan = crate::import::stall_scan(&mut h.app);
    drop_files(&mut h, &[dir.path("more/c.png")], true);
    assert!(said(&h, "An import is running"), "{:?}", h.painted_text());
    assert!(!said(&h, "review"), "{:?}", h.painted_text());
    drop_files(&mut h, &[dir.path("more")], false);
    for _ in 0..30 {
        h.step();
    }
    assert!(h.app.ui.dialog.is_none(), "no review opened");
    assert!(h.app.scan.is_some() && h.app.import.is_none(), "the running scan was left alone and nothing else started");
    assert!(h.app.ui.toast.is_some(), "the person is told why");
    assert_eq!(photos(&h), 0);
}

/// An import that is running stays the one that runs: a direct drop doesn't start a second.
#[test]
fn a_direct_drop_while_an_import_runs_is_refused_too() {
    let dir = Scratch::new("running-direct");
    let mut h = app(&dir);
    h.app.ui.settings.import_dropped_directly = true;
    write_png(&dir.path("more/c.png"), 3);
    let _scan = crate::import::stall_scan(&mut h.app);
    drop_files(&mut h, &[dir.path("more")], false);
    assert!(h.app.import.is_none(), "no second import");
    assert!(h.app.ui.toast.is_some());
}
