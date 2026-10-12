//! The Keywording box at the top of the right panel's Keywords: the selection's keywords, driven
//! through the control channel.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// The Library grid with the Keywords panel open and two photos selected: the first with beach and
/// Weddings, the second with beach.
fn two_selected() -> (Headless, Vec<u64>) {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 1000.0], 1.0);
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(2).map(|p| p.0).collect();
    for (method, params) in [
        ("ui.set", json!({"view": "photoGrid", "right": "keywords"})),
        ("engine.execute", json!({"command": "photo.setMeta", "params": {"ids": [ids[0]], "keywords": ["beach", "Weddings"]}})),
        ("engine.execute", json!({"command": "photo.setMeta", "params": {"ids": [ids[1]], "keywords": ["beach"]}})),
        ("engine.execute", json!({"command": "library.select", "params": {"ids": ids}})),
    ] {
        let r = h.request(method, params, T);
        assert_eq!(r["ok"], true, "{r}");
    }
    h.settle(SETTLE);
    (h, ids)
}

fn ask(h: &mut Headless, method: &str, params: serde_json::Value) {
    let r = h.request(method, params.clone(), T);
    assert_eq!(r["ok"], true, "{method} {params}: {r}");
    h.settle(SETTLE);
}

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

fn keywords_of(h: &Headless, id: u64) -> Vec<String> {
    h.app.session.catalog.photo(lightcraft_catalog::PhotoId(id)).unwrap().meta.keywords.clone()
}

/// The chips are the keywords of every selected photo, not only the active one's; a keyword only
/// some of them have is marked.
#[test]
fn the_chips_are_the_selections_keywords() {
    let (h, _) = two_selected();
    assert!(
        has(&h, "keywordChip:beach") && has(&h, "keywordChip:Weddings"),
        "{:?}",
        h.app.widgets.iter().filter(|(w, _)| w.starts_with("keywordChip")).collect::<Vec<_>>()
    );
    assert!(has(&h, "keywordChipPartial:Weddings"), "only one of the two has Weddings");
    assert!(!has(&h, "keywordChipPartial:beach"), "both have beach");
}

/// Only the × removes a keyword, from every selected photo; a click on the chip itself doesn't.
#[test]
fn only_the_cross_removes_a_keyword() {
    let (mut h, ids) = two_selected();
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordChip:beach"}));
    assert!(ids.iter().all(|id| keywords_of(&h, *id).contains(&"beach".to_string())), "a click on the chip keeps it");
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordChipRemove:beach"}));
    assert!(ids.iter().all(|id| !keywords_of(&h, *id).contains(&"beach".to_string())), "× took it off both");
}

/// A chip's menu acts on the selection, never on the whole library: give a keyword only some have
/// to all of them, take it off them, show its photos, or edit it (Edit Keyword Tag). Deleting a
/// keyword from every photo of the library is the Keyword List's.
#[test]
fn a_chips_menu_acts_on_the_selection() {
    let (mut h, ids) = two_selected();
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordChip:Weddings", "button": "right"}));
    for item in ["add", "remove", "show", "edit"] {
        assert!(has(&h, &format!("keywordChipMenu:{item}")), "{item}");
    }
    assert!(!has(&h, "keywordChipMenu:delete"), "no library-wide delete");
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordChipMenu:add"}));
    assert!(ids.iter().all(|id| keywords_of(&h, *id).contains(&"Weddings".to_string())), "given to both");
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordChip:Weddings", "button": "right"}));
    assert!(!has(&h, "keywordChipMenu:add"), "both have it now");
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordChipMenu:edit"}));
    assert!(matches!(&h.app.ui.dialog, Some(crate::state::Dialog::KeywordTag { editing: Some(k), .. }) if k == "Weddings"), "{:?}", h.app.ui.dialog);
}

/// The "Add keyword" box is a shared text field: right-click ▸ Paste fills it, Return gives the
/// keywords to every selected photo, and Esc gives up what was typed.
#[test]
fn the_add_keyword_box_pastes_adds_and_gives_up() {
    let (mut h, ids) = two_selected();
    h.view.clipboard = "travel, sunset".into();
    ask(&mut h, "ui.clickWidget", json!({"id": "field:keyword", "button": "right"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "field:keyword:paste"}));
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    for id in &ids {
        let k = keywords_of(&h, *id);
        assert!(k.contains(&"travel".to_string()) && k.contains(&"sunset".to_string()), "{k:?}");
    }
    ask(&mut h, "ui.clickWidget", json!({"id": "field:keyword"}));
    ask(&mut h, "ui.text", json!({"text": "harbour"}));
    ask(&mut h, "ui.key", json!({"key": "Escape"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "field:keyword"}));
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    assert!(ids.iter().all(|id| !keywords_of(&h, *id).contains(&"harbour".to_string())), "Esc gave it up");
}

/// The keyword set's buttons say whether the selection has each keyword, as their ⌥1–⌥9 toggle it
/// on the selection: on when every selected photo has it, partly on when only some do.
#[test]
fn keyword_set_buttons_reflect_the_selection() {
    let (mut h, _) = two_selected();
    let sets = h.app.session.execute("keyword.sets", &json!({})).unwrap();
    let at = |k: &str| sets["keywords"].as_array().unwrap().iter().position(|x| x == k).map(|i| i + 1).unwrap_or_else(|| panic!("{k} in {sets}"));
    assert!(has(&h, &format!("kwSetOn:{}", at("beach"))), "both have beach");
    assert!(has(&h, &format!("kwSetSome:{}", at("Weddings"))), "only the active one has Weddings");
    assert!(!has(&h, &format!("kwSetOn:{}", at("Weddings"))));
}

/// "Will Export" shows the names exported files will carry for the selection instead of the chips:
/// a keyword left out of export isn't there; "Keywords" brings the chips back.
#[test]
fn will_export_shows_what_exports_carry() {
    let (mut h, _) = two_selected();
    let r = h.request("engine.execute", json!({"command": "keyword.edit", "params": {"keyword": "Weddings", "includeOnExport": false}}), T);
    assert_eq!(r["ok"], true, "{r}");
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordView:willExport"}));
    assert!(has(&h, "keywordExport:beach"));
    assert!(!has(&h, "keywordExport:Weddings"), "left out of export");
    assert!(!has(&h, "keywordChip:beach"), "the chips give way");
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordView:keywords"}));
    assert!(has(&h, "keywordChip:beach") && !has(&h, "keywordExport:beach"));
}

/// The painter's keyword field is a shared text field: right-click ▸ Paste fills it, and Return
/// starts painting as the Paint button does.
#[test]
fn the_painters_field_pastes_and_return_paints() {
    let (mut h, _) = two_selected();
    h.view.clipboard = "Weddings".into();
    ask(&mut h, "ui.clickWidget", json!({"id": "field:keywordPainter", "button": "right"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "field:keywordPainter:paste"}));
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    assert_eq!(h.app.ui.keyword_painter.as_deref(), Some("Weddings"));
}

/// Suggestions follow the selection: a keyword only some selected photos have is suggested, to give
/// it to all of them; one every selected photo has isn't.
#[test]
fn suggestions_follow_the_selection() {
    let (h, _) = two_selected();
    assert!(has(&h, "kwSuggest:Weddings"), "only the active one has it");
    assert!(!has(&h, "kwSuggest:beach"), "both have it");
}

/// Chips wrap whole and stay inside the panel: none overlaps another or runs past the panel's
/// edge, and one longer than the panel is shortened to fit (a chip laid out in what was left of a
/// line used to wrap its own text, a letter a line, over the others).
#[test]
fn chips_wrap_whole_inside_the_panel() {
    let (mut h, ids) = two_selected();
    let long = "Places|Portugal|Lisbon|Belém tower and the old town square on the river";
    let r = h.request(
        "engine.execute",
        json!({"command": "photo.setMeta", "params": {"ids": ids, "addKeywords": ["Events|Weddings", "sunset", "Places|Portugal|Lisbon", "harbour", long]}}),
        T,
    );
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let panel = h.app.widgets.iter().find(|(w, _)| w == "panel:right_panel").map(|(_, r)| *r).expect("the right panel");
    let chips: Vec<(String, egui::Rect)> = h.app.widgets.iter().filter(|(w, _)| w.starts_with("keywordChip:")).cloned().collect();
    assert!(chips.len() >= 6, "{chips:?}");
    for (i, (id, r)) in chips.iter().enumerate() {
        assert!(r.height() < 30.0, "{id} on one line: {r:?}");
        assert!(r.left() >= panel.left() && r.right() <= panel.right(), "{id} inside the panel: {r:?} in {panel:?}");
        for (other, o) in chips.iter().skip(i + 1) {
            assert!(!r.intersects(o.shrink(0.5)), "{id} overlaps {other}");
        }
    }
}

/// With an active photo and nothing else selected, the box is that photo's, as the commands are:
/// its chips show (keywords typed go to it), and the Keyword List ticks what it has.
#[test]
fn the_active_photo_alone_is_the_selection() {
    let (mut h, ids) = two_selected();
    ask(&mut h, "engine.execute", json!({"command": "library.select", "params": {"ids": [], "active": ids[0]}}));
    assert_eq!(h.app.session.selection.active.map(|p| p.0), Some(ids[0]));
    assert!(h.app.session.selection.ids.is_empty());
    assert!(has(&h, "keywordChip:Weddings") && !has(&h, "keywordChipPartial:Weddings"), "the active photo's keywords");
    let ticks = crate::panels::keyword_list::Ticks::of(&h.app.session.catalog, &h.app.session.targets(&json!({})));
    assert_eq!(ticks.tick("Weddings"), crate::panels::keyword_list::Tick::All);
    assert!(has(&h, "keywordCheck:beach"));
}

/// Esc in the Add keyword box gives back what it held before this edit, as every text field does:
/// text left there by an earlier edit stays.
#[test]
fn escape_gives_back_the_text_from_before_the_edit() {
    let (mut h, ids) = two_selected();
    ask(&mut h, "ui.clickWidget", json!({"id": "field:keyword"}));
    ask(&mut h, "ui.text", json!({"text": "harbour"}));
    ask(&mut h, "ui.key", json!({"key": "Tab"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "field:keyword"}));
    ask(&mut h, "ui.key", json!({"key": "End"}));
    ask(&mut h, "ui.text", json!({"text": ", sea"}));
    ask(&mut h, "ui.key", json!({"key": "Escape"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "field:keyword"}));
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    let k = keywords_of(&h, ids[0]);
    assert!(k.contains(&"harbour".to_string()) && !k.contains(&"sea".to_string()), "{k:?}");
}

/// "Keywords & Containing Keywords" lists the selection's keywords with the keywords containing
/// them, flat (read only).
#[test]
fn containing_keywords_view_lists_parents_too() {
    let (mut h, ids) = two_selected();
    let r = h.request("engine.execute", json!({"command": "photo.setMeta", "params": {"ids": [ids[0]], "addKeywords": ["Events|Birthdays"]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordView:containing"}));
    for name in ["Events", "Birthdays", "beach"] {
        assert!(has(&h, &format!("keywordContaining:{name}")), "{name}");
    }
    assert!(!has(&h, "keywordChip:beach"), "the chips give way");
}

fn rect(h: &Headless, id: &str) -> egui::Rect {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no widget {id}"))
}

fn count(h: &Headless, prefix: &str) -> usize {
    h.app.widgets.iter().filter(|(w, _)| w.starts_with(prefix)).count()
}

/// Give both selected photos `keywords`, replacing what they had.
fn give(h: &mut Headless, ids: &[u64], keywords: &[String]) {
    ask(h, "engine.execute", json!({"command": "photo.setMeta", "params": {"ids": ids, "keywords": keywords}}));
}

/// Sixteen keywords inside "Subjects", each long enough to take a line of the panel.
fn sixteen() -> Vec<String> {
    (1..=16).map(|i| format!("Subjects|a keyword with quite a long name {i:02}")).collect()
}

// Given keywords inside the same keyword and the box set to group them,
// when the panel is drawn,
// then they share a line naming that keyword and their chips say only what is below it, side by
// side where they fit; each chip still acts on its whole keyword.
#[test]
fn grouped_keywords_share_a_line_naming_their_parent() {
    let (mut h, ids) = two_selected();
    let keywords: Vec<String> = ["Places|Italy|Rome", "Places|Italy|Venice", "Transport|Ferry", "sea"].iter().map(|s| s.to_string()).collect();
    give(&mut h, &ids, &keywords);
    assert_eq!(
        count(&h, "keywordGroup:"),
        0,
        "whole names unless asked: {:?}",
        h.app.widgets.iter().filter(|(w, _)| w.starts_with("keywordGroup")).collect::<Vec<_>>()
    );
    let whole = rect(&h, "keywordChip:Places|Italy|Rome").width();

    h.app.ui.settings.keyword_chips = crate::state::KeywordChips::Grouped;
    h.settle(SETTLE);
    let heading = rect(&h, "keywordGroup:Places|Italy");
    let (rome, venice) = (rect(&h, "keywordChip:Places|Italy|Rome"), rect(&h, "keywordChip:Places|Italy|Venice"));
    assert!(rome.top() >= heading.bottom(), "under its heading: {rome:?} {heading:?}");
    assert_eq!(rome.top(), venice.top(), "side by side");
    assert!(rome.width() < whole, "the chip says only “Rome”: {} < {whole}", rome.width());
    // a keyword that shares nothing has no heading and comes first
    assert!(!has(&h, "keywordGroup:Transport") && !has(&h, "keywordGroup:"), "one keyword is no group");
    assert!(rect(&h, "keywordChip:sea").top() < heading.top());
    // the × still takes the whole keyword off both
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordChipRemove:Places|Italy|Rome"}));
    assert!(ids.iter().all(|id| !keywords_of(&h, *id).iter().any(|k| k == "Places|Italy|Rome")));
    assert!(ids.iter().all(|id| keywords_of(&h, *id).iter().any(|k| k == "Places|Italy|Venice")));
}

// Given sixteen keywords and the grouped box,
// when the panel is drawn,
// then eight lines show with "+N more" for the rest, which shows them all; "Show fewer" brings the
// short list back. The Keyword List is nearer the top for it.
#[test]
fn a_long_grouped_list_is_shortened_until_asked_for() {
    let (mut h, ids) = two_selected();
    give(&mut h, &ids, &sixteen());
    assert_eq!(count(&h, "keywordChip:"), 16, "whole names: every keyword, as before");
    assert!(!has(&h, "keywordChipsMore"));
    let list_before = rect(&h, "field:keywordFilter").top();

    h.app.ui.settings.keyword_chips = crate::state::KeywordChips::Grouped;
    h.settle(SETTLE);
    assert_eq!(count(&h, "keywordChip:"), 7, "a heading and seven chips");
    assert!(has(&h, "keywordChipsMore") && !has(&h, "keywordChipsFewer"));
    assert!(rect(&h, "field:keywordFilter").top() < list_before - 80.0, "{} from {list_before}", rect(&h, "field:keywordFilter").top());

    ask(&mut h, "ui.clickWidget", json!({"id": "keywordChipsMore"}));
    assert_eq!(count(&h, "keywordChip:"), 16);
    assert!(has(&h, "keywordChipsFewer") && !has(&h, "keywordChipsMore"));
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordChipsFewer"}));
    assert_eq!(count(&h, "keywordChip:"), 7);
}

// Given the grouped box, chips stay inside the panel and never overlap, whatever their length.
#[test]
fn grouped_chips_stay_inside_the_panel() {
    let (mut h, ids) = two_selected();
    h.app.ui.settings.keyword_chips = crate::state::KeywordChips::Grouped;
    let long = "Places|Portugal|Lisbon|Belém tower and the old town square on the river by the bridge";
    let keywords: Vec<String> =
        ["Events|Weddings", "Events|Birthdays", "sunset", "Places|Portugal|Porto", long].iter().map(|s| s.to_string()).collect();
    give(&mut h, &ids, &keywords);
    let panel = rect(&h, "panel:right_panel");
    let chips: Vec<(String, egui::Rect)> = h.app.widgets.iter().filter(|(w, _)| w.starts_with("keywordChip:")).cloned().collect();
    assert_eq!(chips.len(), 5, "{chips:?}");
    for (i, (id, r)) in chips.iter().enumerate() {
        assert!(r.left() >= panel.left() && r.right() <= panel.right(), "{id} inside the panel: {r:?} in {panel:?}");
        for (other, o) in chips.iter().skip(i + 1) {
            assert!(!r.intersects(o.shrink(0.5)), "{id} overlaps {other}");
        }
    }
}

// Given a selection without keywords, the box says so (as its other two views do).
#[test]
fn a_selection_without_keywords_says_so() {
    let (mut h, ids) = two_selected();
    give(&mut h, &ids, &[]);
    assert!(has(&h, "keywordChipsEmpty"));
    h.app.ui.settings.keyword_chips = crate::state::KeywordChips::Grouped;
    h.settle(SETTLE);
    assert!(has(&h, "keywordChipsEmpty"));
}

// Given the Keywords panel,
// when the title of Keywording or of the Keyword List is clicked,
// then that section folds shut and the other stays; a second click opens it, and what is folded is
// kept with the interface's settings.
#[test]
fn the_sections_fold_by_their_titles() {
    let (mut h, _) = two_selected();
    assert!(has(&h, "field:keyword") && has(&h, "field:keywordFilter"));
    let list_before = rect(&h, "field:keywordFilter").top();
    ask(&mut h, "ui.clickWidget", json!({"id": "section:keywording"}));
    assert!(!has(&h, "field:keyword") && !has(&h, "keywordChip:beach") && !has(&h, "kwSet:1"), "Keywording is shut");
    assert!(has(&h, "field:keywordFilter"), "the Keyword List stays");
    assert!(rect(&h, "field:keywordFilter").top() < list_before - 100.0);
    let saved = serde_json::to_string(&h.app.ui).unwrap();
    let back = serde_json::from_str::<crate::UiState>(&saved).unwrap().sanitized();
    assert!(back.section_folded("keywording") && !back.section_folded("keywordList"));
    assert!(!serde_json::from_str::<crate::UiState>("{}").unwrap().section_folded("keywording"), "settings from before: open");

    ask(&mut h, "ui.clickWidget", json!({"id": "section:keywordList"}));
    assert!(!has(&h, "field:keywordFilter") && !has(&h, "keywordCheck:beach"), "the Keyword List is shut");
    ask(&mut h, "ui.clickWidget", json!({"id": "section:keywording"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "section:keywordList"}));
    assert!(has(&h, "field:keyword") && has(&h, "field:keywordFilter") && has(&h, "keywordChip:beach"));
}

// Given the grouped box, the suggestions say only the keyword's own name (its whole path when two
// of them have the same name) and still give the whole keyword.
#[test]
fn grouped_suggestions_name_the_keyword_alone() {
    let (mut h, ids) = two_selected();
    ask(&mut h, "engine.execute", json!({"command": "photo.setMeta", "params": {"ids": [ids[0]], "addKeywords": ["Places|Portugal|Lisbon"]}}));
    let whole = rect(&h, "kwSuggest:Places|Portugal|Lisbon").width();
    h.app.ui.settings.keyword_chips = crate::state::KeywordChips::Grouped;
    h.settle(SETTLE);
    assert!(rect(&h, "kwSuggest:Places|Portugal|Lisbon").width() < whole);
    ask(&mut h, "ui.clickWidget", json!({"id": "kwSuggest:Places|Portugal|Lisbon"}));
    assert!(ids.iter().all(|id| keywords_of(&h, *id).iter().any(|k| k == "Places|Portugal|Lisbon")));
}

/// Two photos selected (beach + Weddings, beach), and a third photo of the library, not selected,
/// with Places|Portugal|Lisbon and Places|Portugal|Porto.
fn with_places() -> (Headless, Vec<u64>) {
    let (mut h, ids) = two_selected();
    let third = h.app.session.visible_cloned().get(2).map(|p| p.0).expect("a third photo");
    ask(
        &mut h,
        "engine.execute",
        json!({"command": "photo.setMeta", "params": {"ids": [third], "keywords": ["Places|Portugal|Lisbon", "Places|Portugal|Porto"]}}),
    );
    (h, ids)
}

fn typed(h: &mut Headless, text: &str) {
    ask(h, "ui.clickWidget", json!({"id": "field:keyword"}));
    ask(h, "ui.text", json!({"text": text}));
}

fn all_have(h: &Headless, ids: &[u64], keyword: &str) -> bool {
    ids.iter().all(|id| keywords_of(h, *id).iter().any(|k| k == keyword))
}

fn none_has(h: &Headless, ids: &[u64], keyword: &str) -> bool {
    ids.iter().all(|id| !keywords_of(h, *id).iter().any(|k| k == keyword))
}

// Given a library with Places › Portugal › Lisbon,
// when "lis" is typed in the Add keyword box,
// then the keyword is offered right under the box, and the suggestions further down stay what
// they were (they used to turn into the completions, a long way from the box).
#[test]
fn typing_offers_the_librarys_keywords_under_the_box() {
    let (mut h, _) = with_places();
    assert!(has(&h, "kwSuggest:Weddings"));
    typed(&mut h, "lis");
    let (field, offer) = (rect(&h, "field:keyword"), rect(&h, "kwComplete:Places|Portugal|Lisbon"));
    assert!(offer.top() >= field.bottom() && offer.top() - field.bottom() < 12.0, "right under the box: {offer:?} {field:?}");
    assert!(offer.left() >= field.left() - 1.0 && offer.right() <= field.right() + 1.0, "as wide as the box: {offer:?} {field:?}");
    assert!(!has(&h, "kwComplete:Places|Portugal|Porto"), "only what matches");
    assert!(has(&h, "kwSuggest:Weddings") && !has(&h, "kwSuggest:Places|Portugal|Lisbon"), "the suggestions stay");
    // nothing typed, nothing offered
    ask(&mut h, "ui.key", json!({"key": "Escape"}));
    assert_eq!(count(&h, "kwComplete"), 0);
}

// Given the offers under the box,
// when ↓ then Return are pressed,
// then the highlighted keyword goes to every selected photo (not the letters typed), and the box
// is empty and still has the cursor for the next keyword.
#[test]
fn down_and_return_give_the_offered_keyword() {
    let (mut h, ids) = with_places();
    typed(&mut h, "lis");
    ask(&mut h, "ui.key", json!({"key": "ArrowDown"}));
    assert!(has(&h, "kwCompleteAt:Places|Portugal|Lisbon"), "highlighted");
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    assert!(all_have(&h, &ids, "Places|Portugal|Lisbon"));
    assert!(none_has(&h, &ids, "lis"), "the letters typed are not a keyword");
    assert_eq!(count(&h, "kwComplete"), 0, "the box is empty again");
    // still in the box: the next keyword is typed straight away
    ask(&mut h, "ui.text", json!({"text": "harbour"}));
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    assert!(all_have(&h, &ids, "harbour"));
}

// Given the offers under the box, Return alone still gives what was typed, as it always did; ↑
// from the first offer goes back to that.
#[test]
fn return_alone_gives_what_was_typed() {
    let (mut h, ids) = with_places();
    typed(&mut h, "lis");
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    assert!(all_have(&h, &ids, "lis") && none_has(&h, &ids, "Places|Portugal|Lisbon"));

    typed(&mut h, "por");
    ask(&mut h, "ui.key", json!({"key": "ArrowDown"}));
    ask(&mut h, "ui.key", json!({"key": "ArrowUp"}));
    assert_eq!(count(&h, "kwCompleteAt:"), 0, "none highlighted");
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    assert!(all_have(&h, &ids, "por") && none_has(&h, &ids, "Places|Portugal|Porto"));
}

// Given a name the library doesn't have,
// when it is typed,
// then the box says Return will create it (and inside which keyword, with a default parent); a
// keyword the library has is not announced as new.
#[test]
fn a_new_name_is_announced_as_new() {
    let (mut h, _) = with_places();
    typed(&mut h, "Lisbn");
    assert!(has(&h, "kwCompleteCreate"), "a misspelling is a new keyword: say so");
    ask(&mut h, "ui.key", json!({"key": "Escape"}));
    typed(&mut h, "weddings");
    assert!(!has(&h, "kwCompleteCreate"), "the library has Weddings");
    assert!(has(&h, "kwComplete:Weddings"), "one of the two lacks it");
    ask(&mut h, "ui.key", json!({"key": "Escape"}));
    typed(&mut h, "Places|Portugal");
    assert!(!has(&h, "kwCompleteCreate"), "a keyword with keywords inside it is there too");
}

// Given the offers under the box, a click on one gives that keyword to the selected photos; a
// click on the "create" line gives what was typed. Keywords typed before a comma go along.
#[test]
fn a_click_on_an_offer_gives_it() {
    let (mut h, ids) = with_places();
    typed(&mut h, "sea, lis");
    assert!(has(&h, "kwComplete:Places|Portugal|Lisbon"), "the offers are for what follows the comma");
    ask(&mut h, "ui.clickWidget", json!({"id": "kwComplete:Places|Portugal|Lisbon"}));
    assert!(all_have(&h, &ids, "Places|Portugal|Lisbon") && all_have(&h, &ids, "sea") && none_has(&h, &ids, "lis"));

    typed(&mut h, "harbour");
    ask(&mut h, "ui.clickWidget", json!({"id": "kwCompleteCreate"}));
    assert!(all_have(&h, &ids, "harbour"));
}

// Given more offers than there is room for, ↓ stops at the last and never runs off the list.
#[test]
fn the_highlight_stays_on_the_list() {
    let (mut h, ids) = with_places();
    typed(&mut h, "por");
    for _ in 0..40 {
        ask(&mut h, "ui.key", json!({"key": "ArrowDown"}));
    }
    let lit: Vec<String> = h.app.widgets.iter().filter_map(|(w, _)| w.strip_prefix("kwCompleteAt:").map(str::to_string)).collect();
    assert_eq!(lit.len(), 1, "{lit:?}");
    let offered = count(&h, "kwComplete:");
    assert!(offered >= 2, "Portugal and Porto both match");
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    assert!(all_have(&h, &ids, &lit[0]), "the last offer, {}, was given", lit[0]);
    assert!(none_has(&h, &ids, "por"));
}

fn toast(h: &Headless) -> String {
    h.app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default()
}

// Given several photos selected, the box says how many its keywords are of; with one photo there
// is nothing to say.
#[test]
fn the_box_says_how_many_photos_it_is_for() {
    let (mut h, ids) = two_selected();
    assert!(has(&h, "keywordScope"), "two photos are selected");
    assert!(rect(&h, "keywordScope").bottom() <= rect(&h, "keywordChip:beach").top(), "above the chips it is about");
    ask(&mut h, "engine.execute", json!({"command": "library.select", "params": {"ids": [ids[0]]}}));
    assert!(!has(&h, "keywordScope"), "one photo");
}

// The × is a target as tall as its chip and as wide as that, apart from the name next to it.
#[test]
fn the_cross_is_as_tall_as_its_chip() {
    let (h, _) = two_selected();
    let (name, cross) = (rect(&h, "keywordChip:beach"), rect(&h, "keywordChipRemove:beach"));
    assert!(cross.height() >= 22.0 && cross.width() >= 22.0, "{cross:?}");
    assert_eq!(cross.height(), name.height());
    assert!(cross.left() >= name.right() - 0.5, "beside the name, not over it: {cross:?} {name:?}");
}

// Given a keyword on several selected photos,
// when its × takes it off them,
// then a message names the keyword and how many photos lost it and says how to get it back, and
// Undo does. Taking a keyword off one photo says nothing: the chip going is enough.
#[test]
fn taking_a_keyword_off_several_photos_says_so_and_undo_puts_it_back() {
    let (mut h, ids) = two_selected();
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordChipRemove:Weddings"}));
    assert_eq!(toast(&h), "", "only one of the two had Weddings");
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordChipRemove:beach"}));
    assert!(none_has(&h, &ids, "beach"));
    let said = toast(&h);
    assert!(said.contains("beach") && said.contains('2') && said.contains("Undo"), "{said}");
    ask(&mut h, "engine.execute", json!({"command": "edit.undo", "params": {}}));
    assert!(all_have(&h, &ids, "beach"), "one undo step puts it back on both");

    // the menu's Remove from Selected Photos says the same
    h.app.ui.toast = None;
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordChip:beach", "button": "right"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordChipMenu:remove"}));
    assert!(none_has(&h, &ids, "beach") && toast(&h).contains("beach"), "{}", toast(&h));
}

// Names of any length and script stay inside the panel in both layouts, and nothing breaks: a
// name of 400 letters, one of emoji and right-to-left text, levels of one letter.
#[test]
fn any_name_fits_the_panel() {
    let (mut h, ids) = two_selected();
    let long = "x".repeat(400);
    let keywords: Vec<String> =
        vec![long.clone(), format!("a|{long}"), "🙂|العربية|שלום".into(), "🙂|العربية|مرحبا".into(), "a|b|c|d|e|f|g|h".into(), "İ|ı".into()];
    give(&mut h, &ids, &keywords);
    for layout in [crate::state::KeywordChips::Paths, crate::state::KeywordChips::Grouped] {
        h.app.ui.settings.keyword_chips = layout;
        h.settle(SETTLE);
        let panel = rect(&h, "panel:right_panel");
        let chips: Vec<(String, egui::Rect)> = h.app.widgets.iter().filter(|(w, _)| w.starts_with("keywordChip:")).cloned().collect();
        assert_eq!(chips.len(), keywords.len(), "{layout:?}");
        for (id, r) in &chips {
            assert!(
                r.left() >= panel.left() && r.right() <= panel.right(),
                "{layout:?}: {} inside the panel: {r:?}",
                id.chars().take(40).collect::<String>()
            );
            assert!(r.width() > 0.0 && r.height() < 30.0);
        }
    }
    // and typing them offers them without trouble
    typed(&mut h, &"x".repeat(300));
    typed(&mut h, "🙂");
    assert!(count(&h, "kwComplete") >= 1);
}
