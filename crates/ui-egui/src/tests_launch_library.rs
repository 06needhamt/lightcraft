//! Issue #1290: what opens at launch. Settings ▸ General ▸ At launch opens the last library or
//! asks with a window listing the recent ones (greyed out with the reason when they can't be
//! opened); Alt/Option held as the window appears asks once; one recent entry can be forgotten from
//! File ▸ Open Recent Library, the window and `app.recentLibraries {remove}`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::panels::library_problem::{ALT_SECONDS, Launch, LibraryProblem, launch_plan};
use crate::state::{AppSettings, LaunchLibrary};
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

/// A fresh scratch folder for one test.
fn temp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lc-ui-launch-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A library on disk at `dir` (written, then closed: as left by an earlier run).
fn make_library(dir: &Path) -> String {
    let mut s = lightcraft_engine::Session::new().with_fs();
    s.open_library(dir, false).unwrap();
    s.close_library().unwrap();
    drop(s);
    assert!(lightcraft_engine::library::is_library(dir));
    dir.to_string_lossy().into_owned()
}

/// Enough frames for a synthetic click (move, press, release) and what it starts.
fn settle(h: &mut Headless) {
    for _ in 0..6 {
        h.step();
    }
}

/// The widget `id` was drawn in the last frame.
fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

/// `Some(s)` as a path, for the launch plan's arguments.
fn path(s: &str) -> Option<PathBuf> {
    Some(PathBuf::from(s))
}

/// `--library`, then `LIGHTCRAFT_LIBRARY`, then the setting; `--memory` never asks.
#[test]
fn the_launch_plan_puts_the_flag_and_the_variable_before_the_setting() {
    let ask = AppSettings { at_launch: LaunchLibrary::Ask, library_path: "/last".into(), ..AppSettings::default() };
    let last = AppSettings { library_path: "/last".into(), recent_libraries: vec!["/last".into(), "/older".into()], ..AppSettings::default() };
    let fresh = AppSettings::default();
    let open = |dir: &str, alt_asks| Launch::Open { dir: path(dir), alt_asks };
    // --library, then LIGHTCRAFT_LIBRARY, whatever the setting says; neither asks on Alt
    for s in [None, Some(&ask), Some(&last)] {
        assert_eq!(launch_plan(false, path("/flag"), path("/env"), s, path("/default")), open("/flag", false));
        assert_eq!(launch_plan(false, None, path("/env"), s, path("/default")), open("/env", false));
    }
    // Ask me asks, with or without a recent list
    assert_eq!(launch_plan(false, None, None, Some(&ask), path("/default")), Launch::Ask);
    let ask_empty = AppSettings { at_launch: LaunchLibrary::Ask, ..AppSettings::default() };
    assert_eq!(launch_plan(false, None, None, Some(&ask_empty), None), Launch::Ask);
    // Open the last library: the last one, else the default place; Alt can still ask
    assert_eq!(launch_plan(false, None, None, Some(&last), path("/default")), open("/last", true));
    assert_eq!(launch_plan(false, None, None, Some(&fresh), path("/default")), open("/default", true));
    assert_eq!(launch_plan(false, None, None, None, path("/default")), open("/default", true));
    assert_eq!(launch_plan(false, None, None, None, None), Launch::Open { dir: None, alt_asks: true });
    // --memory never asks and opens no folder
    assert_eq!(launch_plan(true, None, None, Some(&ask), path("/default")), Launch::Open { dir: None, alt_asks: false });
    assert_eq!(launch_plan(true, path("/flag"), None, Some(&last), None), Launch::Open { dir: None, alt_asks: false });
}

/// Each listed library is checked when the window opens; the last one opened is selected.
#[test]
fn the_list_checks_each_library_and_selects_the_last_one() {
    let dir = temp("choose");
    let (a, b, held, plain) = (dir.join("A"), dir.join("B"), dir.join("Held"), dir.join("Plain"));
    let (a, b, held_path) = (make_library(&a), make_library(&b), make_library(&held));
    std::fs::create_dir_all(&plain).unwrap();
    std::fs::write(plain.join("notes.txt"), b"not a library").unwrap();
    let gone = dir.join("Gone").to_string_lossy().into_owned();
    let plain = plain.to_string_lossy().into_owned();
    let lock = lightcraft_catalog::LibraryLock::acquire(Path::new(&held_path), "another LightCraft").unwrap();
    let session = lightcraft_engine::Session::new().with_fs();

    let mut settings = AppSettings {
        recent_libraries: vec![gone.clone(), held_path.clone(), a.clone(), plain.clone(), b.clone()],
        library_path: b.clone(),
        ..AppSettings::default()
    };
    settings.recent_opened.insert(b.clone(), "2026-10-20T08:30:00".into());
    let p = LibraryProblem::choose(&session, &settings);
    assert!(!p.saves_nothing(), "the launch list is not a temporary session: removals are saved");
    let c = p.chooser.as_ref().unwrap();
    let why: Vec<Option<&str>> = c.libraries.iter().map(|l| l.unavailable.as_deref()).collect();
    assert!(why[0].unwrap().contains("Not found"), "{why:?}");
    assert!(why[1].unwrap().contains("already open in another LightCraft"), "{why:?}");
    assert_eq!(why[2], None);
    assert!(why[3].unwrap().contains("no LightCraft library"), "{why:?}");
    assert_eq!(why[4], None);
    assert_eq!(c.selected, Some(4), "the last library opened");
    assert_eq!(c.libraries[4].opened.as_deref(), Some("2026-10-20T08:30:00"));
    assert_eq!(c.libraries[2].opened, None, "no time known");
    let json = p.to_json();
    assert_eq!(json["chooser"]["libraries"][1]["available"], false);
    assert_eq!(json["chooser"]["selected"], 4);

    // the last library can't be opened: the first one that can is selected
    settings.library_path = held_path.clone();
    assert_eq!(LibraryProblem::choose(&session, &settings).chooser.unwrap().selected, Some(2));
    // none can: the first row, Open greyed out
    settings.recent_libraries = vec![gone.clone()];
    assert_eq!(LibraryProblem::choose(&session, &settings).chooser.unwrap().selected, Some(0));
    // an empty list selects nothing
    settings.recent_libraries.clear();
    let c = LibraryProblem::choose(&session, &settings).chooser.unwrap();
    assert!(c.libraries.is_empty() && c.selected.is_none());
    assert!(LibraryProblem::new("/x", "e").saves_nothing(), "the failed-library window still saves nothing");
    drop(lock);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The app as the desktop host starts it with *Ask me*: no library open, the list showing.
fn asking(recent: Vec<String>, last: &str, picked: Option<String>) -> Headless {
    let session = lightcraft_engine::Session::new().with_fs();
    // (an encoder for the screenshots LIGHTCRAFT_TEST_SHOTS asks for)
    let png = |img: &lightcraft_raster::Rgba8| {
        lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(img), &lightcraft_codecs::EncodeMeta::default()).unwrap_or_default()
    };
    let services = Services {
        png: Some(Box::new(png)),
        write: Some(Box::new(|p: &str, b: &[u8]| std::fs::write(p, b).map_err(|e| e.to_string()))),
        ..Default::default()
    };
    let mut app = LightcraftApp::new(session, services);
    app.ui.settings.recent_libraries = recent;
    app.ui.settings.library_path = last.to_string();
    app.ui.settings.at_launch = LaunchLibrary::Ask;
    if let Some(p) = picked {
        app.services.pick_folder = Some(Box::new(move || Some(p.clone())));
    }
    app.library_problem = Some(LibraryProblem::choose(&app.session, &app.ui.settings));
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    settle(&mut h);
    h
}

/// The window lists every recent library, greys out a missing one and opens the chosen one.
#[test]
fn the_window_lists_the_libraries_and_opens_the_chosen_one() {
    let dir = temp("window");
    let (a, b) = (make_library(&dir.join("Alpha")), make_library(&dir.join("Beta")));
    let gone = dir.join("Gone").to_string_lossy().into_owned();
    let mut h = asking(vec![a.clone(), gone.clone(), b.clone()], &a, None);
    assert!(h.app.session.library.is_none(), "nothing opened before the choice");
    for id in ["libraryRecent:0", "libraryRecent:1", "libraryRecent:2", "button:libraryOpen", "button:libraryOpenAnother", "button:libraryNew"] {
        assert!(has(&h, id), "{id}");
    }
    assert!(has(&h, "button:libraryQuit") && has(&h, "button:libraryForget"));
    assert!(!has(&h, "button:libraryRetry"), "not the failed-library window");
    if let Some(p) = std::env::var_os("LIGHTCRAFT_TEST_SHOTS") {
        let r = h.request("ui.screenshot", json!({"path": Path::new(&p).join("launch-list.png").to_string_lossy(), "headless": true}), T);
        assert_eq!(r["ok"], true, "{r}");
    }
    let inspect = h.request("ui.inspect", json!({}), T);
    let c = &inspect["result"]["libraryProblem"]["chooser"];
    assert_eq!(c["selected"], 0, "{c}");
    assert_eq!(c["libraries"][1]["available"], false, "{c}");

    // ↓ onto the missing one: Return does nothing there (Open is greyed out)
    assert_eq!(h.request("ui.key", json!({"key": "ArrowDown"}), T)["ok"], true);
    settle(&mut h);
    assert_eq!(h.app.library_problem.as_ref().unwrap().chooser.as_ref().unwrap().selected, Some(1));
    h.request("ui.key", json!({"key": "Enter"}), T);
    settle(&mut h);
    assert!(h.app.session.library.is_none() && h.app.library_problem.is_some());
    // ↓ again, then Open: Beta opens and the window goes away
    h.request("ui.key", json!({"key": "ArrowDown"}), T);
    settle(&mut h);
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:libraryOpen"}), T)["ok"], true);
    settle(&mut h);
    assert_eq!(h.app.session.library.as_ref().map(|l| l.dir.to_string_lossy().into_owned()), Some(b.clone()));
    assert!(h.app.library_problem.is_none());
    assert!(!has(&h, "button:libraryOpen"));
    assert_eq!(h.app.ui.settings.library_path, b);
    drop(h);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Return opens the selection; a library taken meanwhile stays listed with the reason.
#[test]
fn return_opens_the_selected_library_and_a_failure_says_why() {
    let dir = temp("return");
    let (a, b) = (make_library(&dir.join("Alpha")), make_library(&dir.join("Beta")));
    let mut h = asking(vec![a.clone(), b.clone()], &b, None);
    assert_eq!(h.app.library_problem.as_ref().unwrap().chooser.as_ref().unwrap().selected, Some(1));
    // another program takes Beta after the window opened: Open says so and keeps the window
    let lock = lightcraft_catalog::LibraryLock::acquire(Path::new(&b), "another LightCraft").unwrap();
    h.request("ui.key", json!({"key": "Enter"}), T);
    settle(&mut h);
    assert!(h.app.session.library.is_none());
    let p = h.app.library_problem.as_ref().unwrap();
    assert!(p.error.contains("already open"), "{}", p.error);
    assert!(p.chooser.as_ref().unwrap().libraries[1].unavailable.is_some(), "greyed out now");
    drop(lock);
    // ↑ to Alpha, Return: it opens
    h.request("ui.key", json!({"key": "ArrowUp"}), T);
    settle(&mut h);
    h.request("ui.key", json!({"key": "Enter"}), T);
    settle(&mut h);
    assert_eq!(h.app.session.library.as_ref().map(|l| l.dir.to_string_lossy().into_owned()), Some(a));
    assert!(h.app.library_problem.is_none());
    drop(h);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Quit and Esc quit without opening anything.
#[test]
fn quit_and_esc_quit_from_the_window() {
    let dir = temp("quit");
    let a = make_library(&dir.join("Alpha"));
    let mut h = asking(vec![a.clone()], &a, None);
    h.request("ui.key", json!({"key": "Escape"}), T);
    settle(&mut h);
    assert!(h.quit_requested(), "Esc quits when no library is open");
    assert!(h.app.session.library.is_none(), "and opens nothing");
    drop(h);

    let mut h = asking(vec![a.clone()], &a, None);
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:libraryQuit"}), T)["ok"], true);
    settle(&mut h);
    assert!(h.quit_requested());
    assert!(h.app.session.library.is_none());
    drop(h);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Open Another… runs the folder dialog of `app.openLibrary`, also with an empty list.
#[test]
fn open_another_asks_for_a_folder_and_an_empty_list_still_offers_it() {
    let dir = temp("another");
    let other = make_library(&dir.join("Elsewhere"));
    let mut h = asking(Vec::new(), "", Some(other.clone()));
    assert!(has(&h, "indicator:noRecentLibraries"));
    assert!(!has(&h, "libraryRecent:0"));
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:libraryOpenAnother"}), T)["ok"], true);
    settle(&mut h);
    assert_eq!(h.app.session.library.as_ref().map(|l| l.dir.to_string_lossy().into_owned()), Some(other.clone()));
    assert!(h.app.library_problem.is_none());
    assert_eq!(h.app.ui.settings.recent_libraries.first(), Some(&other), "remembered once opened");
    drop(h);

    // without a folder dialog (the browser) Open Another… and New Library… are greyed out
    let mut h = asking(Vec::new(), "", None);
    h.request("ui.clickWidget", json!({"id": "button:libraryOpenAnother"}), T);
    h.request("ui.clickWidget", json!({"id": "button:libraryNew"}), T);
    settle(&mut h);
    assert!(h.app.library_problem.is_some() && h.app.ui.dialog.is_none());
    drop(h);
    let _ = std::fs::remove_dir_all(&dir);
}

/// New Library… asks for a name in front of the window; command-line photos go into it.
#[test]
fn new_library_from_the_window_asks_for_a_name_and_the_list_returns_if_cancelled() {
    let dir = temp("newlib");
    let a = make_library(&dir.join("Alpha"));
    let mut h = asking(vec![a.clone()], &a, Some(dir.to_string_lossy().into_owned()));
    // a photo given on the command line waits for the library chosen
    let img = lightcraft_raster::Rgba8 { width: 8, height: 8, data: vec![[90, 3, 9, 255]; 64] };
    let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    let photo = dir.join("a.png");
    std::fs::write(&photo, png).unwrap();
    h.app.library_problem.as_mut().unwrap().pending_import = vec![photo.to_string_lossy().into_owned()];
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:libraryNew"}), T)["ok"], true);
    settle(&mut h);
    assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::TextPrompt { .. })), "{:?}", h.app.ui.dialog);
    assert!(!has(&h, "button:libraryOpen"), "the name prompt is in front");
    h.request("ui.dialog.cancel", json!({}), T);
    settle(&mut h);
    assert!(has(&h, "button:libraryOpen"), "the list is back");
    // what the prompt's OK runs: the library is made where the folder dialog says, and opens
    h.app.run("app.newLibrary", json!({"name": "Fresh"})).unwrap();
    settle(&mut h);
    assert_eq!(h.app.session.library.as_ref().map(|l| l.dir.clone()), Some(dir.join("Fresh")));
    assert!(h.app.library_problem.is_none());
    h.step_until(T, |h| h.app.import.is_none() && h.app.session.catalog.len() == 1);
    assert_eq!(h.app.session.catalog.len(), 1, "the command-line photo is imported into the new library");
    drop(h);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Remove from List in the window forgets the selected entry and deletes nothing.
#[test]
fn remove_from_list_forgets_one_library_and_deletes_nothing() {
    let dir = temp("forget");
    let (a, b) = (make_library(&dir.join("Alpha")), make_library(&dir.join("Beta")));
    let gone = dir.join("Gone").to_string_lossy().into_owned();
    let mut h = asking(vec![a.clone(), gone.clone(), b.clone()], &a, None);
    h.app.ui.settings.recent_opened.insert(gone.clone(), "2026-01-01T00:00:00".into());
    // the window: the missing one can be selected and removed
    h.request("ui.clickWidget", json!({"id": "libraryRecent:1"}), T);
    settle(&mut h);
    assert_eq!(h.app.library_problem.as_ref().unwrap().chooser.as_ref().unwrap().selected, Some(1));
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:libraryForget"}), T)["ok"], true);
    settle(&mut h);
    assert_eq!(h.app.ui.settings.recent_libraries, [a.clone(), b.clone()]);
    assert!(!h.app.ui.settings.recent_opened.contains_key(&gone), "its time goes too");
    let c = h.app.library_problem.as_ref().unwrap().chooser.clone().unwrap();
    assert_eq!(c.libraries.len(), 2);
    assert_eq!(c.selected, Some(1));
    // removing an existing library leaves its folder and catalog alone
    h.request("ui.clickWidget", json!({"id": "button:libraryForget"}), T);
    settle(&mut h);
    assert_eq!(h.app.ui.settings.recent_libraries, vec![a.clone()]);
    assert!(lightcraft_engine::library::is_library(Path::new(&b)), "nothing is deleted on disk");
    settle(&mut h);
    assert!(!has(&h, "libraryRecent:1"));
    drop(h);
    let _ = std::fs::remove_dir_all(&dir);
}

/// File ▸ Open Recent Library's items.
fn open_recent(app: &LightcraftApp) -> Vec<crate::menubar::MenuNode> {
    use crate::menubar::MenuNode;
    let bar = crate::menubar::menu_bar(app);
    let file = &bar.iter().find(|(t, _)| t == "File").unwrap().1;
    match file.iter().find(|n| matches!(n, MenuNode::Submenu { label, .. } if label == "Open Recent Library")) {
        Some(MenuNode::Submenu { children, .. }) => children.clone(),
        _ => panic!("no Open Recent Library submenu in {file:?}"),
    }
}

/// File ▸ Open Recent Library ▸ Remove from List and `app.recentLibraries {remove}`.
#[test]
fn open_recent_library_and_the_command_remove_one_entry() {
    use crate::menubar::MenuNode;
    let dir = temp("menu");
    let mut session = lightcraft_engine::Session::new().with_fs();
    session.open_library(dir.join("first"), false).unwrap();
    let mut app = LightcraftApp::new(session, Services::default());
    let (first, gone) = (dir.join("first").to_string_lossy().into_owned(), dir.join("gone").to_string_lossy().into_owned());
    app.ui.settings.recent_libraries = vec![first.clone(), gone.clone(), "/elsewhere/third".into()];

    let items = open_recent(&app);
    let Some(MenuNode::Submenu { children, .. }) = items.iter().find(|n| matches!(n, MenuNode::Submenu { label, .. } if label == "Remove from List"))
    else {
        panic!("no Remove from List in {items:?}")
    };
    assert_eq!(children.len(), 3, "every entry, missing ones too");
    let MenuNode::Item { id, params, label, enabled, .. } = &children[1] else { panic!("{children:?}") };
    assert_eq!((id.as_str(), params, *enabled), ("app.recentLibraries", &json!({"remove": gone}), true));
    assert!(label.starts_with("gone — "), "{label}");
    assert_eq!(crate::menubar::display_item_label(id, params, label), label, "a path is never translated");
    // what clicking it runs
    let list = app.run(id, params.clone()).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 2);
    assert_eq!(app.ui.settings.recent_libraries, [first.clone(), "/elsewhere/third".to_string()]);

    // the command: the open library can be forgotten too (it stays open), and nothing is deleted
    let list = app.run("app.recentLibraries", json!({"remove": first})).unwrap();
    assert_eq!(list, json!([{"path": "/elsewhere/third", "name": "third", "exists": false, "open": false, "lastOpened": null}]));
    assert!(app.session.library.is_some() && lightcraft_engine::library::is_library(&dir.join("first")));
    let e = app.run("app.recentLibraries", json!({"remove": first})).unwrap_err();
    assert!(e.contains("not in the recent libraries"), "{e}");
    let e = app.run("app.recentLibraries", json!({"remove": 3})).unwrap_err();
    assert!(e.contains("remove must be"), "{e}");
    app.run("app.recentLibraries", json!({"remove": "/elsewhere/third"})).unwrap();
    // an empty list has nothing to remove
    let items = open_recent(&app);
    assert!(!items.iter().any(|n| matches!(n, MenuNode::Submenu { .. })), "{items:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `atLaunch` and `recentLibrariesOpened` round-trip, and older or odd files still load.
#[test]
fn the_setting_and_the_open_times_are_kept_in_the_settings_file() {
    // files from earlier versions: no key, the default; a value from a later version: the default
    let old: crate::UiState = serde_json::from_str(r#"{"settings": {"recentLibraries": ["/a"], "startupView": "detail"}}"#).unwrap();
    assert_eq!(old.settings.at_launch, LaunchLibrary::Last);
    assert!(old.settings.recent_opened.is_empty());
    assert_eq!(old.settings.recent_libraries, ["/a"]);
    let odd: crate::UiState =
        serde_json::from_str(r#"{"settings": {"atLaunch": "someday", "recentLibrariesOpened": 7, "startupView": "detail"}}"#).unwrap();
    assert_eq!(odd.settings.at_launch, LaunchLibrary::Last);
    assert!(odd.settings.recent_opened.is_empty());
    assert_eq!(odd.settings.startup_view, crate::state::StartupView::Detail, "the rest of the file still loads");

    let mut ui = crate::UiState::default();
    ui.settings.at_launch = LaunchLibrary::Ask;
    ui.settings.recent_libraries = vec!["/a".into()];
    ui.settings.recent_opened.insert("/a".into(), "2026-10-23T09:00:00".into());
    let saved = serde_json::to_value(&ui).unwrap();
    assert_eq!(saved["settings"]["atLaunch"], "ask");
    assert_eq!(saved["settings"]["recentLibraries"], json!(["/a"]), "the list keeps its old shape");
    assert_eq!(saved["settings"]["recentLibrariesOpened"], json!({"/a": "2026-10-23T09:00:00"}));
    let back: crate::UiState = serde_json::from_value(saved).unwrap();
    assert_eq!(back.settings, ui.settings);
}

/// The Settings choice and `ui.set` change the setting; an opened library gets its time.
#[test]
fn settings_choose_ask_me_and_opening_a_library_records_when() {
    let dir = temp("settings");
    let mut session = lightcraft_engine::Session::new().with_fs();
    session.open_library(dir.join("first"), false).unwrap();
    let mut app = LightcraftApp::new(session, Services::default());
    app.services.pick_folder = Some(Box::new(|| None));
    app.ui.settings.recent_opened.insert("/forgotten".into(), "2020-01-01T00:00:00".into());
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    settle(&mut h);
    let first = dir.join("first").to_string_lossy().into_owned();
    // the session clock's time (tests: a fixed one), only for listed libraries
    assert_eq!(h.app.ui.settings.recent_opened.get(&first).map(String::as_str), Some("2026-09-30T12:00:00"));
    assert!(!h.app.ui.settings.recent_opened.contains_key("/forgotten"));

    h.request("engine.execute", json!({"command": "app.settings", "params": {"tab": "general"}}), T);
    settle(&mut h);
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:settingsAtLaunch-1"}), T)["ok"], true);
    settle(&mut h);
    assert_eq!(h.app.ui.settings.at_launch, LaunchLibrary::Ask);
    h.request("ui.clickWidget", json!({"id": "button:settingsAtLaunch-0"}), T);
    settle(&mut h);
    assert_eq!(h.app.ui.settings.at_launch, LaunchLibrary::Last);
    // also through ui.set, as scripts change settings
    let r = h.request("ui.set", json!({"settings": {"atLaunch": "ask"}}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(h.app.ui.settings.at_launch, LaunchLibrary::Ask);
    drop(h);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The app as the host starts it with *Open the last library*: `first` open, Alt watched.
fn opened_last(dir: &Path) -> (Headless, String, String) {
    let (first, second) = (make_library(&dir.join("first")), make_library(&dir.join("second")));
    let mut session = lightcraft_engine::Session::new().with_fs();
    session.open_library(&first, false).unwrap();
    let mut app = LightcraftApp::new(session, Services::default());
    app.ui.settings.recent_libraries = vec![first.clone(), second.clone()];
    app.ui.settings.library_path = first.clone();
    app.alt_asks_for_library = true;
    (Headless::new(app, [1300.0, 900.0], 1.0), first, second)
}

/// Alt held as the window appears shows the list once over the open library; Esc keeps it.
#[test]
fn alt_held_as_the_window_appears_asks_once_over_the_last_library() {
    let dir = temp("alt");
    let (mut h, first, _) = opened_last(&dir);
    settle(&mut h);
    assert!(h.app.library_problem.is_none(), "no Alt: the last library, no window");
    h.app.synthetic.push(egui::Event::ModifiersChanged(egui::Modifiers::ALT));
    settle(&mut h);
    let c = h.app.library_problem.as_ref().and_then(|p| p.chooser.clone()).expect("the list shows");
    assert_eq!(c.selected, Some(0));
    assert!(c.over.is_some(), "over the library that opened");
    assert!(c.libraries[0].unavailable.is_none(), "the open library itself can be chosen");
    assert!(has(&h, "button:libraryOpen"));
    // Esc keeps the library that opened
    h.request("ui.key", json!({"key": "Escape"}), T);
    settle(&mut h);
    assert!(!h.quit_requested(), "Esc doesn't quit while a library is open");
    assert!(h.app.library_problem.is_none());
    assert_eq!(h.app.session.library.as_ref().map(|l| l.dir.to_string_lossy().into_owned()), Some(first.clone()));
    // once only
    h.app.synthetic.push(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
    h.app.synthetic.push(egui::Event::ModifiersChanged(egui::Modifiers::ALT));
    settle(&mut h);
    assert!(h.app.library_problem.is_none());
    drop(h);

    // choosing another library from it switches to that one
    let dir2 = temp("alt-switch");
    let (mut h, _, second2) = opened_last(&dir2);
    h.app.synthetic.push(egui::Event::ModifiersChanged(egui::Modifiers::ALT));
    settle(&mut h);
    h.request("ui.key", json!({"key": "ArrowDown"}), T);
    settle(&mut h);
    h.request("ui.key", json!({"key": "Enter"}), T);
    settle(&mut h);
    assert_eq!(h.app.session.library.as_ref().map(|l| l.dir.to_string_lossy().into_owned()), Some(second2));
    assert!(h.app.library_problem.is_none());
    drop(h);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&dir2);
}

/// The Alt watch ends after the first seconds and needs the host to arm it.
#[test]
fn alt_is_only_watched_while_the_window_first_appears_and_when_armed() {
    let frame = |app: &mut LightcraftApp, ctx: &egui::Context, time: f64, alt: bool| {
        let modifiers = if alt { egui::Modifiers::ALT } else { egui::Modifiers::NONE };
        let raw = egui::RawInput { time: Some(time), events: vec![egui::Event::ModifiersChanged(modifiers)], ..Default::default() };
        ctx.run_ui(raw, |ui| crate::panels::library_problem::logic(app, ui.ctx())).textures_delta.clear();
    };
    let new_app = |armed| {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), Services::default());
        app.alt_asks_for_library = armed;
        app
    };
    // too late: the watch ends, a later Alt does nothing
    let (mut app, ctx) = (new_app(true), egui::Context::default());
    frame(&mut app, &ctx, ALT_SECONDS + 0.5, false);
    assert!(!app.alt_asks_for_library);
    frame(&mut app, &ctx, ALT_SECONDS + 0.6, true);
    assert!(app.library_problem.is_none());
    // not armed (--library, LIGHTCRAFT_LIBRARY, --memory, the browser)
    let (mut app, ctx) = (new_app(false), egui::Context::default());
    frame(&mut app, &ctx, 0.1, true);
    assert!(app.library_problem.is_none());
    // armed, in time
    let (mut app, ctx) = (new_app(true), egui::Context::default());
    frame(&mut app, &ctx, 0.1, false);
    assert!(app.alt_asks_for_library && app.library_problem.is_none());
    frame(&mut app, &ctx, 0.2, true);
    assert!(app.library_problem.as_ref().is_some_and(|p| p.chooser.is_some()));
    assert!(!app.alt_asks_for_library);
}
