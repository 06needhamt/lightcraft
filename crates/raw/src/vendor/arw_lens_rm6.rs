//! Sony ARW lens corrections from the tables in the raw IFD, as the equivalent DNG `OpcodeList3` opcodes, so they
//! reach the same "profile corrections" (`optics.lens_profile`) as a DNG's embedded warp.
//!
//! These are the ILCE-7RM6 conventions, measured against the camera JPEG. [`super::arw_lens`] holds the ILCE-7RM4A
//! conventions, measured against Sony's Imaging Edge exports (unit 16384, the table indexed at the output radius).
//! Each is selected by camera model; no single unit fits every lens under either. See both docs:
//! docs/sony-ilce7rm6-lens-corrections.md (→ Two conventions) and docs/sony-lens-corrections.md.
//!
//! Sony records three count-prefixed signed 16-bit tables in the raw IFD, each with an in-camera on/off tag (ExifTool
//! Sony tag names): distortion [`DISTORTION_PARAMS`] (flag [`DISTORTION_FLAG`]), vignetting [`VIGNETTING_PARAMS`]
//! ([`VIGNETTING_FLAG`]) and lateral chromatic aberration [`CA_PARAMS`] ([`CA_FLAG`]). The first value is the knot
//! count: 16 / 16 / 32 in full-frame files of the ILCE-7RM6, 11 / 11 / 22 in its APS-C crop mode, the rest padding;
//! the CA table holds red's knots, then blue's.
//!
//! Their meaning was measured by registering blocks of the camera JPEG in each file on LightCraft's uncorrected render
//! (and raw data, for CA) over 54 ILCE-7RM6 photos with four lenses, 41 of them shot with in-camera distortion
//! correction on; nothing else (clean room). The conventions, with their worst residuals at a 1440 px long edge:
//! - knot `i` sits at sensor radius `i · s` from the default-crop centre, `s` = the body's full-frame default-crop
//!   half diagonal / 15 (the same pixel spacing in APS-C mode: the knots are fixed on the sensor); linear in between,
//!   the last segment extended beyond the last knot.
//! - distortion: a corrected radius `r` (px) reads the source radius `S(r) = r · (1 + v(r) / 15000)` (positive:
//!   pincushion), and the camera then zooms (output radius = `z` · corrected radius) so the output fills the default
//!   crop and reads only inside it: the largest `S(ρ/z)/ρ` over the crop border radii `ρ` is 1. 0.596 px over the 34
//!   measurable corrected photos (budget 0.75 px; corners and edge midpoints 0.570 px).
//! - `WarpRectilinear`'s 4-term odd polynomial fits that map within 2.35·10⁻⁴ of the DNG radius (0.220 px) on every
//!   photo, at its 65 fit samples. A table it fits worse than 0.25 px at the samples the crop shows is refused (the
//!   photo stays uncorrected rather than wrongly warped). Between samples the error is somewhat larger: 0.071 against
//!   0.049 px for the full-frame 16–35 mm example in the tests.
//! - vignetting: read but never applied. The camera JPEGs keep the uncorrected render's radial falloff (median
//!   corner/centre ratio 0.996 over 39 photos) and no radial gain model fits the tables.
//! - lateral CA: red and blue relative to green on the same knots (unit about 1/3.36·10⁶; 0.399 px, borderline);
//!   not applied yet.
//!
//! The correction is produced whatever the in-camera flag says. Only bodies listed in [`FULL_FRAME_CROP`] get it: the
//! unit and the knot spacing are verified on the ILCE-7RM6 alone. Another body is added by repeating the measurement
//! on its photos shot with distortion correction on and listing its full-frame default crop. The method and the
//! numbers: docs/sony-ilce7rm6-lens-corrections.md.

use super::fit_radial_warp;
use crate::opcodes::max_radius;
use crate::{Opcode, Rect};
use lightcraft_tiff::Ifd;
use std::ops::RangeInclusive;

pub(crate) const DISTORTION_PARAMS: u16 = 0x7037;
pub(crate) const VIGNETTING_PARAMS: u16 = 0x7032;
pub(crate) const CA_PARAMS: u16 = 0x7035;
pub(crate) const DISTORTION_FLAG: u16 = 0x7036;
pub(crate) const VIGNETTING_FLAG: u16 = 0x7031;
pub(crate) const CA_FLAG: u16 = 0x7034;

/// The bodies whose tables are verified (Exif `Model`), with their full-frame default crop (width, height) in px.
const FULL_FRAME_CROP: &[(&str, (f64, f64))] = &[("ILCE-7RM6", (9984.0, 6656.0))];
/// The distortion table's value of 1 (relative radius); a global least-squares fit over the 34 photos gives 15000.7.
const DISTORTION_UNIT: f64 = 15000.0;
/// Crop border radii scanned by the frame-filling zoom.
const ZOOM_SCAN: usize = 4096;
/// Largest polynomial fit error accepted at the fit samples the crop shows, px at a 1440 px long edge.
const FIT_ERROR_PX_AT_1440: f64 = 0.25;

/// A file's lens tables: the first `count` values of each (CA: red, blue); `None` when absent or malformed.
/// `vignetting` and `flags` are parsed for the tests and the docs only: don't build an opcode from them (the camera
/// applies no vignetting correction to its JPEGs, and the flags don't decide anything).
#[derive(Debug)]
// only the distortion table becomes an opcode so far
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct LensTables {
    pub distortion: Option<Vec<f64>>,
    pub vignetting: Option<Vec<f64>>,
    pub ca: Option<(Vec<f64>, Vec<f64>)>,
    /// In-camera on/off of distortion, vignetting and CA correction, as recorded.
    pub flags: [Option<i64>; 3],
}

/// The values of a count-prefixed integer table whose count is in `counts` and fits the array.
fn table(raw: &Ifd, tag: u16, counts: RangeInclusive<usize>) -> Option<Vec<f64>> {
    let v = raw.value(tag)?;
    let count = usize::try_from(v.get_i64(0)?).ok().filter(|n| counts.contains(n) && *n < v.count())?;
    (1..=count).map(|i| v.get_i64(i).map(|x| x as f64)).collect()
}

/// The lens tables of the raw IFD `raw`.
pub(crate) fn read_tables(raw: &Ifd) -> LensTables {
    let ca = table(raw, CA_PARAMS, 4..=32).filter(|v| v.len().is_multiple_of(2)).map(|v| {
        let (red, blue) = v.split_at(v.len() / 2);
        (red.to_vec(), blue.to_vec())
    });
    LensTables {
        distortion: table(raw, DISTORTION_PARAMS, 2..=16),
        vignetting: table(raw, VIGNETTING_PARAMS, 2..=16),
        ca,
        flags: [raw.i64(DISTORTION_FLAG), raw.i64(VIGNETTING_FLAG), raw.i64(CA_FLAG)],
    }
}

/// The knot spacing (px) of a verified body: its full-frame default-crop half diagonal / 15.
fn knot_spacing(model: &str) -> Option<f64> {
    let model = model.trim();
    FULL_FRAME_CROP.iter().find(|(m, _)| *m == model).map(|&(_, (w, h))| w.hypot(h) / 30.0)
}

/// The knot table at radius `r` px: knot `i` at `i · spacing`, linear in between, the last segment extended beyond
/// the last knot. NaN with fewer than two knots.
fn interpolate(knots: &[f64], spacing: f64, r: f64) -> f64 {
    let t = r / spacing;
    // `as` saturates, so a huge `t` lands on the last segment; NaN fails `t > 0`
    let i = if t > 0.0 { (t as usize).min(knots.len().saturating_sub(2)) } else { 0 };
    match (knots.get(i), knots.get(i + 1)) {
        (Some(a), Some(b)) => a + (b - a) * (t - i as f64),
        _ => f64::NAN,
    }
}

/// The camera's frame-filling zoom `z` for the source radius map `source` and a crop of half size `x × y`: the
/// largest `source(ρ/z)/ρ` over the border radii `ρ` in [min(x, y), hypot(x, y)] is 1, so the output crop reads
/// only inside the source crop and touches it (at the corner for pincushion, the short-edge midpoint for barrel).
/// Bisection over [0.5, 2]; `None` when that range holds no solution or a ratio is not finite.
fn fill_zoom(source: &dyn Fn(f64) -> f64, x: f64, y: f64) -> Option<f64> {
    let (near, far) = (x.min(y), x.hypot(y));
    let overshoots = |z: f64| -> Option<bool> {
        let mut worst = f64::NEG_INFINITY;
        for k in 0..=ZOOM_SCAN {
            let rb = near + k as f64 * (far - near) / ZOOM_SCAN as f64;
            let ratio = source(rb / z) / rb;
            if !ratio.is_finite() {
                return None;
            }
            worst = worst.max(ratio);
        }
        Some(worst > 1.0)
    };
    let (mut lo, mut hi) = (0.5, 2.0);
    if !overshoots(lo)? || overshoots(hi)? {
        return None;
    }
    for _ in 0..100 {
        let z = 0.5 * (lo + hi);
        if overshoots(z)? { lo = z } else { hi = z }
    }
    Some(0.5 * (lo + hi))
}

/// The distortion table as a one-plane `WarpRectilinear` relative to the active area `active` (centred on the
/// default crop `crop`, as the camera corrects), with knots `knot_spacing` px apart. `None` when the table is absent
/// or all zero, the zoom or the fit fails, or the fit is worse than 0.25 px at a 1440 px long edge at one of the fit
/// samples within the crop (a padded active area, as in lossless files, reaches radii that are never shown).
pub(crate) fn distortion_opcode(t: &LensTables, knot_spacing: f64, active: Rect, crop: Rect) -> Option<Opcode> {
    let v = t.distortion.as_deref().filter(|v| v.iter().any(|&x| x != 0.0))?;
    let (aw, ah) = (active.width as f64, active.height as f64);
    if aw < 2.0 || ah < 2.0 || crop.width == 0 || crop.height == 0 {
        return None;
    }
    let (cw, ch) = (crop.width as f64, crop.height as f64);
    let source = |r: f64| r * (1.0 + interpolate(v, knot_spacing, r) / DISTORTION_UNIT);
    let z = fill_zoom(&source, cw / 2.0, ch / 2.0)?;
    // DNG's normalisation: pixel-index units, radius 1 = the farthest active-area corner
    let (cx, cy) = (crop.x as f64 + (cw - 1.0) / 2.0, crop.y as f64 + (ch - 1.0) / 2.0);
    let m = max_radius(cx, cy, aw - 1.0, ah - 1.0);
    let max_fit_error = FIT_ERROR_PX_AT_1440 * (cw.max(ch) / 1440.0) / m;
    let shown = (cw / 2.0).hypot(ch / 2.0) / m;
    let [k0, k1, k2, k3] = fit_radial_warp(&|u| source(u * m / z) / m, max_fit_error, shown)?;
    Some(Opcode::WarpRectilinear { planes: vec![[k0, k1, k2, k3, 0.0, 0.0]], center: [cx / (aw - 1.0), cy / (ah - 1.0)] })
}

/// The `OpcodeList3` lens corrections of a Sony raw IFD for the body `model` (Exif `Model`); empty for a body whose
/// tables are not verified.
pub(crate) fn lens_opcodes(raw: &Ifd, model: &str, active: Rect, crop: Rect) -> Vec<Opcode> {
    let Some(spacing) = knot_spacing(model) else { return Vec::new() };
    distortion_opcode(&read_tables(raw), spacing, active, crop).into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_tiff::{Entry, Value};

    /// The FF 16–35 mm example (corpus `arw-sony-ilce7rm6-ff-land-lossless.arw`, docs/sony-ilce7rm6-lens-corrections.md).
    const FF_DISTORTION: [i16; 17] = [16, 0, 2, 7, 16, 28, 43, 61, 83, 109, 139, 175, 217, 267, 325, 395, 479];
    const FF_VIGNETTING: [i16; 17] = [16, 0, 80, 336, 672, 1120, 1648, 2240, 2880, 3600, 4336, 5104, 5872, 6640, 7392, 8160, 8976];
    const FF_CA: [i16; 33] = [
        32, 242, 234, 224, 212, 196, 176, 154, 132, 108, 86, 66, 46, 30, 16, 8, 4, -276, -258, -238, -212, -184, -148, -114, -74, -40, -6, 20, 42,
        56, 60, 54, 32,
    ];
    /// The APS-C example (corpus `arw6-sony-ilce7rm6-apsc-land-hq.arw`): counts 11 / 11 / 22, then padding.
    const APSC_DISTORTION: [i16; 17] = [11, 0, -10, -36, -78, -134, -202, -277, -356, -435, -512, -583, -583, -583, -583, -583, -583];
    const APSC_VIGNETTING: [i16; 17] = [11, 0, 48, 208, 496, 880, 1360, 1952, 2624, 3568, 4832, 6176, 0, 0, 0, 0, 0];
    const APSC_CA: [i16; 33] = [
        22, 790, 764, 734, 702, 666, 624, 582, 544, 510, 484, 470, -432, -404, -372, -330, -286, -230, -174, -114, -58, -6, 34, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0,
    ];
    /// ARW6 geometry (the decoder's raw image size and default crop).
    const FF_ACTIVE: Rect = Rect { x: 0, y: 0, width: 10016, height: 6672 };
    const FF_CROP: Rect = Rect { x: 12, y: 8, width: 9984, height: 6656 };
    const APSC_ACTIVE: Rect = Rect { x: 0, y: 0, width: 6592, height: 4372 };
    const APSC_CROP: Rect = Rect { x: 16, y: 10, width: 6528, height: 4352 };

    fn knots() -> f64 {
        knot_spacing("ILCE-7RM6").unwrap()
    }

    fn ifd(tags: Vec<(u16, Value)>) -> Ifd {
        let mut entries: Vec<Entry> = tags.into_iter().map(|(tag, value)| Entry { tag, value, offset: 0 }).collect();
        entries.sort_by_key(|e| e.tag);
        Ifd { entries, ..Default::default() }
    }

    fn sony_lens_ifd(distortion: &[i16], vignetting: &[i16], ca: &[i16], flags: [u16; 3]) -> Ifd {
        ifd(vec![
            (DISTORTION_PARAMS, Value::SShort(distortion.to_vec())),
            (VIGNETTING_PARAMS, Value::SShort(vignetting.to_vec())),
            (CA_PARAMS, Value::SShort(ca.to_vec())),
            (DISTORTION_FLAG, Value::Short(vec![flags[0]])),
            (VIGNETTING_FLAG, Value::Short(vec![flags[1]])),
            (CA_FLAG, Value::Short(vec![flags[2]])),
        ])
    }

    fn tables_with_distortion(v: &[f64]) -> LensTables {
        LensTables { distortion: Some(v.to_vec()), vignetting: None, ca: None, flags: [None; 3] }
    }

    fn values(table: &[i16]) -> Vec<f64> {
        table[1..=table[0] as usize].iter().map(|&v| v as f64).collect()
    }

    /// The warp's one plane and centre against the recipe's numbers (docs/sony-ilce7rm6-lens-corrections.md).
    fn assert_warp(op: Option<Opcode>, center: [f64; 2], k: [f64; 4]) {
        let Some(Opcode::WarpRectilinear { planes, center: c }) = op else { panic!("no warp: {op:?}") };
        assert_eq!(planes.len(), 1);
        assert!((c[0] - center[0]).abs() < 1e-9 && (c[1] - center[1]).abs() < 1e-9, "centre {c:?}, want {center:?}");
        let p = planes[0];
        for (i, want) in k.iter().enumerate() {
            assert!((p[i] - want).abs() < 1e-6, "k{i} {}, want {want} ({p:?})", p[i]);
        }
        assert_eq!((p[4], p[5]), (0.0, 0.0), "radial only");
    }

    #[test]
    fn reads_the_three_tables_and_flags() {
        let t = read_tables(&sony_lens_ifd(&FF_DISTORTION, &FF_VIGNETTING, &FF_CA, [0, 2, 1]));
        let d = t.distortion.unwrap();
        assert_eq!((d.len(), d[0], d[15]), (16, 0.0, 479.0));
        let v = t.vignetting.unwrap();
        assert_eq!((v.len(), v[1], v[15]), (16, 80.0, 8976.0));
        let (red, blue) = t.ca.unwrap();
        assert_eq!((red.len(), red[0], red[15]), (16, 242.0, 4.0));
        assert_eq!((blue.len(), blue[0], blue[15]), (16, -276.0, 32.0));
        assert_eq!(t.flags, [Some(0), Some(2), Some(1)]);
    }

    #[test]
    fn apsc_tables_use_only_the_first_count_values() {
        let t = read_tables(&sony_lens_ifd(&APSC_DISTORTION, &APSC_VIGNETTING, &APSC_CA, [0, 2, 1]));
        let d = t.distortion.unwrap();
        assert_eq!((d.len(), d[10]), (11, -583.0));
        assert_eq!(t.vignetting.unwrap().len(), 11);
        let (red, blue) = t.ca.unwrap();
        assert_eq!((red.len(), red[10]), (11, 470.0));
        assert_eq!((blue.len(), blue[0], blue[10]), (11, -432.0, 34.0));
    }

    #[test]
    fn missing_or_malformed_tables_yield_no_opcode() {
        let mut short = FF_DISTORTION.to_vec();
        short.pop();
        let mut count_17 = FF_DISTORTION.to_vec();
        count_17[0] = 17;
        count_17.push(500);
        let mut too_large = FF_DISTORTION.to_vec();
        too_large[0] = 600;
        for (what, table) in [
            ("count 15 with 3 values", vec![15, 0, 2, 7]),
            ("count 17", count_17),
            ("empty", vec![]),
            ("count 16 with 15 values", short),
            ("count 1", vec![1, 5]),
            ("count 0", vec![0, 5, 6]),
            ("negative count", vec![-16, 0, 2, 7]),
            ("count larger than the array", too_large),
        ] {
            let ifd = ifd(vec![(DISTORTION_PARAMS, Value::SShort(table))]);
            let t = read_tables(&ifd);
            assert!(t.distortion.is_none(), "{what}");
            assert!(distortion_opcode(&t, knots(), FF_ACTIVE, FF_CROP).is_none(), "{what}");
            assert!(lens_opcodes(&ifd, "ILCE-7RM6", FF_ACTIVE, FF_CROP).is_empty(), "{what}");
        }
        // not an integer array, or absent
        for value in [Value::Ascii("16 0 2 7".into()), Value::Double(vec![2.0, 0.0, 1.0])] {
            assert!(read_tables(&ifd(vec![(DISTORTION_PARAMS, value)])).distortion.is_none());
        }
        let none = read_tables(&ifd(vec![]));
        assert!(none.distortion.is_none() && none.vignetting.is_none() && none.ca.is_none() && none.flags == [None; 3]);
        // the CA table needs an even count of 4..=32
        for ca in [vec![3, 1, 2, 3], vec![34; 35], vec![2, 1, 2]] {
            assert!(read_tables(&ifd(vec![(CA_PARAMS, Value::SShort(ca))])).ca.is_none());
        }
    }

    #[test]
    fn hostile_tables_are_rejected() {
        let folded = [0.0, 2000.0, 1000.0, 3000.0, 2000.0, 4000.0, 3000.0, 5000.0, 4000.0, 6000.0, 5000.0, 7000.0, 6000.0, 8000.0, 7000.0, 9000.0];
        for (what, table) in [
            ("huge", vec![(i64::MAX / 4) as f64; 16]),
            ("most negative", vec![i64::MIN as f64; 16]),
            ("zig-zag", folded.to_vec()),
            ("all zero", vec![0.0; 16]),
            ("one knot", vec![100.0]),
            ("no knots", vec![]),
            ("not finite", vec![0.0, f64::NAN, 20.0]),
            ("infinite", vec![0.0, f64::INFINITY, 20.0]),
        ] {
            assert!(distortion_opcode(&tables_with_distortion(&table), knots(), FF_ACTIVE, FF_CROP).is_none(), "{what}");
        }
        // the same through the IFD, as 64-bit values
        for v in [i64::MAX / 4, i64::MIN, i64::MAX] {
            let mut table = vec![v; 17];
            table[0] = 16;
            let ifd = ifd(vec![(DISTORTION_PARAMS, Value::SLong8(table))]);
            assert!(lens_opcodes(&ifd, "ILCE-7RM6", FF_ACTIVE, FF_CROP).is_empty(), "{v}");
        }
        // degenerate geometry
        let ff = tables_with_distortion(&values(&FF_DISTORTION));
        for (what, knots, active, crop) in [
            ("empty crop", knots(), FF_ACTIVE, Rect::new(12, 8, 0, 6656)),
            ("1 × 1 active area", knots(), Rect::new(0, 0, 1, 1), Rect::new(0, 0, 1, 1)),
            ("knot spacing NaN", f64::NAN, FF_ACTIVE, FF_CROP),
            ("knot spacing 0", 0.0, FF_ACTIVE, FF_CROP),
        ] {
            assert!(distortion_opcode(&ff, knots, active, crop).is_none(), "{what}");
        }
    }

    #[test]
    fn ff_example_table_becomes_the_measured_warp() {
        let t = tables_with_distortion(&values(&FF_DISTORTION));
        assert_warp(
            distortion_opcode(&t, knots(), FF_ACTIVE, FF_CROP),
            [0.4996005991, 0.5],
            [0.9714495823, 0.022508032, 0.0006090085237, 0.005683284548],
        );
    }

    #[test]
    fn apsc_example_table_becomes_the_measured_warp() {
        let t = tables_with_distortion(&values(&APSC_DISTORTION));
        let op = distortion_opcode(&t, knots(), APSC_ACTIVE, APSC_CROP);
        assert_warp(op, [0.4975724473, 0.5], [1.01627551, -0.0618606692, 0.02768349621, -0.005595798468]);
    }

    /// The lossless files' padded active area (10240 × 7168) puts the farthest active-area corner (DNG radius 1) well
    /// beyond the crop's (u 0.925): the gate looks only at the radii the crop shows. The FF example × 1.5 fits within
    /// 0.12 px at 1440 at the samples there, 0.27 px at all 65.
    #[test]
    fn padded_active_area_gates_only_the_displayed_radii() {
        let strong: Vec<f64> = values(&FF_DISTORTION).iter().map(|v| v * 1.5).collect();
        let padded = Rect { x: 0, y: 0, width: 10240, height: 7168 };
        let op = distortion_opcode(&tables_with_distortion(&strong), knots(), padded, FF_CROP);
        assert!(matches!(op, Some(Opcode::WarpRectilinear { .. })), "{op:?}");
    }

    #[test]
    fn unknown_model_gets_no_opcodes() {
        let ifd = sony_lens_ifd(&FF_DISTORTION, &FF_VIGNETTING, &FF_CA, [0, 2, 1]);
        assert!(lens_opcodes(&ifd, "ILCE-7M4", FF_ACTIVE, FF_CROP).is_empty());
        assert!(lens_opcodes(&ifd, "", FF_ACTIVE, FF_CROP).is_empty());
        // the camera's in-camera flag (0 = off here) doesn't matter: the table is applied
        let ops = lens_opcodes(&ifd, " ILCE-7RM6 ", FF_ACTIVE, FF_CROP);
        assert_eq!(ops, distortion_opcode(&read_tables(&ifd), knots(), FF_ACTIVE, FF_CROP).into_iter().collect::<Vec<_>>());
        assert_eq!(ops.len(), 1);
    }
}
