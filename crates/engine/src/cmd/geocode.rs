//! Reverse geocoding (issue #582): fill City / State / Country from a photo's GPS position, offline, with the
//! GeoNames place names ([`lightcraft_geocode`]). The place names (CC-BY 4.0) are not shipped with the app: the user
//! downloads them once (`metadata.downloadPlaceNames`, ~3 MB from download.geonames.org) into the settings folder;
//! after that every lookup is local and no position ever leaves the machine.
//!
//! Sources: GeoNames dump readme (prose); own design.

use std::path::PathBuf;

use serde_json::{Value, json};

use lightcraft_catalog::{Op, PhotoId};
use lightcraft_geocode::{GEONAMES_ATTRIBUTION, GEONAMES_FILES, GeoNames, LatLon};

use super::{CommandSpec, always, bad, bool_or, cmd, has_selection, str_param};
use crate::activity::Cancel;
use crate::{Result, Session};

/// Where the place names live: `dataset` (a folder) or `<settings>/geonames`.
fn dataset_dir(p: &Value) -> Option<PathBuf> {
    str_param(p, "dataset").map(PathBuf::from).or_else(|| crate::config::config_dir().map(|d| d.join("geonames")))
}

fn status(_s: &mut Session, p: &Value) -> Result<Value> {
    let dir = dataset_dir(p);
    let size = dir.as_ref().and_then(|d| std::fs::metadata(d.join(GEONAMES_FILES[0])).ok()).map(|m| m.len());
    Ok(
        json!({"downloaded": size.is_some(), "bytes": size, "path": dir, "attribution": GEONAMES_ATTRIBUTION, "source": lightcraft_geocode::GEONAMES_BASE}),
    )
}

/// Which place fields to write: a field is filled when it is empty, or always with `overwrite`.
fn fill(meta: &mut lightcraft_catalog::Meta, place: &lightcraft_geocode::Place, overwrite: bool) {
    for (field, value) in [
        (&mut meta.location, &place.sublocation),
        (&mut meta.city, &place.city),
        (&mut meta.state, &place.state),
        (&mut meta.country, &place.country),
    ] {
        if !value.is_empty() && (overwrite || field.trim().is_empty()) {
            *field = value.clone();
        }
    }
}

fn reverse(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "metadata.reverseGeocode";
    let overwrite = bool_or(p, "overwrite", false);
    let ids = s.targets(p);
    let todo: Vec<(PhotoId, LatLon)> = ids
        .iter()
        .filter_map(|id| {
            let (la, lo) = s.catalog.photo(*id)?.meta.gps?;
            let at = LatLon::new(la, lo);
            at.is_valid().then_some((*id, at))
        })
        .collect();
    if todo.is_empty() {
        return Err(bad(C, "none of these photos has a GPS position"));
    }
    let dir = dataset_dir(p).ok_or_else(|| bad(C, "no settings folder: give `dataset`"))?;
    let names = GeoNames::load(&dir).map_err(|e| bad(C, format!("{e}: download the place names first (Photo ▸ Download Place Names)")))?;
    // a large selection shows in the activity stack and can be stopped
    let task = (todo.len() > 1).then(|| s.activity.start("geocode", "Looking up places", Cancel::Yes));
    let mut ops = Vec::new();
    let mut found = Vec::new();
    for (n, (id, at)) in todo.iter().enumerate() {
        if let Some(t) = &task {
            if t.cancel_flag().is_some_and(|f| f.load(std::sync::atomic::Ordering::Relaxed)) {
                return Err(bad(C, "cancelled"));
            }
            t.progress(n as u64, todo.len() as u64);
        }
        let Some(place) = names.reverse(*at) else { continue };
        let Some(ph) = s.catalog.photo(*id) else { continue };
        let mut meta = ph.meta.clone();
        fill(&mut meta, &place, overwrite);
        if meta != ph.meta {
            ops.push(Op::SetMeta { id: *id, meta: Box::new(meta) });
        }
        found.push(json!({"id": id.0, "place": place}));
    }
    drop(task);
    let changed = ops.len();
    if !bool_or(p, "dryRun", false) && changed > 0 {
        s.commit("Reverse Geocode", Op::Batch { ops })?;
    }
    Ok(json!({"looked": todo.len(), "found": found.len(), "changed": changed, "photos": found, "attribution": GEONAMES_ATTRIBUTION}))
}

#[cfg(not(target_arch = "wasm32"))]
mod download {
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    use lightcraft_geocode::{GEONAMES_BASE, GEONAMES_ZIP};

    use super::*;
    use crate::activity::Unit;

    const MAX_ZIP: u64 = 64 * 1024 * 1024;
    const MAX_TEXT: usize = 256 * 1024 * 1024;

    fn get(name: &str, base: &str, dir: &Path, cancel: &AtomicBool, task: &crate::activity::TaskHandle) -> std::result::Result<(), String> {
        let _ = std::fs::remove_file(dir.join(name));
        let file = lightcraft_fetch::FileSpec { name, size: None, sha256: None, max: MAX_ZIP };
        lightcraft_fetch::download_url(&file, &format!("{base}{name}"), dir, &lightcraft_fetch::Options::default(), cancel, &mut |pr| {
            task.progress(pr.done, pr.total.max(pr.done))
        })
        .map_err(|e| e.to_string())
    }

    /// Fetch and unpack the place names into `dir`; the new files replace the old ones only when complete.
    pub(super) fn run(base: &str, dir: &Path, cancel: &AtomicBool, task: &crate::activity::TaskHandle) -> std::result::Result<usize, String> {
        let staging = dir.join(".download");
        std::fs::create_dir_all(&staging).map_err(|e| format!("could not create {}: {e}", staging.display()))?;
        get(GEONAMES_ZIP, base, &staging, cancel, task)?;
        let zip = std::fs::read(staging.join(GEONAMES_ZIP)).map_err(|e| e.to_string())?;
        let cities = lightcraft_geocode::unzip::extract(&zip, GEONAMES_FILES[0], MAX_TEXT).map_err(|e| e.to_string())?;
        let places = GeoNames::parse(&String::from_utf8_lossy(&cities), None, None).len();
        if places == 0 {
            return Err("the downloaded place list is empty".into());
        }
        let _ = std::fs::remove_file(staging.join(GEONAMES_ZIP));
        let write = |name: &str, data: &[u8]| std::fs::write(dir.join(name), data).map_err(|e| format!("could not save {name}: {e}"));
        for name in &GEONAMES_FILES[1..] {
            // optional: without them states stay blank and countries are ISO codes
            match get(name, base, &staging, cancel, task).and_then(|()| std::fs::read(staging.join(name)).map_err(|e| e.to_string())) {
                Ok(b) => write(name, &b)?,
                Err(e) => log::warn!("place names: {name}: {e}"),
            }
            let _ = std::fs::remove_file(staging.join(name));
        }
        write(GEONAMES_FILES[0], &cities)?;
        let _ = std::fs::remove_dir(&staging);
        Ok(places)
    }

    /// `metadata.downloadPlaceNames`: starts a background download (shown in the activity stack, cancellable).
    pub(super) fn start(s: &mut Session, p: &Value) -> Result<Value> {
        const C: &str = "metadata.downloadPlaceNames";
        let dir = dataset_dir(p).ok_or_else(|| bad(C, "no settings folder: give `dataset`"))?;
        let base = str_param(p, "base").unwrap_or(GEONAMES_BASE).to_string();
        if !(base.starts_with("https://") || base.starts_with("http://127.0.0.1")) {
            return Err(bad(C, "`base` must be an https:// address"));
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let task = s.activity.start("download", "Downloading place names", Cancel::Flag(cancel.clone()));
        task.set_unit(Unit::Bytes);
        task.detail("GeoNames");
        let handle = task.handle();
        let thread_dir = dir.clone();
        std::thread::Builder::new()
            .name("place-names-download".into())
            .spawn(move || {
                // the activity row goes when the thread ends, however it ends
                let _task = task;
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&base, &thread_dir, &cancel, &handle)));
                match outcome {
                    Ok(Ok(n)) => log::info!("place names: {n} places downloaded"),
                    Ok(Err(e)) => log::warn!("place names download failed: {e}"),
                    Err(_) => log::warn!("place names download stopped unexpectedly"),
                }
            })
            .map_err(|e| bad(C, format!("could not start the download: {e}")))?;
        Ok(json!({"started": true, "path": dir, "attribution": GEONAMES_ATTRIBUTION}))
    }
}

pub fn specs() -> Vec<CommandSpec> {
    #[allow(unused_mut)]
    let mut v = vec![
        cmd!(
            "metadata.reverseGeocode",
            "Fill Location from GPS",
            ["Photo"],
            None,
            "{ids?, overwrite?: bool (also replace place fields that already have text; default false: only empty ones are filled), dryRun?, dataset?: folder} → {looked, found, changed, photos: [{id, place: {sublocation, city, state, country, iso, distance}}], attribution} — fills City / State / Country (and Sublocation when known) from GPS with the offline GeoNames place names (download them first); one undo step",
            has_selection,
            reverse
        ),
        cmd!(query "metadata.placeNamesStatus", "Place Names Status", [], None, "{dataset?: folder} → {downloaded, bytes, path, attribution, source}", always, status),
    ];
    #[cfg(not(target_arch = "wasm32"))]
    v.push(cmd!(
        "metadata.downloadPlaceNames",
        "Download Place Names (GeoNames)",
        ["Photo"],
        None,
        "{dataset?: folder} → {started, path, attribution} — downloads the GeoNames cities list (CC-BY 4.0, ~3 MB, from download.geonames.org) in the background for Fill Location from GPS; no photo data is sent",
        always,
        download::start
    ));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    const CITIES: &str = "2988507\tParis\tParis\t\t48.85341\t2.3488\tP\tPPLC\tFR\t\t11\t75\t\t\t2138551\t\t42\tEurope/Paris\t2024-01-01\n\
                          2643743\tLondon\tLondon\t\t51.50853\t-0.12574\tP\tPPLC\tGB\t\tENG\t\t\t\t8961989\t\t25\tEurope/London\t2024-01-01\n";

    fn dataset(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lightcraft-geocode-cmd-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(GEONAMES_FILES[0]), CITIES).unwrap();
        std::fs::write(dir.join(GEONAMES_FILES[1]), "FR.11\tÎle-de-France\nGB.ENG\tEngland\n").unwrap();
        std::fs::write(dir.join(GEONAMES_FILES[2]), "#c\nFR\tFRA\t250\tFR\tFrance\nGB\tGBR\t826\tUK\tUnited Kingdom\n").unwrap();
        dir
    }

    fn set_gps(s: &mut Session, id: PhotoId, gps: Option<(f64, f64)>, city: &str) {
        let mut meta = s.catalog.photo(id).unwrap().meta.clone();
        meta.gps = gps;
        meta.city = city.into();
        meta.state.clear();
        meta.country.clear();
        meta.location.clear();
        s.commit("test", Op::SetMeta { id, meta: Box::new(meta) }).unwrap();
    }

    #[test]
    fn fills_empty_fields_keeps_typed_ones_and_undoes() {
        let mut s = Session::with_demo();
        let ids = s.visible_cloned();
        let dir = dataset("fill");
        set_gps(&mut s, ids[0], Some((48.86, 2.34)), "");
        set_gps(&mut s, ids[1], Some((51.5, -0.1)), "Westminster");
        set_gps(&mut s, ids[2], Some((f64::NAN, 0.0)), "");
        set_gps(&mut s, ids[3], Some((0.0, 0.0)), "");
        let r = s.execute("metadata.reverseGeocode", &json!({"ids": [ids[0].0, ids[1].0, ids[2].0, ids[3].0], "dataset": dir})).unwrap();
        assert_eq!((r["looked"].as_u64(), r["found"].as_u64(), r["changed"].as_u64()), (Some(3), Some(2), Some(2)), "{r}");
        let m0 = s.catalog.photo(ids[0]).unwrap().meta.clone();
        assert_eq!((m0.city.as_str(), m0.state.as_str(), m0.country.as_str()), ("Paris", "Île-de-France", "France"));
        let m1 = s.catalog.photo(ids[1]).unwrap().meta.clone();
        assert_eq!((m1.city.as_str(), m1.state.as_str(), m1.country.as_str()), ("Westminster", "England", "United Kingdom"));
        assert!(s.activity.list().is_empty(), "the activity row ends with the command");
        // one undo step
        s.undo_step().unwrap();
        assert!(s.catalog.photo(ids[0]).unwrap().meta.city.is_empty());
        assert_eq!(s.catalog.photo(ids[1]).unwrap().meta.city, "Westminster");
        // overwrite replaces typed text
        s.execute("metadata.reverseGeocode", &json!({"ids": [ids[1].0], "dataset": dir, "overwrite": true})).unwrap();
        assert_eq!(s.catalog.photo(ids[1]).unwrap().meta.city, "London");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn explains_missing_data_and_gps() {
        let mut s = Session::with_demo();
        let ids = s.visible_cloned();
        set_gps(&mut s, ids[0], None, "");
        let e = s.execute("metadata.reverseGeocode", &json!({"ids": [ids[0].0]})).unwrap_err();
        assert!(e.to_string().contains("GPS"), "{e}");
        set_gps(&mut s, ids[0], Some((48.86, 2.34)), "");
        let missing = std::env::temp_dir().join(format!("lightcraft-geocode-none-{}", std::process::id()));
        let e = s.execute("metadata.reverseGeocode", &json!({"ids": [ids[0].0], "dataset": missing})).unwrap_err();
        assert!(e.to_string().contains("download"), "{e}");
        let st = s.execute("metadata.placeNamesStatus", &json!({"dataset": missing})).unwrap();
        assert_eq!(st["downloaded"], false);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn download_refuses_plain_http() {
        let mut s = Session::with_demo();
        assert!(s.execute("metadata.downloadPlaceNames", &json!({"dataset": "/nonexistent", "base": "http://example.com/"})).is_err());
    }
}
