//! Slideshow slides: `slideshow.layout` (validated, saved with the library) and
//! `slideshow.preview` (a slide composed with the layout, as a PNG).

use std::path::PathBuf;

use serde_json::json;

use crate::Session;

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-slide-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn png_size(bytes: &[u8]) -> (u32, u32) {
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "a PNG");
    let be = |o: usize| u32::from_be_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
    (be(16), be(20))
}

fn base64_decode(text: &str) -> Vec<u8> {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let (mut out, mut acc, mut bits) = (Vec::new(), 0u32, 0u32);
    for c in text.bytes().take_while(|c| *c != b'=') {
        acc = (acc << 6) | A.iter().position(|a| *a == c).unwrap() as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    out
}

#[test]
fn slide_layout_is_validated_and_persists_with_the_library() {
    let lib = temp_dir("layout");
    {
        let mut s = Session::new().with_fs();
        s.open_library(&lib, false).unwrap();
        let r = s.execute("slideshow.layout", &json!({})).unwrap();
        assert_eq!(r["marginLeft"], 0.0, "defaults: {r}");
        assert_eq!(r["backdropColor"], "#000000");
        let r = s
            .execute(
                "slideshow.layout",
                &json!({"marginTop": 0.1, "strokeBorder": true, "strokeColor": [255, 255, 255], "castShadow": true, "backdropColor": "#203040", "aspect": "4:3"}),
            )
            .unwrap();
        assert_eq!((r["marginLeft"].as_f64(), r["marginBottom"].as_f64()), (Some(0.1), Some(0.1)), "linked margins: {r}");
        assert_eq!(r["strokeColor"], "#ffffff");
        // a bad value changes nothing (the good keys beside it included)
        for bad in [json!({"marginLeft": "wide", "zoomToFill": true}), json!({"backdropColor": "red"}), json!({"glitter": 1})] {
            assert!(s.execute("slideshow.layout", &bad).is_err(), "{bad}");
        }
        assert!(!s.slide_layout.zoom_to_fill);
        // out of range: clamped
        let r = s.execute("slideshow.layout", &json!({"linkMargins": false, "marginRight": 9, "shadowOpacity": -1})).unwrap();
        assert_eq!((r["marginRight"].as_f64(), r["marginLeft"].as_f64(), r["shadowOpacity"].as_f64()), (Some(0.45), Some(0.1), Some(0.0)));
        drop(s);
    }
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    let r = s.execute("slideshow.layout", &json!({})).unwrap();
    assert_eq!((r["marginRight"].as_f64(), r["aspect"].as_str(), r["backdropColor"].as_str()), (Some(0.45), Some("4:3"), Some("#203040")));
    assert_eq!(r["linkMargins"], false);
    drop(s);
    // hand-edited values out of range are clamped when the library opens
    std::fs::write(lib.join("prefs.json"), r#"{"slideLayout": {"marginTop": 1e9, "shadowAngle": 720, "strokeWidth": -3}}"#).unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    assert_eq!((s.slide_layout.margin_top, s.slide_layout.shadow_angle, s.slide_layout.stroke_width), (0.45, 0.0, 0.0));
    // and the playback settings beside them are untouched by a layout change
    let before = s.slideshow.clone();
    s.execute("slideshow.layout", &json!({"zoomToFill": true})).unwrap();
    assert_eq!(s.slideshow, before);
    drop(s);
    let _ = std::fs::remove_dir_all(&lib);
}

#[test]
fn slide_preview_returns_the_composed_slide_at_the_asked_size() {
    let mut s = Session::with_demo();
    let photos = s.visible_cloned();
    s.execute("library.select", &json!({"ids": [photos[0].0]})).unwrap();
    s.execute("slideshow.layout", &json!({"margins": 0.1, "backdropColor": "#ff0000"})).unwrap();
    let r = s.execute("slideshow.preview", &json!({"width": 320, "height": 200})).unwrap();
    assert_eq!((r["width"].as_u64(), r["height"].as_u64(), r["id"].as_u64()), (Some(320), Some(200), Some(photos[0].0)));
    assert_eq!(r["cell"], json!([20.0, 20.0, 300.0, 180.0]), "10 % of the 200 px short side");
    let png = base64_decode(r["png"].as_str().unwrap());
    assert_eq!(png_size(&png), (320, 200));
    // written to a file instead
    let dir = temp_dir("preview");
    let path = dir.join("slide.png");
    let r = s.execute("slideshow.preview", &json!({"id": photos[1].0, "width": 240, "path": path.to_string_lossy()})).unwrap();
    assert_eq!(r["path"], path.to_string_lossy().as_ref());
    assert!(r.get("png").is_none());
    assert_eq!(png_size(&std::fs::read(&path).unwrap()), (240, 150), "the height from the aspect preview (screen: 16:10)");
    s.execute("slideshow.layout", &json!({"aspect": "16:9"})).unwrap();
    let r = s.execute("slideshow.preview", &json!({"height": 90})).unwrap();
    assert_eq!((r["width"].as_u64(), r["height"].as_u64()), (Some(160), Some(90)));
    // the pixels: red margins, the photo inside
    let img = s.slide_now(&s.slide_layout.clone(), photos[0], 320, 200).unwrap();
    assert_eq!((img.width, img.height), (320, 200));
    assert_eq!(img.get(3, 3), [255, 0, 0, 255]);
    assert_eq!(img.get(160, 196), [255, 0, 0, 255]);
    assert_ne!(img.get(160, 100), [255, 0, 0, 255], "the photo in the cell");
    // refused: bad sizes, a photo that isn't there
    for bad in [json!({"width": 4}), json!({"height": 100000}), json!({"width": "big"}), json!({"id": 999_999}), json!({"id": "one"})] {
        assert!(s.execute("slideshow.preview", &bad).is_err(), "{bad}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_background_image_and_the_slide_key() {
    let mut s = Session::with_demo();
    let photos = s.visible_cloned();
    let plain = s.slide_layout.clone();
    let job = s.slide_job(photos[0], 320, 200, None).unwrap();
    let loupe = s.loupe_job(photos[0], 320, 200, true).unwrap();
    assert_ne!(job.key, loupe.key, "a slide is not the photo's render");
    // a background image fills the slide around the photo
    let bg =
        s.slide_layout.merged(&json!({"margins": 0.2, "backdropColor": "#ff0000", "backgroundImage": photos[2].0, "backgroundOpacity": 1})).unwrap();
    let img = s.slide_now(&bg, photos[0], 320, 200).unwrap();
    assert_ne!(img.get(3, 3), [255, 0, 0, 255], "the background photo, not the colour");
    // it is rendered once and reused
    assert!(s.slide_backdrop.lock().unwrap().is_some());
    let other = s.slide_job_with(&bg, photos[1], 320, 200, None, false).unwrap();
    assert_ne!(other.key, s.slide_job_with(&plain, photos[1], 320, 200, None, false).unwrap().key);
    // a background photo that isn't in the catalog: the colour alone
    let gone = bg.merged(&json!({"backgroundImage": 987_654})).unwrap();
    let img = s.slide_now(&gone, photos[0], 320, 200).unwrap();
    assert_eq!(img.get(3, 3), [255, 0, 0, 255]);
}
