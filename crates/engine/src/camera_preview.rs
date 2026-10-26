//! Estimate the starting look of a raw without a camera colour matrix (Sony ARW, Nikon NEF, Panasonic
//! RW2, Fujifilm RAF, Canon CR2/CR3, Pentax PEF, Samsung SRW, Olympus ORF) from its own JPEG. Colour and luminance are fitted separately; the JPEG supplies correspondences only,
//! never output pixels or a replacement for RAW editing.
//! A global matrix can't follow the camera's hue-dependent rendering (the best matrix rendered a
//! lime shirt olive that the camera kept lime): a hue/saturation table fitted to the residuals
//! (applied like a DNG `ProfileHueSatMap`) corrects that when it also improves the held-out pixels.
use lightcraft_color::{D50, D65, Mat3, PROPHOTO, REC2020, Xy, bradford, luminance_2020};
use lightcraft_pipeline::tone::{CameraTone, ToneMap};
use lightcraft_raster::{
    Rgb32f,
    resample::{Filter, fit, resize},
};
use lightcraft_raw::{
    ColorData, RawFormat, RawImage,
    color::CameraTransform,
    profile::{HsvTable, ProfileLook, ToneCurve},
};

/// The version of the camera-look fit: how a raw's colour matrix, hue/saturation table and tone
/// and chroma curves are chosen. Smart previews record it in their header (`look_version`), because
/// they keep the curve and the colour the fit produced; one written by an older version is rebuilt
/// from its original when that is online, and kept as it is while it is offline.
///
/// **Bump this whenever a change makes the fit give a different result for the same file** (the
/// fit, its gates, the fallbacks, the camera profiles' effect on it). Do not bump it for changes
/// that leave the fitted look alone. `RENDER_CACHE_VERSION` covers renders; this covers proxies.
///
/// 1: the fit as of #499's follow-up; 2: Sony DRO (tone curve lowered to Sony's curve without DRO)
/// and the ILCE-7CR profile (#528, #583, #568, #616); 3: the camera JPEG read in the colour space the raw's EXIF
/// states, for every make (#778).
/// 4: a tone curve from black-and-white camera JPEGs (#577).
/// 5: dark JPEG luminance pairs for the camera toe (#1307).
/// 6: the ILCA-99M2 profile.
/// 7: the sensor proxy follows the file's lens warp when the camera JPEG is lens-corrected.
/// 8: the bundled NIKON Z 6_2 profile.
/// 9: the bundled NIKON Z 8 and NIKON D5300 profiles.
/// 10: DNG tone fit for profiles with a look table and for 1024 px (Lightroom) previews.
/// 12: tone curves of low-contrast scenes (range limit 1.2, with the bright-area check for curves under 1.5, #699);
/// a tone-only fallback for near-black scenes with too few colour pairs (#1587); DNG tone fitting uses a
/// lens-corrected proxy when the file carries lens opcodes; validated A7CR distortion, including the lens data
/// stored with each smart preview.
pub const LOOK_VERSION: u32 = 12;

#[derive(Clone, Debug)]
pub(crate) struct CameraLook {
    pub matrix: Mat3,
    pub tone: CameraTone,
    /// Hue/saturation correction after `matrix`, in linear ProPhoto RGB (DNG `ProfileHueSatMap`).
    pub hue_sat: Option<HsvTable>,
}

/// Long edge of the sensor/JPEG proxy a single photo's look is fitted on (the acceptance gates
/// below were set at this size).
const PROXY: usize = 96;
/// Long edge of the proxies pooled for a camera profile: small objects (a shirt) get 4× the samples.
pub(crate) const PROFILE_PROXY: usize = 192;

/// Raw formats whose decoder supplies vendor white-balance multipliers but no camera colour matrix:
/// their starting look is fitted to the file's own JPEG, and white balance is relative to the
/// as-shot look (`docs/camera-preview-colour.md`). The catalog's `Photo::relative_wb` matches the
/// same formats by file extension.
pub(crate) fn file_local_look(format: RawFormat) -> bool {
    matches!(
        format,
        RawFormat::Arw
            | RawFormat::Nef
            | RawFormat::Nrw
            | RawFormat::Rw2
            | RawFormat::Raf
            | RawFormat::Cr3
            | RawFormat::Cr2
            | RawFormat::Pef
            | RawFormat::Srw
            | RawFormat::Orf
            | RawFormat::Mrw
            | RawFormat::Erf
    )
}

/// The colour a raw starts from, first available first: the file's own colour matrices (DNG); a look fitted to the
/// file's JPEG ([`fit_preview`]: with the camera profile for its model when there is one, else with colour fitted to
/// this photo alone); the model's colour matrices from measured spectral sensitivities (`lightcraft_raw::spectral`);
/// the neutral fallback. The spectral matrices only stand in for the neutral fallback, so a photo whose JPEG fit is
/// accepted renders as without them; when used they are written into `raw.color`. A photo whose JPEG is black and
/// white (a monochrome picture style) has no colour to fit, but still gives its tone curve: with the camera profile's
/// colour in [`fit_preview`], else on top of the spectral matrices or the fallback ([`monochrome_look`]). Returns the
/// as-shot white, the camera transform at it and the fitted look.
/// `lens` is the file's own lens-correction warp ([`lens_geometry`], read before the caller removes it from
/// `OpcodeList3`): the camera applied it to its JPEG, so the sensor proxy is warped the same way before pairing.
/// `embedded` is the file's embedded lens vignetting, for the DNG tone fit.
pub(crate) fn starting_colour(
    raw: &mut RawImage,
    bytes: &[u8],
    lens: Option<&LensGeometry>,
    embedded: Option<&lightcraft_develop::EmbeddedLens>,
) -> (Xy, CameraTransform, Option<CameraLook>) {
    let (xy, t, look) = starting_colour_with(
        raw,
        |raw, t| fit_preview(raw, bytes, t, lens),
        |raw, t| monochrome_look(raw, bytes, t, lens),
        |raw, t| {
            if !file_local_look(raw.format) {
                return None;
            }
            let (sensor, reference, clipped) = proxies(raw, bytes, t, PROXY, 0.0, lens)?;
            fit_dark_tone(&sensor, &reference, &clipped)
        },
    );
    if look.is_none() && dng_tone_eligible(raw, &t) {
        let look = fit_dng_tone(raw, bytes, &t, embedded);
        return (xy, t, look);
    }
    (xy, t, look)
}

/// Whether the profile's tone curve is absent or the identity (within 0.005 over the range).
fn identity_tone_curve(curve: Option<&lightcraft_raw::profile::ToneCurve>) -> bool {
    curve.is_none_or(|c| {
        (0..=32).all(|i| {
            let x = i as f32 / 32.0;
            (c.eval(x) - x).abs() < 0.005
        })
    })
}

/// A DNG with its own colour model whose tone is not (yet) defined by the file: a matrix, no
/// non-identity `ProfileToneCurve` and no `ProfileGainTableMap`. A `LookTable` does not define the tone: an
/// Adobe Standard profile carries one without a tone curve and is rendered with Lightroom's default curve on
/// top (Lightroom Classic's lossy DNGs rendered flat and dull without a fit), and the fit applies the tables
/// to its proxy, so it fits only the tone around them.
pub(crate) fn dng_tone_eligible(raw: &RawImage, t: &CameraTransform) -> bool {
    let p = &raw.color.profile;
    raw.format == RawFormat::Dng && !t.matrix_is_fallback && identity_tone_curve(p.tone_curve.as_ref()) && p.gain_table_map.is_none()
}

/// [`starting_colour`] with the JPEG fit (the look, and whether the JPEG is black and white) and the black-and-white
/// JPEG's tone fit and the near-black tone fallback passed in (tests pin the order with them).
fn starting_colour_with(
    raw: &mut RawImage,
    fit: impl FnOnce(&RawImage, &CameraTransform) -> (Option<CameraLook>, bool),
    monochrome: impl FnOnce(&RawImage, &CameraTransform) -> Option<CameraLook>,
    dark: impl FnOnce(&RawImage, &CameraTransform) -> Option<CameraLook>,
) -> (Xy, CameraTransform, Option<CameraLook>) {
    let start = |raw: &RawImage| {
        let xy = lightcraft_raw::color::as_shot_white_xy(raw);
        (xy, lightcraft_raw::color::camera_transform(raw, xy))
    };
    let (xy, t) = start(raw);
    if !t.matrix_is_fallback {
        return (xy, t, None);
    }
    let (look, black_and_white) = fit(raw, &t);
    if let Some(look) = look {
        return (xy, t, Some(look));
    }
    let (xy, t) = match raw.metadata.model.as_deref().and_then(|model| lightcraft_raw::spectral::find(raw.metadata.make.as_deref(), model)) {
        Some(camera) => {
            camera.fill(&mut raw.color);
            if lightcraft_pipeline::profiling() {
                eprintln!("[profile] {:?} camera colour from spectral sensitivities: {} {}", raw.format, camera.make, camera.model);
            }
            start(raw)
        }
        None => (xy, t),
    };
    // A black-and-white JPEG first tries its existing tone fit on the chosen colour.
    let look = if black_and_white { monochrome(raw, &t) } else { None };
    (xy, t, look.or_else(|| dark(raw, &t)))
}

/// The colour a raw starts from ([`starting_colour`]) as DNG colour tags, for a DNG of the raw's camera RGB (Photo
/// Merge, issue #965): without them a merge of CR2s, whose colour comes from a look fitted to their JPEG, rendered
/// green and dark. `None` when the file has colour matrices of its own (a DNG keeps them).
pub(crate) fn starting_colour_tags(raw: &mut RawImage, bytes: &[u8]) -> Option<ColorData> {
    if lightcraft_raw::color::has_matrix(&raw.color) {
        return None;
    }
    // the white the source shows, decided before `starting_colour` may fill in measured matrices
    let scale = crate::files::WbScaleOf::new(raw.format, &raw.color, raw.wb_multipliers, &raw.metadata);
    let white = scale.dng_white(&raw.color, lightcraft_raw::color::as_shot_white_xy(raw));
    // (a merge frame arrives without its lens warp: `lightcraft_merge` removes it before calling)
    let (_, t, look) = starting_colour(raw, bytes, None, None);
    dng_colour(&raw.color, &white, &t, look.as_ref())
}

/// Give a raw about to be written as a DNG of its own (Export as DNG, Convert / Copy as DNG) the colour it opens
/// with, when the file has no colour matrices (issue #1745: a Z 9 NEF's DNG had no `ColorMatrix1`, and other raw
/// converters refused it). The lens corrections stay in the DNG; only the fit sees the raw without them, as when
/// the photo opens.
pub(crate) fn with_starting_colour(raw: &mut RawImage, bytes: &[u8]) {
    if lightcraft_raw::color::has_matrix(&raw.color) {
        return;
    }
    let list3 = raw.opcodes.list3.clone();
    raw.opcodes.list3.retain(|op| !op.is_lens_correction());
    let tags = starting_colour_tags(raw, bytes);
    raw.opcodes.list3 = list3;
    if let Some(tags) = tags {
        raw.color = tags;
    }
}

/// DNG tags that render white-balanced camera RGB at its as-shot white as `t` with `look` on top does: one forward
/// matrix (with a colour matrix and as-shot neutral that agree with it at that white), the look's hue/saturation
/// table as `ProfileHueSatMap` and its tone curve as `ProfileToneCurve`. Not quite the same render: a DNG table
/// doesn't keep each pixel's luminance as the fitted one does ([`HueSat`]), and the chroma curve has no tag; on two
/// CC0 CR2s (EOS 550D, 40D) an HDR merge of three copies renders within mean CIE76 ΔE 1.07 and 0.47 of the photo
/// (1.47 and 0.66 without the table; 26 and 23 with no colour tags at all).
/// The colour matrices declare `declared`, the white balance the source shows ([`crate::files::WbScaleOf::dng_white`]),
/// so the DNG opens at the source's As Shot temperature and tint and a white-balance edit starts from it. Matrices
/// derived from `forward` put the declared white wherever the look and the generic camera model took it: ARW merges
/// opened at about 7000 K / +115 where the ARWs show 4800 K / +8, and any white-balance edit turned them green.
/// `forward` is used only with the as-shot neutral, so the render at As Shot doesn't depend on them.
/// `None` when the white balance or the matrix can't give tags.
pub(crate) fn dng_colour(tags: &ColorData, declared: &crate::files::DngWhite, t: &CameraTransform, look: Option<&CameraLook>) -> Option<ColorData> {
    let m = look.map_or(t.matrix, |l| l.matrix.mul(&t.matrix));
    // the DNG model renders the camera's white at unit brightness: what a look does to white's brightness goes
    // into the baseline exposure
    let white = m.apply([1.0; 3]);
    let k = (white[0] + white[1] + white[2]) / 3.0;
    if !(k.is_finite() && k > 1e-6 && t.wb.iter().all(|w| w.is_finite() && *w > 0.0)) {
        return None;
    }
    let m = Mat3(m.0.map(|r| r.map(|v| v / k)));
    let forward = bradford(D65, D50).mul(&REC2020.to_xyz()).mul(&m);
    let wb = t.wb.map(f64::from);
    let neutral = wb.map(|w| wb.iter().cloned().fold(f64::MAX, f64::min) / w);
    let (illuminant, color_matrix) = match declared {
        crate::files::DngWhite::Matrices(c) if lightcraft_raw::color::has_matrix(c) => (c.illuminant, c.color_matrix),
        crate::files::DngWhite::Matrices(_) => ([21, 0], [Some(declaring(t.white_xy, neutral)?), None]),
        crate::files::DngWhite::At(xy) => ([21, 0], [Some(declaring(*xy, neutral)?), None]),
    };
    if !color_matrix.iter().flatten().all(|m| m.0.iter().flatten().all(|v| v.is_finite())) {
        return None;
    }
    Some(ColorData {
        illuminant,
        color_matrix,
        // a second colour matrix comes with a second forward matrix (DNG spec): the same one, fitted at one white
        forward_matrix: [Some(forward), color_matrix[1].map(|_| forward)],
        camera_calibration: [None, None],
        analog_balance: None,
        as_shot_neutral: Some(neutral),
        as_shot_white_xy: None,
        baseline_exposure: t.baseline_exposure + k.log2(),
        baseline_sharpness: tags.baseline_sharpness,
        kelvin_presets: None,
        profile: ProfileLook {
            hue_sat_map: [look.and_then(|l| l.hue_sat.clone()), None],
            tone_curve: look.and_then(|l| dng_tone_curve(&l.tone)),
            ..Default::default()
        },
    })
}

/// An XYZ → camera colour matrix under which camera `neutral` is white `xy`: the generic camera model, scaled per
/// channel. `None` when the model gives no positive neutral there.
fn declaring(xy: Xy, neutral: [f64; 3]) -> Option<Mat3> {
    let g = lightcraft_raw::color::xyz_to_camera(&ColorData::default(), xy);
    let n = g.apply(xy.to_xyz());
    if !n.iter().all(|v| v.is_finite() && *v > 1e-9) {
        return None;
    }
    Some(Mat3::diag(neutral[0] / n[0], neutral[1] / n[1], neutral[2] / n[2]).mul(&g))
}

/// A camera tone curve as a DNG `ProfileToneCurve` (linear in, linear out, 1 = white): sampled densely over the
/// 12 stops below white, the range the curve is read back over (`files::dng_tone_curve`).
fn dng_tone_curve(tone: &CameraTone) -> Option<ToneCurve> {
    const N: usize = 256;
    let samples = (0..=N).map(|i| {
        let x = 2f32.powf(-12.0 + 12.0 * i as f32 / N as f32);
        [x, tone.apply(x).min(1.0)]
    });
    let flat: Vec<f64> = std::iter::once([0.0, 0.0]).chain(samples).flatten().map(f64::from).collect();
    ToneCurve::from_tag(&flat)
}

/// The look fitted to the file's camera JPEG, and whether that JPEG is black and white ([`monochrome`]).
pub(crate) fn fit_preview(raw: &RawImage, bytes: &[u8], transform: &CameraTransform, lens: Option<&LensGeometry>) -> (Option<CameraLook>, bool) {
    if !transform.matrix_is_fallback || !file_local_look(raw.format) {
        return (None, false);
    }
    let Some((sensor, reference, clipped)) = proxies(raw, bytes, transform, PROXY, 0.0, lens) else { return (None, false) };
    let black_and_white = monochrome(&reference);
    // A camera profile pooled from many photos knows colours this photo shows too little of;
    // try its colour first and fit tone/chroma per photo (picture styles vary).
    let profile = raw.metadata.model.as_deref().and_then(crate::camera_profiles::get);
    let colour = profile.as_ref().and_then(|p| Some((p.matrix().mul(&transform.matrix.inverse()?), p.hue_sat.clone())));
    // Sony's Dynamic Range Optimizer (on by default) brightens the camera JPEG's darker regions,
    // not the raw: keep the colour fitted to the JPEG, take the tone curve without DRO.
    let dro = lightcraft_raw::embedded_preview_dynamic_range_optimized(bytes) == Some(true);
    let look = match fit_look_with(&sensor, &reference, &clipped, colour.clone(), dro) {
        Some(look) => look,
        // A black-and-white JPEG has no colour to fit: the profile's colour and the JPEG's tone curve
        // (`monochrome_look` does it without a profile).
        None if colour.is_some() && black_and_white => {
            let unclipped = without_clipped(&sensor, &clipped);
            let bright = HighlightCheck { pairs: highlights(&sensor, &reference), dro, grey: true };
            let Some(look) = fit_tone_only_with(unclipped.as_ref().unwrap_or(&sensor), &reference, colour, &bright) else { return (None, true) };
            look
        }
        None => return (None, black_and_white),
    };
    let look = if dro { with_dro_off_tone(look) } else { look };
    if lightcraft_pipeline::profiling() {
        eprintln!(
            "[profile] {:?} camera look: {:?}, {:?}, hue/sat table {}, camera profile available {}, DRO tone replaced {dro}",
            raw.format,
            look.matrix.0,
            look.tone,
            look.hue_sat.is_some(),
            profile.is_some()
        );
    }
    (Some(look), black_and_white)
}

/// Sensor level (normalised, white = 1) at or above which a raw channel counts as clipped, as in
/// the render's highlight reconstruction.
const SENSOR_CLIP: f32 = 0.99;

/// [`fit_look_with`] for a photo without Sony's DRO.
#[cfg(test)]
fn fit_look(sensor: &Rgb32f, reference: &Rgb32f, clipped: &[bool], colour: Option<(Mat3, Option<HsvTable>)>) -> Option<CameraLook> {
    fit_look_with(sensor, reference, clipped, colour, false)
}

/// The photo's look, refitted without the proxy pixels whose sensor values are clipped.
///
/// A clipped raw pixel no longer records the scene's colour: with one channel held at the clip
/// level while the others keep rising, a white overcast sky comes out magenta or blue through the
/// camera matrix, while the camera JPEG shows it white. As correspondences such pixels teach the
/// colour matrix to pull colours toward grey, and the chroma curve to remove colour at their
/// display luminance (on a Z 6II forest under a clipped sky the curve cut colourfulness above
/// display luminance 0.4 to 13–19%, and every one of its samples there was clipped). The render
/// rebuilds clipped highlights separately (`lightcraft_raw::highlight`), so the look is fitted on
/// what the sensor measured.
///
/// The search on all pixels decides, as before, whether the photo gets a look and which attempt
/// of [`search_ordered`] gives it; only that attempt is refitted without the clipped pixels, and
/// when the refit misses the acceptance gates the look on all pixels is kept. So no photo loses
/// its look and none switches attempt (a refit that just clears the gate on all pixels must not
/// replace a look fitted away from edges). A photo that had no look gets one when the search
/// without the clipped pixels finds it, and otherwise the [`fit_partial`] look when that is clearly
/// closer to the camera JPEG than the neutral fallback.
fn fit_look_with(sensor: &Rgb32f, reference: &Rgb32f, clipped: &[bool], colour: Option<(Mat3, Option<HsvTable>)>, dro: bool) -> Option<CameraLook> {
    let unclipped = without_clipped(sensor, clipped);
    // from the proxies as they are: the retries' filters (clipped pixels, edges) don't remove them
    let bright = HighlightCheck { pairs: highlights(sensor, reference), dro, grey: false };
    let Some((look, attempt)) = search_ordered(sensor, reference, colour.clone(), TONE_FITS, &bright) else {
        return unclipped
            .as_ref()
            .and_then(|u| search_ordered(u, reference, colour, TONE_FITS, &bright))
            .map(|(look, _)| look)
            .or_else(|| fit_partial(unclipped.as_ref().unwrap_or(sensor), reference, &bright));
    };
    let Some(unclipped) = unclipped else { return Some(look) };
    let refit = fit_attempt(&unclipped, reference, &colour, attempt, &bright);
    if refit.is_none() && lightcraft_pipeline::profiling() {
        eprintln!("[profile] camera look: no fit without the clipped pixels on the accepted attempt, keeping the fit on all pixels");
    }
    Some(refit.unwrap_or(look))
}

/// `sensor` with its clipped pixels set to NaN (left out of every training pair), or `None` when
/// none is clipped.
fn without_clipped(sensor: &Rgb32f, clipped: &[bool]) -> Option<Rgb32f> {
    if clipped.len() != sensor.data.len() || !clipped.iter().any(|c| *c) {
        return None;
    }
    let mut out = sensor.clone();
    for (p, c) in out.data.iter_mut().zip(clipped) {
        if *c {
            *p = [f32::NAN; 3];
        }
    }
    if lightcraft_pipeline::profiling() {
        eprintln!(
            "[profile] camera look: {} of {} proxy pixels have a clipped sensor channel",
            clipped.iter().filter(|c| **c).count(),
            clipped.len()
        );
    }
    Some(out)
}

/// 1 where any channel of a camera-RGB pixel is at or above [`SENSOR_CLIP`], else 0.
fn clip_mask(camera: &Rgb32f) -> Rgb32f {
    let mut mask = camera.clone();
    mask.map_in_place(|p| if p.iter().any(|v| *v >= SENSOR_CLIP) { [1.0; 3] } else { [0.0; 3] });
    mask
}

/// Sony's global tone curve with the Dynamic Range Optimizer off (Standard creative style), pooled
/// from 12 CC0 raw.pixls.us ARWs of 10 interchangeable-lens bodies and their camera JPEGs; the file
/// lists them and how the curve was derived (`assets/ATTRIBUTION.md`). Pooled the same way from 55
/// private ILCE-7CR photos with DRO off, the curve differs by at most 1.2 L* between scene luminance
/// 0.01 and 0.5. Issue #244, `docs/camera-preview-colour.md`.
const SONY_DRO_OFF_TONE: &str = include_str!("../../../assets/camera-tone/sony-dro-off.json");

/// [`SONY_DRO_OFF_TONE`]'s curve (`None` only if the built-in file were invalid; a test checks it).
fn sony_dro_off_tone() -> Option<CameraTone> {
    #[derive(serde::Deserialize)]
    struct File {
        knots: [[f32; 2]; 32],
    }
    static TONE: std::sync::OnceLock<Option<CameraTone>> = std::sync::OnceLock::new();
    *TONE.get_or_init(|| serde_json::from_str::<File>(SONY_DRO_OFF_TONE).ok().and_then(|f| CameraTone::new(f.knots)))
}

/// `look`, fitted to a camera JPEG brightened by Sony's Dynamic Range Optimizer, with its tone
/// curve lowered to Sony's without DRO ([`SONY_DRO_OFF_TONE`]) wherever that is darker. DRO only
/// brightens, so the fitted curve bounds the curve without it from above, everywhere: where Sony's
/// curve is brighter than the photo's own (the 1″ compacts map raw values darker), the photo keeps
/// its own. Its colour (matrix, hue/saturation table and chroma curve) stays as fitted.
fn with_dro_off_tone(look: CameraLook) -> CameraLook {
    CameraLook { tone: dro_off(&look.tone), ..look }
}

/// The curve of [`with_dro_off_tone`]: `own` lowered to Sony's without DRO wherever that is darker,
/// or `own` unchanged when it can't be.
fn dro_off(own_tone: &CameraTone) -> CameraTone {
    let Some(dro_off) = sony_dro_off_tone() else { return *own_tone };
    let own = own_tone.knots();
    let mut knots = own.map(|[x, y]| [x, y.min(dro_off.apply(x))]);
    // Past its last knot a curve goes on as a shoulder whose rate comes from its last two knots
    // (`CameraTone::apply`): lowering the second-last knot more than the last steepens it, and the
    // extended highlights would end up above the fitted curve. Raise the second-last knot (never
    // above the fitted curve) until the rate is at most the fitted curve's.
    let ([a, b], last) = ([own[30], own[31]], knots[31]);
    let rate = ((b[1] - a[1]) / (b[0] - a[0])).clamp(0.1, 16.0) / (1.0 - b[1]).max(0.01);
    let floor = last[1] - rate * (1.0 - last[1]).max(0.01) * (b[0] - a[0]);
    knots[30][1] = knots[30][1].max(floor).min(last[1]);
    let Some(tone) = CameraTone::new(knots).and_then(|tone| tone.with_chroma(*own_tone.chroma())) else { return *own_tone };
    if !at_most(&tone, own_tone) {
        return *own_tone;
    }
    tone
}

/// Whether curve `a` is nowhere brighter than `b` over the scene luminances the finish stage's tone
/// table covers ([`ToneMap::camera`]).
fn at_most(a: &CameraTone, b: &CameraTone) -> bool {
    use lightcraft_pipeline::tone::{GREY, LUT_MAX_EV, LUT_MIN_EV, LUT_N};
    (0..LUT_N).all(|i| {
        let x = GREY * 2f32.powf(LUT_MIN_EV + (LUT_MAX_EV - LUT_MIN_EV) * i as f32 / (LUT_N - 1) as f32);
        a.apply(x) <= b.apply(x) + 1e-5
    })
}

/// Pixel cap for the embedded JPEG decoded for the fit. The codec checks it before decoding and
/// decodes at full size, so a 66.5 MP full-frame ILCE-7RM6 JPEG transiently needs ~200 MB (3 B/px);
/// 128 MP also admits 100 MP medium-format previews.
const FIT_PREVIEW_MAX_PIXELS: u64 = 128_000_000;

/// A DNG's tone and chroma curves fitted to its own camera JPEG ([`dng_tone_eligible`]); the file's colour model
/// (matrix, profile tables, white balance) stays as it is: the proxy goes through the file's tables and the given
/// colour of the search is the identity. Same held-out acceptance gates as [`fit_look`]; the preview must not be a
/// thumbnail ([`MIN_PREVIEW_FRACTION`]), must show the same picture ([`structure_agrees`]), and the curve must not
/// clip more highlights than the plain render ([`clips_more`]).
fn fit_dng_tone(raw: &RawImage, bytes: &[u8], transform: &CameraTransform, lens: Option<&lightcraft_develop::EmbeddedLens>) -> Option<CameraLook> {
    // Logged under LIGHTCRAFT_PROFILE so a corpus run can count the outcome per gate.
    let refuse = |why: &str| {
        if std::env::var_os("LIGHTCRAFT_PROFILE").is_some() {
            eprintln!("DNGTONE refused {why}");
        }
        None
    };
    let Some((mut sensor, reference, clipped)) = proxies(raw, bytes, transform, PROXY, MIN_PREVIEW_FRACTION, None) else { return refuse("preview") };
    // As the loader renders it: the profile's tables around the baseline exposure.
    let xy = lightcraft_raw::color::as_shot_white_xy(raw);
    if let Some(tables) = lightcraft_raw::profile::ProfileTables::new(&raw.color.profile, lightcraft_raw::color::illuminant_weight(&raw.color, xy)) {
        let gain = 2f32.powf(transform.baseline_exposure as f32);
        sensor.map_in_place(|p| tables.apply(p.map(|v| v / gain), gain));
    }
    correct_vignetting(&mut sensor, lens, raw.orientation);
    if (sensor.width, sensor.height) != (reference.width, reference.height) {
        return refuse("preview");
    }
    if !structure_agrees(&sensor, &reference) {
        return refuse("structure");
    }
    let unclipped = without_clipped(&sensor, &clipped);
    let fit_on = unclipped.as_ref().unwrap_or(&sensor);
    let colour = Some((Mat3::IDENTITY, None));
    let bright = HighlightCheck { pairs: highlights(&sensor, &reference), dro: false, grey: false };
    let look = TONE_FITS.iter().find_map(|&tone| {
        [false, true].into_iter().find_map(|away_from_edges| {
            fit_attempt(fit_on, &reference, &colour, Attempt { tone, profile: true, away_from_edges, dark: true }, &bright)
        })
    });
    let Some(look) = look else { return refuse("fit") };
    let plain = match raw.color.profile.tone_curve.as_ref().and_then(crate::files::dng_tone_curve) {
        Some(curve) => ToneMap::camera(&curve, 0.0, 0.0, 0.0),
        None => ToneMap::new(0.0, 0.0, 0.0),
    };
    let fitted = ToneMap::camera(&look.tone, 0.0, 0.0, 0.0);
    if clips_more(&sensor, &reference, &fitted, &plain) {
        return refuse("clip");
    }
    if std::env::var_os("LIGHTCRAFT_PROFILE").is_some() {
        eprintln!("DNGTONE accepted");
    }
    Some(look)
}

/// Whether the fitted tone map puts clearly more of the photo's proxy pixels at or near white than both the plain
/// render and the camera's own preview do ([`CLIP_TOLERANCE`]): a fit that clips what the camera clips too is the
/// camera's look, one that clips what neither does loses highlight detail.
fn clips_more(sensor: &Rgb32f, reference: &Rgb32f, fitted: &ToneMap, plain: &ToneMap) -> bool {
    let near_white = |tone: &ToneMap| {
        let n = sensor.data.iter().filter(|p| p.iter().all(|v| v.is_finite()) && tone.apply(luminance_2020(p.map(|v| v.max(0.0)))) >= 0.98).count();
        n as f64 / sensor.data.len().max(1) as f64
    };
    let preview = reference.data.iter().filter(|p| luminance_2020(**p) >= 0.98).count() as f64 / reference.data.len().max(1) as f64;
    near_white(fitted) > near_white(plain).max(preview) + CLIP_TOLERANCE
}

/// Multiply the sensor proxy (sensor orientation, default crop) by the embedded vignetting correction, the gain the
/// pipeline applies to the render (`optics::embedded_vignette_gain`; `lens` is in the oriented, cropped frame): the
/// camera JPEG has it applied, so a tone fitted without it brightens the render's corners twice.
fn correct_vignetting(sensor: &mut Rgb32f, lens: Option<&lightcraft_develop::EmbeddedLens>, orientation: lightcraft_geom::Orientation) {
    let Some(vignette) = lens.and_then(|l| l.vignette) else { return };
    let (w, h) = (sensor.width as f64, sensor.height as f64);
    let (ow, oh) = if orientation.swaps_axes() { (h, w) } else { (w, h) };
    let width = sensor.width.max(1);
    for (i, p) in sensor.data.iter_mut().enumerate() {
        let (x, y) = orientation.map((i % width) as f64 + 0.5, (i / width) as f64 + 0.5, w, h);
        let gain = lightcraft_pipeline::optics::embedded_vignette_gain(&vignette, lightcraft_geom::Point::new(x, y), ow, oh) as f32;
        *p = p.map(|v| v * gain);
    }
}

/// The embedded preview's long edge must be at least this share of the raw's (and 384 px): a DNG whose preview is a
/// small thumbnail is not fitted. The fit itself runs on a proxy of at most 384 px, so the 1024 px preview
/// Lightroom writes into its DNGs (15% of a 33 MP raw's long edge) is plenty.
const MIN_PREVIEW_FRACTION: f64 = 0.1;

/// Lowest preview luminance and channel value a tone-only fit trains on (the other fits use 0.015 and 0.004): a dark
/// night scene has most of its pixels below 0.015, and a curve fitted without them overshoots its shadows.
const DARK_FLOOR: (f32, f32) = (0.004, 0.001);

/// Share of the proxy by which the fitted curve may clip more highlights than the plain render and the camera's
/// preview (chosen on the CC0 and user DNGs from the clipped share of their 1440 px renders: it refuses the fits
/// that clip about 1% more than the camera does and passes the ones that clip like the camera).
const CLIP_TOLERANCE: f64 = 0.001;

/// Log-luminance correlation (5 × 5 box blurred) the unshifted proxies must reach.
const MIN_STRUCTURE: f64 = 0.85;

/// Whether the sensor proxy and the camera JPEG proxy show the same picture in the same place: their log luminance
/// correlates by at least [`MIN_STRUCTURE`] unshifted, and no shift of more than one proxy pixel (within ±3)
/// correlates better.
fn structure_agrees(sensor: &Rgb32f, reference: &Rgb32f) -> bool {
    let (w, h) = (sensor.width, sensor.height);
    if (w, h) != (reference.width, reference.height) || w < 16 || h < 16 || sensor.data.len() != w * h || reference.data.len() != w * h {
        return false;
    }
    let log = |img: &Rgb32f| -> Vec<f64> {
        let l: Vec<f64> =
            img.data.iter().map(|p| f64::from(luminance_2020(p.map(|v| if v.is_finite() { v.max(0.0) } else { 0.0 }))).max(1e-4).ln()).collect();
        (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                let (mut sum, mut n) = (0.0, 0.0);
                for yy in y.saturating_sub(2)..(y + 3).min(h) {
                    for xx in x.saturating_sub(2)..(x + 3).min(w) {
                        if let Some(v) = l.get(yy * w + xx) {
                            sum += v;
                            n += 1.0;
                        }
                    }
                }
                sum / f64::max(n, 1.0)
            })
            .collect()
    };
    let (a, b) = (log(sensor), log(reference));
    const M: usize = 3;
    let corr = |dx: isize, dy: isize| -> f64 {
        let (mut n, mut sa, mut sb, mut saa, mut sbb, mut sab) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        for y in M..h - M {
            for x in M..w - M {
                let (xx, yy) = (x.saturating_add_signed(dx), y.saturating_add_signed(dy));
                let (Some(&u), Some(&v)) = (a.get(y * w + x), b.get(yy * w + xx)) else { continue };
                n += 1.0;
                sa += u;
                sb += v;
                saa += u * u;
                sbb += v * v;
                sab += u * v;
            }
        }
        let var = (saa - sa * sa / n) * (sbb - sb * sb / n);
        if n < 2.0 || var <= 1e-12 { 0.0 } else { (sab - sa * sb / n) / var.sqrt() }
    };
    let at_zero = corr(0, 0);
    let (mut best, mut shift) = (at_zero, (0isize, 0isize));
    for dy in -(M as isize)..=M as isize {
        for dx in -(M as isize)..=M as isize {
            let c = corr(dx, dy);
            if c > best + 1e-9 {
                (best, shift) = (c, (dx, dy));
            }
        }
    }
    at_zero >= MIN_STRUCTURE && shift.0.abs() <= 1 && shift.1.abs() <= 1
}

/// Same-size proxies of the sensor (white-balanced, baseline exposure, through `transform`'s
/// matrix: the generic camera ≈ sRGB model) and of the file's embedded camera JPEG (linear Rec.2020),
/// and per proxy pixel whether any sensor sample it covers is clipped.
fn proxies(
    raw: &RawImage,
    bytes: &[u8],
    transform: &CameraTransform,
    size: usize,
    min_fraction: f64,
    lens: Option<&LensGeometry>,
) -> Option<(Rgb32f, Rgb32f, Vec<bool>)> {
    let edge = (2 * size).max(384) as u32;
    let decoded = crate::files::decode_raw_preview(
        bytes,
        lightcraft_codecs::DecodeOptions { max_size: Some((edge, edge)), max_pixels: FIT_PREVIEW_MAX_PIXELS },
        None,
    )?;
    let (a, crop) = (raw.active_area, raw.crop.clipped(raw.active_area.width, raw.active_area.height));
    // a preview below `min_fraction` of the crop's long edge (or under 384 px) is refused; 0 = any
    if min_fraction > 0.0 && (decoded.source_width.max(decoded.source_height) as f64) < (min_fraction * crop.width.max(crop.height) as f64).max(384.0)
    {
        return None;
    }
    let mut reference = decoded.to_working();
    if crop.width == 0 || crop.height == 0 || reference.width == 0 || reference.height == 0 {
        return None;
    }
    let matches = |w: usize, h: usize, rw: usize, rh: usize| (rw as f64 / rh as f64 / (w as f64 / h as f64) - 1.0).abs() <= 0.02;
    if !matches(crop.width, crop.height, reference.width, reference.height) {
        // Panasonic previews show the whole active area while the default crop is the in-camera aspect ratio
        if !matches(a.width, a.height, reference.width, reference.height) {
            return None;
        }
        let (sx, sy) = (reference.width as f64 / a.width as f64, reference.height as f64 / a.height as f64);
        let (x, y) = ((crop.x as f64 * sx).round() as usize, (crop.y as f64 * sy).round() as usize);
        let (w, h) = ((crop.width as f64 * sx).round() as usize, (crop.height as f64 * sy).round() as usize);
        if w < 16 || h < 16 || x + w > reference.width || y + h > reference.height {
            return None;
        }
        reference = reference.into_crop(x, y, w, h);
    }
    // Fixed, bounded proxy: the selected look cannot depend on thumbnail/export resolution.
    let k = (crop.width.max(crop.height).div_ceil(edge as usize).max(2)).div_ceil(2) * 2;
    let mut sensor = sensor_proxy(raw, k, edge as usize)?;
    let mut clipped = clip_mask(&sensor);
    // The camera may have corrected its JPEG with the lens warp the file carries (Nikon Z lenses always, at up to 9 %
    // zoom; other cameras when their correction setting is on): the sensor moved into that geometry is a candidate,
    // kept below when it lines up with the JPEG better than the uncorrected one.
    let warped = lens
        .filter(|_| raw.format != RawFormat::Cr3)
        .and_then(|l| Some((into_lens_geometry(&sensor, raw, k, l)?, into_lens_geometry(&clipped, raw, k, l)?)));
    let gain = 2f32.powf(transform.baseline_exposure as f32);
    let to_working = |p: [f32; 3]| transform.matrix.apply_f32(std::array::from_fn(|i| p[i] * transform.wb[i] * gain));
    if raw.format == RawFormat::Cr3 {
        // Orient the camera JPEG and the sensor identically before collecting pixel correspondences.
        sensor = sensor.into_oriented(raw.orientation);
        clipped = clipped.into_oriented(raw.orientation);
        reference = reference.into_oriented(raw.orientation);
        sensor.map_in_place(to_working);
        // Canon's JPEG can be cropped differently from the sensor. Estimate only that common
        // framing from edge directions, independently of the subsequent colour fit.
        (sensor, clipped) = align_cr3_framing(sensor, clipped, &reference);
    }
    let mut sensor = fit(&sensor, size, size, Filter::Box);
    let mut reference_small = reference_at(&reference, &sensor);
    if let Some((warped, warped_clipped)) = warped {
        let candidate = fit(&warped, size, size, Filter::Box);
        let candidate_reference = reference_at(&reference, &candidate);
        if alignment(&candidate, &candidate_reference) > alignment(&sensor, &reference_small) {
            (sensor, reference_small, clipped) = (candidate, candidate_reference, warped_clipped);
        }
    }
    let reference = reference_small;
    // A proxy pixel is clipped when any sample it averages is (outside the CR3 framing the mask is
    // NaN: those pixels are left out of the pairs anyway). Resized to exactly the proxy's size, like
    // the reference, so the mask pairs pixel for pixel.
    let clipped: Vec<bool> = resize(&clipped, sensor.width, sensor.height, Filter::Box).data.iter().map(|p| p[0] > 0.0).collect();
    if raw.format != RawFormat::Cr3 {
        sensor.map_in_place(to_working);
    }
    Some((sensor, reference, clipped))
}

/// The camera JPEG at exactly the sensor proxy's size, so the two pair pixel for pixel. Fitting it
/// into the proxy's box instead keeps its own aspect, which can round a pixel short: an ILCE-7RM4's
/// 1616×1080 preview, decoded at 384×257, fits a 192×128 proxy as 191×128, and `collect_pairs`
/// then rejected every file `lightcraft-cli calibrate` was given. `proxies` has already checked
/// that the aspects agree within 2 %.
fn reference_at(reference: &Rgb32f, sensor: &Rgb32f) -> Rgb32f {
    resize(reference, sensor.width, sensor.height, Filter::Box)
}

#[derive(Clone, Copy, Debug)]
struct Cr3Framing {
    scale: f32,
    /// Translation as a fraction of the oriented frame's width and height.
    offset: [f32; 2],
}

impl Cr3Framing {
    const IDENTITY: Self = Self { scale: 1.0, offset: [0.0; 2] };

    fn source(self, x: f32, y: f32, w: usize, h: usize) -> (f32, f32) {
        let (w, h) = (w as f32, h as f32);
        ((x - w * 0.5) * self.scale + w * (0.5 + self.offset[0]), (y - h * 0.5) * self.scale + h * (0.5 + self.offset[1]))
    }
}

/// Register the JPEG's remaining framing on a fixed proxy. Edge directions survive different
/// white balance and monotone camera tone; no target RGB values train this three-parameter fit.
/// A separate third of the edges must confirm a substantial improvement and a close match.
/// A weak, flat or unrelated preview leaves the sensor framing unchanged.
fn estimate_cr3_framing(sensor: &Rgb32f, reference: &Rgb32f) -> Option<Cr3Framing> {
    if sensor.width.checked_mul(sensor.height)? != sensor.data.len() || reference.width.checked_mul(reference.height)? != reference.data.len() {
        return None;
    }
    let sensor = fit(sensor, PROXY, PROXY, Filter::Box);
    let (w, h) = (sensor.width, sensor.height);
    if w < 16 || h < 16 {
        return None;
    }
    let reference = fit(reference, w, h, Filter::Box);
    if (reference.width, reference.height) != (w, h) {
        return None;
    }
    let mut gradients = Rgb32f::new(w, h);
    let mut edges = Vec::new();
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let derivative = |image: &Rgb32f| {
                [
                    luminance_2020(image.data[y * w + x + 1]) - luminance_2020(image.data[y * w + x - 1]),
                    luminance_2020(image.data[(y + 1) * w + x]) - luminance_2020(image.data[(y - 1) * w + x]),
                ]
            };
            let d = derivative(&sensor);
            gradients.data[y * w + x] = [d[0], d[1], 0.0];
            let d = derivative(&reference);
            let magnitude = d[0].hypot(d[1]);
            let value = luminance_2020(reference.data[y * w + x]);
            if (0.02..0.9).contains(&value) && magnitude.is_finite() && magnitude > 0.03 {
                edges.push((x as f32 + 0.5, y as f32 + 0.5, d, magnitude, (x * 17 + y * 11) % 3 == 0));
            }
        }
    }
    if edges.len() < 192 {
        return None;
    }
    let score = |framing: Cr3Framing, held_out: bool| -> Option<f64> {
        let (mut agreement, mut weight, mut valid, mut total) = (0.0, 0.0, 0usize, 0usize);
        for &(x, y, target, magnitude, holdout) in &edges {
            if holdout != held_out {
                continue;
            }
            total += 1;
            let (sx, sy) = framing.source(x, y, w, h);
            if sx < 1.5 || sy < 1.5 || sx > w as f32 - 1.5 || sy > h as f32 - 1.5 {
                continue;
            }
            let sample = gradients.sample_bilinear(sx, sy);
            let length = sample[0].hypot(sample[1]);
            if !length.is_finite() || length <= 0.01 {
                continue;
            }
            let k = f64::from(magnitude.min(0.25));
            agreement += k * f64::from((sample[0] * target[0] + sample[1] * target[1]) / (length * magnitude));
            weight += k;
            valid += 1;
        }
        (valid >= 64 && valid * 5 >= total * 4 && weight > 0.0).then_some(agreement / weight)
    };
    let before = score(Cr3Framing::IDENTITY, false)?;
    let mut best = (before, Cr3Framing::IDENTITY);
    // At most 10% zoom and two proxy pixels of translation; a coarse search followed by one
    // local refinement prevents unbounded optimisation on the photograph's contents.
    for s in -10..=10 {
        for tx in -4..=4 {
            for ty in -4..=4 {
                let framing = Cr3Framing { scale: 1.0 + s as f32 * 0.01, offset: [tx as f32 * 0.5 / w as f32, ty as f32 * 0.5 / h as f32] };
                if let Some(value) = score(framing, false)
                    && value > best.0
                {
                    best = (value, framing);
                }
            }
        }
    }
    let coarse = best.1;
    for s in -5..=5 {
        for tx in -4..=4 {
            for ty in -4..=4 {
                let framing = Cr3Framing {
                    scale: coarse.scale + s as f32 * 0.002,
                    offset: [coarse.offset[0] + tx as f32 * 0.125 / w as f32, coarse.offset[1] + ty as f32 * 0.125 / h as f32],
                };
                if !(0.9..=1.1).contains(&framing.scale) || framing.offset[0].abs() > 2.0 / w as f32 || framing.offset[1].abs() > 2.0 / h as f32 {
                    continue;
                }
                if let Some(value) = score(framing, false)
                    && value > best.0
                {
                    best = (value, framing);
                }
            }
        }
    }
    let (held_before, held_after) = (score(Cr3Framing::IDENTITY, true)?, score(best.1, true)?);
    if best.0 < 0.9 || best.0 < before + 0.04 || held_after < 0.9 || held_after < held_before + 0.04 {
        return None;
    }
    if lightcraft_pipeline::profiling() {
        eprintln!("[profile] CR3 JPEG framing {:?}, held-out edge agreement {held_before:.4} -> {held_after:.4}", best.1);
    }
    Some(best.1)
}

/// The sensor proxy and its clip mask (same size), both moved to the camera JPEG's framing.
fn align_cr3_framing(sensor: Rgb32f, clipped: Rgb32f, reference: &Rgb32f) -> (Rgb32f, Rgb32f) {
    let Some(framing) = estimate_cr3_framing(&sensor, reference) else { return (sensor, clipped) };
    let apply = |image: &Rgb32f| {
        let (w, h) = (image.width, image.height);
        let mut aligned = Rgb32f::new(w, h);
        for (i, pixel) in aligned.data.iter_mut().enumerate() {
            let (x, y) = framing.source((i % w) as f32 + 0.5, (i / w) as f32 + 0.5, w, h);
            *pixel = if x >= 0.5 && y >= 0.5 && x <= w as f32 - 0.5 && y <= h as f32 - 0.5 { image.sample_bilinear(x, y) } else { [f32::NAN; 3] };
        }
        aligned
    };
    (apply(&sensor), apply(&clipped))
}

/// Colour training pairs of one raw for a camera profile: white-balanced camera RGB (with the
/// baseline exposure) → its camera JPEG (linear Rec.2020). `None` for formats with their own
/// colour matrices, other files or unusable previews.
pub(crate) fn profile_pairs(raw: &RawImage, bytes: &[u8], lens: Option<&LensGeometry>) -> Option<Vec<([f64; 3], [f64; 3])>> {
    if !file_local_look(raw.format) || lightcraft_raw::color::has_matrix(&raw.color) {
        return None;
    }
    let transform = lightcraft_raw::color::camera_transform(raw, lightcraft_raw::color::as_shot_white_xy(raw));
    let (sensor, reference, clipped) = proxies(raw, bytes, &transform, PROFILE_PROXY, 0.0, lens)?;
    let to_camera = transform.matrix.inverse()?;
    // clipped sensor pixels don't record the scene's colour (see `fit_look`)
    let sensor = without_clipped(&sensor, &clipped).unwrap_or(sensor);
    let (pairs, _) = collect_pairs(&sensor, &reference, 0.05, None, false)?;
    Some(pairs.into_iter().map(|(x, y)| (to_camera.apply(x), y)).collect())
}

/// A camera profile's colour model (matrix from white-balanced camera RGB, hue/saturation table)
/// fitted to pairs pooled from many photos.
pub(crate) fn fit_profile(pairs: &[([f64; 3], [f64; 3])]) -> Option<(Mat3, Option<HsvTable>)> {
    let matrix = fit_matrix(pairs)?;
    Some((matrix, fit_hue_sat(pairs, &matrix)))
}

/// A raw's lens-correction warp, as its camera applied it to the embedded JPEG: the first `WarpRectilinear` of
/// `OpcodeList3` (its green plane; the in-file corrections LightCraft reads have no tangential terms). `None` without
/// one, or for a DNG 1.6 `WarpRectilinear2` chain (phone DNGs, which carry colour matrices and are not fitted).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LensGeometry {
    /// `kr0..kr3`: source radius = r * (kr0 + kr1 r^2 + kr2 r^4 + kr3 r^6), r over the farthest corner distance.
    k: [f64; 4],
    /// The optical centre, relative to the active area (as the opcode has it).
    center: [f64; 2],
}

pub(crate) fn lens_geometry(list3: &[lightcraft_raw::Opcode]) -> Option<LensGeometry> {
    use lightcraft_raw::Opcode;
    let ops: Vec<&Opcode> = lightcraft_raw::opcodes::applied_ops(list3).collect();
    if ops.iter().any(|op| matches!(op, Opcode::WarpRectilinear2 { .. })) {
        return None;
    }
    ops.into_iter().find_map(|op| match op {
        Opcode::WarpRectilinear { planes, center } => {
            let p = planes.get(1).or(planes.first())?;
            let k = [p[0], p[1], p[2], p[3]];
            (k.iter().chain(center).all(|v| v.is_finite()) && (0.0..=1.0).contains(&center[0]) && (0.0..=1.0).contains(&center[1]))
                .then_some(LensGeometry { k, center: *center })
        }
        _ => None,
    })
}

/// How well two same-size proxies line up: the correlation of their high-passed log luminance over the pixels both
/// have (a tone curve or colour change leaves it alone, a geometric offset lowers it). `-1` without enough pixels.
fn alignment(a: &Rgb32f, b: &Rgb32f) -> f64 {
    let (w, h) = (a.width, a.height);
    if (b.width, b.height) != (w, h) || w < 3 || h < 3 {
        return -1.0;
    }
    let lum = |img: &Rgb32f| img.data.iter().map(|p| (luminance_2020(*p).max(1e-4) as f64).ln()).collect::<Vec<f64>>();
    let high = |l: &[f64]| -> Vec<f64> {
        (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                if x == 0 || y == 0 || x + 1 == w || y + 1 == h {
                    return f64::NAN;
                }
                let around = [i - w - 1, i - w, i - w + 1, i - 1, i + 1, i + w - 1, i + w, i + w + 1];
                l[i] - around.iter().map(|&j| l[j]).sum::<f64>() / 8.0
            })
            .collect()
    };
    let (ha, hb) = (high(&lum(a)), high(&lum(b)));
    let pairs: Vec<(f64, f64)> = ha.into_iter().zip(hb).filter(|(x, y)| x.is_finite() && y.is_finite()).collect();
    if pairs.len() < 64 {
        return -1.0;
    }
    let n = pairs.len() as f64;
    let (ma, mb) = (pairs.iter().map(|p| p.0).sum::<f64>() / n, pairs.iter().map(|p| p.1).sum::<f64>() / n);
    let cov: f64 = pairs.iter().map(|p| (p.0 - ma) * (p.1 - mb)).sum();
    let (va, vb): (f64, f64) = (pairs.iter().map(|p| (p.0 - ma).powi(2)).sum(), pairs.iter().map(|p| (p.1 - mb).powi(2)).sum());
    let r = cov / (va * vb).sqrt().max(1e-12);
    if r.is_finite() { r } else { -1.0 }
}

/// A `k`-binned sensor proxy of `raw` ([`sensor_proxy`]: the default crop, or the active area when the crop is
/// smaller than a block, at `1/k`) moved into the camera JPEG's geometry: each pixel samples the proxy where `lens`
/// maps it, and is NaN where that falls outside the proxy (the pairs leave it out). `None` for a proxy of another
/// size (the demosaiced fallback of linear raws).
fn into_lens_geometry(img: &Rgb32f, raw: &RawImage, k: usize, lens: &LensGeometry) -> Option<Rgb32f> {
    let a = raw.active_area;
    let c = raw.crop.clipped(a.width, a.height);
    let c = if c.width < k || c.height < k { lightcraft_raw::Rect::new(0, 0, a.width, a.height) } else { c };
    let (w, h) = (img.width, img.height);
    if k == 0 || w != c.width / k || h != c.height / k || w == 0 || h == 0 || a.width < 2 || a.height < 2 {
        return None;
    }
    let (aw, ah) = (a.width as f64 - 1.0, a.height as f64 - 1.0);
    let (cx, cy) = (lens.center[0] * aw, lens.center[1] * ah);
    let m = [(0.0, 0.0), (aw, 0.0), (0.0, ah), (aw, ah)].iter().map(|&(x, y)| (x - cx).hypot(y - cy)).fold(0.0, f64::max).max(1e-9);
    let [k0, k1, k2, k3] = lens.k;
    let kf = k as f64;
    let (x0, y0) = (c.x as f64, c.y as f64);
    let mut out = Rgb32f::new(w, h);
    for (i, px) in out.data.iter_mut().enumerate() {
        // the proxy pixel's centre in active-area pixel-index coordinates, mapped to the sensor
        let (ox, oy) = (x0 + ((i % w) as f64 + 0.5) * kf - 0.5, y0 + ((i / w) as f64 + 0.5) * kf - 0.5);
        let (dx, dy) = ((ox - cx) / m, (oy - cy) / m);
        let r2 = dx * dx + dy * dy;
        let f = k0 + r2 * (k1 + r2 * (k2 + r2 * k3));
        let (sx, sy) = ((cx + m * dx * f + 0.5 - x0) / kf, (cy + m * dy * f + 0.5 - y0) / kf);
        *px = if sx >= 0.5 && sy >= 0.5 && sx <= w as f64 - 0.5 && sy <= h as f64 - 0.5 {
            img.sample_bilinear(sx as f32, sy as f32)
        } else {
            [f32::NAN; 3]
        };
    }
    Some(out)
}

fn sensor_proxy(raw: &RawImage, k: usize, edge: usize) -> Option<Rgb32f> {
    Some(match raw.develop_binned(k, SENSOR_CLIP).ok()? {
        Some(sensor) => sensor,
        // Lens opcodes cannot be applied to a binned mosaic: they are defined at full resolution.
        None if (raw.cpp == 3 && raw.cfa.is_none()) || (raw.format == RawFormat::Dng && !raw.opcodes.list3.is_empty()) => {
            fit(&raw.develop(lightcraft_raw::Method::Bilinear).ok()?, edge, edge, Filter::Box)
        }
        None => return None,
    })
}

/// A fit must cut the held-out squared error to below this share of the fallback's.
const MIN_IMPROVEMENT: f64 = 0.7;
/// ...and stay within this per-channel RMS of the camera JPEG (linear display values). On public
/// raw.pixls.us samples (eight Sony bodies) good fits that still beat the fallback 1.5–3× landed
/// at 0.065–0.093 (camera local tone, vignetting and lens processing that a global matrix + curve
/// cannot follow): visibly better renders that 0.055 rejected.
const MAX_HOLDOUT_RMS: f64 = 0.10;

fn luma(p: [f64; 3]) -> f64 {
    p[0] * 0.2627 + p[1] * 0.6780 + p[2] * 0.0593
}

/// The finish stage's tone map and chroma curve (`lightcraft_pipeline::finish`).
fn displayed(scene: [f64; 3], tone: &ToneMap) -> [f64; 3] {
    let scene = scene.map(|v| v.max(0.0));
    let y = luma(scene);
    if y <= 0.0 {
        return [0.0; 3];
    }
    let o = f64::from(tone.apply(y as f32));
    let k = f64::from(tone.chroma_scale(o as f32));
    scene.map(|v| o + (v * o / y - o) * k)
}

#[cfg(test)]
fn fit_pairs(sensor: &Rgb32f, reference: &Rgb32f) -> Option<CameraLook> {
    fit_pairs_with(sensor, reference, None)
}

/// Largest luminance ratio within the 3×3 neighbourhood of pixel `i` (1 on flat areas, large on edges).
fn local_contrast(image: &Rgb32f, i: usize) -> f32 {
    let (w, h) = (image.width, image.height);
    if w == 0 {
        return f32::INFINITY;
    }
    let (x, y) = (i % w, i / w);
    let (mut lo, mut hi) = (f32::INFINITY, 0f32);
    for row in y.saturating_sub(1)..(y + 2).min(h) {
        for col in x.saturating_sub(1)..(x + 2).min(w) {
            let l = image.data.get(row * w + col).map_or(0.0, |p| luminance_2020(*p));
            lo = lo.min(l);
            hi = hi.max(l);
        }
    }
    if hi.is_finite() { hi / lo.max(1e-4) } else { f32::INFINITY }
}

/// Training pairs (sensor → JPEG, unclipped midtones) and the wider set including highlights
/// (for the chroma curve); `None` when too few, or the photo has too little colour. With
/// `edge_limit`, pixels whose 3×3 neighbourhood in either proxy spans a larger luminance ratio
/// are left out: there a small geometric mismatch between the raw and its JPEG pairs unrelated
/// colours (see [`EDGE_CONTRAST`]).
type Pair = ([f64; 3], [f64; 3]);
type Pairs = Vec<Pair>;
fn collect_pairs(sensor: &Rgb32f, reference: &Rgb32f, min_chroma: f32, edge_limit: Option<f32>, dark: bool) -> Option<(Pairs, Pairs)> {
    if (sensor.width, sensor.height) != (reference.width, reference.height) || sensor.data.len() != reference.data.len() {
        return None;
    }
    let mut pairs = Vec::new();
    // Highlights too (camera JPEGs bleach colours toward white there), for the chroma curve only.
    let mut bright = Vec::new();
    let mut colour = 0;
    for (i, (input, output)) in sensor.data.iter().zip(&reference.data).enumerate() {
        if edge_limit.is_some_and(|limit| local_contrast(sensor, i) > limit || local_contrast(reference, i) > limit) {
            continue;
        }
        let y = luminance_2020(*output);
        if !input.iter().all(|v| v.is_finite() && *v > 0.001 && *v < 1.5) || !output.iter().all(|v| v.is_finite() && *v >= 0.0) {
            continue;
        }
        if (0.015..=1.0).contains(&y) {
            bright.push((input.map(f64::from), output.map(f64::from)));
        }
        let floors = if dark { DARK_FLOOR } else { (0.015, 0.004) };
        if !colour_pair_above(*input, *output, floors) {
            continue;
        }
        let min = output.iter().copied().fold(f32::INFINITY, f32::min);
        let max = output.iter().copied().fold(0.0, f32::max);
        colour += usize::from(max - min > min_chroma);
        pairs.push((input.map(f64::from), output.map(f64::from)));
    }
    (pairs.len() >= 256 && colour >= pairs.len() / 20).then_some((pairs, bright))
}

/// The original colour-pair eligibility, also used to preserve its held-out split in tone fitting.
fn colour_pair(input: [f32; 3], output: [f32; 3]) -> bool {
    colour_pair_above(input, output, (0.015, 0.004))
}

/// [`colour_pair`] with its lowest preview luminance and channel value given (`floors`: the usual 0.015 and 0.004, or
/// [`DARK_FLOOR`] for the tone-only fit of a DNG).
fn colour_pair_above(input: [f32; 3], output: [f32; 3], (ylo, clo): (f32, f32)) -> bool {
    input.iter().all(|v| v.is_finite() && *v > 0.001 && *v < 1.5)
        && output.iter().all(|v| *v > clo && *v < 0.98)
        && (ylo..0.85).contains(&luminance_2020(output))
}

/// Fit tone independently of colour eligibility when enough dark JPEG signal is available.
/// The original pairs and their split still own the colour fit and its acceptance gates.
/// Luminance needs no channel ratios, so the binned proxy can use a lower sensor floor and
/// keep JPEG pixels with a channel near black. Quantile medians limit the effect of noise.
fn fit_tone_with_shadows(
    fit: ToneFit,
    pairs: &[([f64; 3], [f64; 3])],
    sensor: &Rgb32f,
    reference: &Rgb32f,
    edge_limit: Option<f32>,
    colour: impl Fn([f64; 3]) -> [f64; 3],
) -> Option<CameraTone> {
    let luminances = |x: [f64; 3], y: [f64; 3]| (luma(colour(x).map(|v| v.max(0.0))), luma(y));
    let original = fit.fit(pairs.iter().enumerate().filter(|(i, _)| i % 3 != 0).map(|(_, (x, y))| luminances(*x, *y)).collect());
    let mut eligible = Vec::new();
    let (mut colour_index, mut extra_index) = (0, 0);
    for (i, (input, output)) in sensor.data.iter().zip(&reference.data).enumerate() {
        if edge_limit.is_some_and(|limit| local_contrast(sensor, i) > limit || local_contrast(reference, i) > limit) {
            continue;
        }
        // About one 8-bit sRGB code above black: don't learn a flat black segment from
        // JPEG compression. The sensor proxy averages many samples before this test.
        if !input.iter().all(|v| v.is_finite() && *v > 0.0001 && *v < 1.5)
            || !output.iter().all(|v| v.is_finite() && *v >= 0.0)
            || !(0.0003..0.85).contains(&luminance_2020(*output))
        {
            continue;
        }
        // A colour holdout must never enter tone training just because the wider sample set
        // changes its index. Split the extra samples separately, keeping every colour index.
        let index = if colour_pair(*input, *output) { &mut colour_index } else { &mut extra_index };
        let held = *index % 3 == 0;
        *index += 1;
        let (x, y) = luminances(input.map(f64::from), output.map(f64::from));
        if x.is_finite() && x > 0.0 {
            eligible.push((x, y, held));
        }
    }
    let dark = eligible.iter().filter(|(_, y, _)| *y < 0.015).count();
    let held: Pairs = eligible.iter().filter(|(_, y, held)| *held && *y < 0.015).map(|(x, y, _)| ([*x; 3], [*y; 3])).collect();
    // Sparse or uncorrelated shadow noise cannot teach a toe. Tied JPEG levels share a rank,
    // so a single quantized grey also keeps the original curve.
    if dark < 96 || held.len() < 32 || rank_correlation_ties(&held) < PARTIAL_MIN_RANK_CORRELATION {
        return original;
    }
    let training = eligible.iter().filter(|(_, _, held)| !held).map(|(x, y, _)| (*x, *y)).collect();
    let Some(candidate) = fit.fit(training) else { return original };
    let error = |tone: &CameraTone| -> f64 {
        held.iter().map(|(x, y)| (f64::from(tone.apply(x[0] as f32)).powf(1.0 / 2.2) - y[0].powf(1.0 / 2.2)).powi(2)).sum()
    };
    // A wider histogram must also help independent dark pixels, not just redistribute the
    // knots. The usual held-out gates still check the completed look on the colour pairs.
    if let Some(old) = original {
        let (before, after) = (error(&old), error(&candidate));
        if lightcraft_pipeline::profiling() {
            eprintln!("[profile] camera toe: {dark} dark pairs, held-out error {before:.5} -> {after:.5}");
        }
        if !after.is_finite() || after >= before * 0.99 {
            return original;
        }
    }
    Some(candidate)
}

/// Ridge-regularised 3×3 chromaticity matrix (luminance-normalised RGB) on the training pairs.
fn fit_matrix(pairs: &[([f64; 3], [f64; 3])]) -> Option<Mat3> {
    let mut gram = [[0.0; 3]; 3];
    let mut cross = [[0.0; 3]; 3];
    for (i, (x, y)) in pairs.iter().enumerate() {
        if i % 3 == 0 {
            continue;
        }
        let (lx, ly) = (luma(*x), luma(*y));
        if lx <= 0.0 || ly <= 0.0 {
            continue;
        }
        for row in 0..3 {
            for col in 0..3 {
                // Normalising by luminance prevents a camera S-curve from corrupting colour.
                gram[row][col] += x[row] * x[col] / (lx * lx);
                cross[row][col] += y[row] * x[col] / (ly * lx);
            }
        }
    }
    let trace: f64 = (0..3).map(|i| gram[i][i]).sum();
    let inverse = Mat3(gram).inverse()?;
    let condition = trace * (0..3).map(|i| inverse.0[i][i].abs()).sum::<f64>();
    if trace <= 0.0 || !condition.is_finite() || condition > 1e6 {
        return None;
    }
    let regularization = trace * 1e-4;
    for i in 0..3 {
        gram[i][i] += regularization;
        cross[i][i] += regularization;
    }
    let matrix = Mat3(cross).mul(&Mat3(gram).inverse()?);
    matrix.0.iter().flatten().all(|v| v.is_finite() && v.abs() < 8.0).then_some(matrix)
}

/// Neighbourhood luminance ratio above which a pixel counts as an edge for the second attempt
/// of [`search_ordered`]. Camera JPEGs are often lens-corrected (Sony "Distortion Comp.: Auto",
/// compacts and kit zooms): on a 51 mm ILCE-7RM2 shot of a glass façade the JPEG is up to 15 px
/// of 1440 (about one proxy pixel) off the raw, and the fit failed the gate on the mismatched
/// window frames alone (held-out RMS 0.111; 0.059 for the same shot with distortion correction
/// off). Away from edges: 0.049. Issue #232.
const EDGE_CONTRAST: f32 = 3.0;

/// The look on all pixels (the search of [`search_ordered`] with [`TONE_FITS`]), as before
/// clipped pixels were left out.
#[cfg(test)]
fn fit_pairs_with(sensor: &Rgb32f, reference: &Rgb32f, colour: Option<(Mat3, Option<HsvTable>)>) -> Option<CameraLook> {
    fit_pairs_ordered(sensor, reference, colour, TONE_FITS)
}

/// The look from [`search_ordered`] without its attempt.
#[cfg(test)]
fn fit_pairs_ordered(sensor: &Rgb32f, reference: &Rgb32f, colour: Option<(Mat3, Option<HsvTable>)>, order: &[ToneFit]) -> Option<CameraLook> {
    search_ordered(sensor, reference, colour, order, &HighlightCheck { pairs: highlights(sensor, reference), dro: false, grey: false })
        .map(|(look, _)| look)
}

/// The tone fits the search tries, in order: the first whose look passes the acceptance gates is
/// used. Quantile matching comes first because it cannot produce flat steps; the
/// conditional-median fit follows because on a few photos (compacts, early Micro Four Thirds) its
/// slightly lower held-out error is what clears the gates, and a photo must not lose its look to
/// the change of fit.
const TONE_FITS: &[ToneFit] = &[ToneFit::Quantile, ToneFit::ConditionalMedian];

/// One attempt of the search: tone fit, the camera profile's colour or the photo's own, all pixels
/// or only those away from edges.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Attempt {
    tone: ToneFit,
    profile: bool,
    away_from_edges: bool,
    /// Train on the dark end of the preview too ([`DARK_FLOOR`]): the tone-only fit of a DNG.
    dark: bool,
}

/// The photo's look and the attempt that produced it: its own matrix (with and without a
/// hue/saturation table), or the given colour model (a camera profile's, in the sensor proxy's
/// space), each completed with a tone and chroma curve fitted to this photo. Per tone fit in
/// `order`: the camera profile's colour (when there is one), then the photo's own, as a different
/// picture style can make a camera profile fail the gates; each fitted on all pixels and, when that
/// fails the gates, once more on the pixels away from edges (same gates): there colour pairs stay
/// valid when the camera JPEG's geometry differs slightly from the raw's (issue #232). The whole
/// search runs with one tone fit before the next is tried, so the conditional-median fallback
/// accepts exactly what the previous releases accepted.
fn search_ordered(
    sensor: &Rgb32f,
    reference: &Rgb32f,
    colour: Option<(Mat3, Option<HsvTable>)>,
    order: &[ToneFit],
    bright: &HighlightCheck,
) -> Option<(CameraLook, Attempt)> {
    let profiles: &[bool] = if colour.is_some() { &[true, false] } else { &[false] };
    order.iter().find_map(|&tone| {
        profiles.iter().find_map(|&profile| {
            [false, true].into_iter().find_map(|away_from_edges| {
                let attempt = Attempt { tone, profile, away_from_edges, dark: false };
                fit_attempt(sensor, reference, &colour, attempt, bright).map(|look| (look, attempt))
            })
        })
    })
}

fn fit_attempt(
    sensor: &Rgb32f,
    reference: &Rgb32f,
    colour: &Option<(Mat3, Option<HsvTable>)>,
    attempt: Attempt,
    bright: &HighlightCheck,
) -> Option<CameraLook> {
    let colour = if attempt.profile { colour.clone() } else { None };
    fit_pairs_on(sensor, reference, colour, attempt.away_from_edges.then_some(EDGE_CONTRAST), attempt.tone, attempt.dark, bright)
}

fn fit_pairs_on(
    sensor: &Rgb32f,
    reference: &Rgb32f,
    colour: Option<(Mat3, Option<HsvTable>)>,
    edge_limit: Option<f32>,
    tone_fit: ToneFit,
    dark: bool,
    highlights: &HighlightCheck,
) -> Option<CameraLook> {
    // A known colour model needs enough signal for tone fitting, not a scene rich enough to
    // learn a new colour matrix. Still reject monochrome references.
    let (pairs, bright) = collect_pairs(sensor, reference, if colour.is_some() { 0.005 } else { 0.05 }, edge_limit, dark)?;
    let candidates = match colour {
        Some(given) => vec![given],
        None => {
            let matrix = fit_matrix(&pairs)?;
            vec![(matrix, fit_hue_sat(&pairs, &matrix)), (matrix, None)]
        }
    };
    // Matrix + table when the table helps the held-out pixels, else the matrix alone. Chosen by
    // error in gamma-encoded display values (closer to what is seen: in linear values a slightly
    // missed bright rock outweighs a clearly wrong dark shirt); the acceptance gates below stay linear.
    let mut best: Option<(f64, f64, usize, CameraLook)> = None;
    for (matrix, hue_sat) in candidates {
        let correction = hue_sat.as_ref().and_then(HueSat::new);
        let colour = |x: [f64; 3]| {
            let p = matrix.apply(x);
            correction.as_ref().map_or(p, |c| c.apply(p.map(|v| v as f32)).map(f64::from))
        };
        let Some(curve) = fit_tone_with_shadows(tone_fit, &pairs, sensor, reference, edge_limit, colour) else { continue };
        let mut look = CameraLook { matrix, tone: curve, hue_sat: hue_sat.clone() };
        if let Some(tone) = fit_chroma(&bright, &look) {
            look.tone = tone;
        }
        let tone = ToneMap::camera(&look.tone, 0.0, 0.0, 0.0);
        if !highlights_hold(&look.tone, highlights, colour) {
            continue;
        }
        let (mut linear, mut perceptual, mut samples) = (0.0, 0.0, 0);
        for (x, target) in pairs.iter().step_by(3) {
            let corrected = displayed(colour(*x), &tone);
            for c in 0..3 {
                linear += (corrected[c] - target[c]).powi(2);
                perceptual += (corrected[c].max(0.0).powf(1.0 / 2.2) - target[c].max(0.0).powf(1.0 / 2.2)).powi(2);
                samples += 1;
            }
        }
        if linear.is_finite() && perceptual.is_finite() && best.as_ref().is_none_or(|b| perceptual < b.0) {
            best = Some((perceptual, linear, samples, look));
        }
    }
    let (_, after, samples, look) = best?;
    let original_tone = ToneMap::new(0.0, 0.0, 0.0);
    let before: f64 = pairs
        .iter()
        .step_by(3)
        .map(|(x, target)| {
            let original = displayed(*x, &original_tone);
            (0..3).map(|c| (original[c] - target[c]).powi(2)).sum::<f64>()
        })
        .sum();
    if lightcraft_pipeline::profiling() {
        eprintln!(
            "[profile] camera look holdout RMS {:.5} -> {:.5} ({samples} channels{}{})",
            (before / samples as f64).sqrt(),
            (after / samples as f64).sqrt(),
            if edge_limit.is_some() { ", away from edges" } else { "" },
            if tone_fit == ToneFit::ConditionalMedian { ", conditional-median tone" } else { "" }
        );
    }
    if samples == 0 || after >= before * MIN_IMPROVEMENT || after / samples as f64 > MAX_HOLDOUT_RMS.powi(2) {
        return None;
    }
    Some(look)
}

/// Weight of the photo's own colour matrix in a [`fit_partial`] look; the rest is the identity (the
/// neutral fallback's generic camera ≈ sRGB colour). On the CC0 archive's proxies, with the
/// matrix at full weight 2 of the 23 colourful photos without a look came out more than 2° further
/// from the camera's hues than the neutral fallback, at half weight none (median hue error 9.6° with
/// the generic colour, which the tone and chroma curves make visible, 5.1° at half weight).
const PARTIAL_MATRIX_WEIGHT: f64 = 0.5;

/// Smallest rank correlation of scene and camera JPEG luminance over a [`fit_partial`] look's pairs:
/// the JPEG must show the same picture. A look that only matches the brightness distribution of an
/// unrelated picture can still beat the dark neutral fallback pixel for pixel. On the CC0 archive's
/// proxies the accepted full looks have 0.55 or more, lens-corrected compacts without one 0.53–0.77,
/// three old Coolpix files whose raw decodes wrongly 0.07–0.27.
const PARTIAL_MIN_RANK_CORRELATION: f64 = 0.5;

/// Spearman rank correlation of the scene's and the JPEG's luminance over `pairs`.
fn rank_correlation(pairs: &[([f64; 3], [f64; 3])]) -> f64 {
    let ranks = |values: Vec<f64>| {
        let mut order: Vec<usize> = (0..values.len()).collect();
        order.sort_by(|a, b| values[*a].total_cmp(&values[*b]));
        let mut ranks = vec![0.0; values.len()];
        for (rank, i) in order.into_iter().enumerate() {
            ranks[i] = rank as f64;
        }
        ranks
    };
    let scene = ranks(pairs.iter().map(|(x, _)| luma(*x)).collect());
    let jpeg = ranks(pairs.iter().map(|(_, y)| luma(*y)).collect());
    let mean = (pairs.len() as f64 - 1.0) / 2.0;
    let (covariance, variance) =
        scene.iter().zip(&jpeg).fold((0.0, 0.0), |(c, v), (a, b)| (c + (a - mean) * (b - mean), v + (a - mean) * (a - mean)));
    if variance > 0.0 { covariance / variance } else { 0.0 }
}

/// [`rank_correlation`] with tied luminances sharing their mean rank, and 0 when either side has no spread. Without
/// colour to require, a JPEG may be flat (one grey for every pixel): [`rank_correlation`] ranks ties in pixel order,
/// so a flat JPEG of a scene whose pixels are stored in brightness order looks perfectly related.
fn rank_correlation_ties(pairs: &[([f64; 3], [f64; 3])]) -> f64 {
    let ranks = |values: Vec<f64>| {
        let mut order: Vec<usize> = (0..values.len()).collect();
        order.sort_by(|a, b| values[*a].total_cmp(&values[*b]));
        let mut ranks = vec![0.0; values.len()];
        let mut start = 0;
        while start < order.len() {
            let mut end = start + 1;
            while end < order.len() && values[order[end]] == values[order[start]] {
                end += 1;
            }
            let mean = (start + end - 1) as f64 / 2.0;
            for &i in &order[start..end] {
                ranks[i] = mean;
            }
            start = end;
        }
        ranks
    };
    let scene = ranks(pairs.iter().map(|(x, _)| luma(*x)).collect());
    let jpeg = ranks(pairs.iter().map(|(_, y)| luma(*y)).collect());
    let mean = (pairs.len() as f64 - 1.0) / 2.0;
    let (covariance, scene_variance, jpeg_variance) = scene
        .iter()
        .zip(&jpeg)
        .fold((0.0, 0.0, 0.0), |(c, va, vb), (a, b)| (c + (a - mean) * (b - mean), va + (a - mean) * (a - mean), vb + (b - mean) * (b - mean)));
    if scene_variance > 0.0 && jpeg_variance > 0.0 { covariance / (scene_variance * jpeg_variance).sqrt() } else { 0.0 }
}

/// A partial look for a photo whose full look misses the acceptance gates (lens-corrected JPEGs of
/// compacts, dull or nearly colourless scenes): the tone curve by quantile matching, which needs
/// no pixel-exact registration and was the largest part of the neutral fallback's error (rendered
/// 9–22 L* too dark), the chroma curve (the fallback kept about a third of the camera's
/// colourfulness), and, when the scene has enough colour to learn one (the full look's condition),
/// the photo's own colour matrix at [`PARTIAL_MATRIX_WEIGHT`]. No hue/saturation table. Tried on
/// all pixels and then, like the full look, on the pixels away from edges; used only when it cuts
/// the held-out squared error to below [`MIN_IMPROVEMENT`] of the neutral fallback's, in linear
/// and in gamma-encoded display values, and when the JPEG shows the same picture
/// ([`PARTIAL_MIN_RANK_CORRELATION`]). There is no limit on its own error: it only has to be clearly
/// closer to the camera JPEG than what the photo would get otherwise.
fn fit_partial(sensor: &Rgb32f, reference: &Rgb32f, highlights: &HighlightCheck) -> Option<CameraLook> {
    [None, Some(EDGE_CONTRAST)].into_iter().find_map(|edge_limit| fit_partial_on(sensor, reference, edge_limit, highlights))
}

fn fit_partial_on(sensor: &Rgb32f, reference: &Rgb32f, edge_limit: Option<f32>, highlights: &HighlightCheck) -> Option<CameraLook> {
    // as for a known colour model: enough signal for the tone, but no monochrome reference
    let (pairs, bright) = collect_pairs(sensor, reference, 0.005, edge_limit, false)?;
    let related = rank_correlation(&pairs);
    if related < PARTIAL_MIN_RANK_CORRELATION {
        if lightcraft_pipeline::profiling() {
            eprintln!("[profile] camera look partial: luminance rank correlation {related:.3}, not the same picture");
        }
        return None;
    }
    let spread = |y: &[f64; 3]| y.iter().copied().fold(f64::NEG_INFINITY, f64::max) - y.iter().copied().fold(f64::INFINITY, f64::min);
    let colourful = pairs.iter().filter(|(_, y)| spread(y) > 0.05).count() >= pairs.len() / 20;
    let matrix = match colourful.then(|| fit_matrix(&pairs)).flatten() {
        Some(own) => Mat3(std::array::from_fn(|r| {
            std::array::from_fn(|c| Mat3::IDENTITY.0[r][c] + PARTIAL_MATRIX_WEIGHT * (own.0[r][c] - Mat3::IDENTITY.0[r][c]))
        })),
        None => Mat3::IDENTITY,
    };
    let tone = fit_tone_with_shadows(ToneFit::Quantile, &pairs, sensor, reference, edge_limit, |x| matrix.apply(x))?;
    let mut look = CameraLook { matrix, tone, hue_sat: None };
    if let Some(tone) = fit_chroma(&bright, &look) {
        look.tone = tone;
    }
    let (tone, neutral) = (ToneMap::camera(&look.tone, 0.0, 0.0, 0.0), ToneMap::new(0.0, 0.0, 0.0));
    if !highlights_hold(&look.tone, highlights, |x| matrix.apply(x)) {
        return None;
    }
    let encoded = |v: f64| v.max(0.0).powf(1.0 / 2.2);
    let (mut before, mut after, mut before_encoded, mut after_encoded, mut samples) = (0.0, 0.0, 0.0, 0.0, 0);
    for (x, target) in pairs.iter().step_by(3) {
        let (fallback, partial) = (displayed(*x, &neutral), displayed(matrix.apply(*x), &tone));
        for c in 0..3 {
            before += (fallback[c] - target[c]).powi(2);
            after += (partial[c] - target[c]).powi(2);
            before_encoded += (encoded(fallback[c]) - encoded(target[c])).powi(2);
            after_encoded += (encoded(partial[c]) - encoded(target[c])).powi(2);
            samples += 1;
        }
    }
    if lightcraft_pipeline::profiling() {
        eprintln!(
            "[profile] camera look partial (tone{}{}) holdout RMS {:.5} -> {:.5} ({samples} channels{})",
            if look.tone.chroma().iter().any(|k| *k != 1.0) { ", chroma" } else { "" },
            if matrix != Mat3::IDENTITY { ", damped matrix" } else { "" },
            (before / samples.max(1) as f64).sqrt(),
            (after / samples.max(1) as f64).sqrt(),
            if edge_limit.is_some() { ", away from edges" } else { "" }
        );
    }
    let better =
        after.is_finite() && after_encoded.is_finite() && after < before * MIN_IMPROVEMENT && after_encoded < before_encoded * MIN_IMPROVEMENT;
    (samples > 0 && better).then_some(look)
}

/// A camera JPEG pixel's channels may differ by this much (linear) and still count as grey.
const GREY_SPREAD: f32 = 0.002;

/// Whether the camera JPEG is black and white: a monochrome picture style (Canon Monochrome, Sony B/W, Nikon
/// Monochrome), shot from a full-colour raw. All but one proxy pixel in a thousand are grey within
/// [`GREY_SPREAD`]. A toned monochrome (sepia) has colour, so it isn't one.
fn monochrome(reference: &Rgb32f) -> bool {
    let coloured = reference
        .data
        .iter()
        .filter(|p| {
            let (min, max) = p.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), v| (lo.min(*v), hi.max(*v)));
            !p.iter().all(|v| v.is_finite()) || max - min > GREY_SPREAD
        })
        .count();
    reference.data.len() >= 256 && coloured.saturating_mul(1000) <= reference.data.len()
}

/// The look of a raw whose camera JPEG is black and white ([`monochrome`]; issue #577) and gave no look with a camera
/// profile's colour in [`fit_preview`] (or whose model has no profile). Such a JPEG has no colour to fit, so the photo
/// would start from the spectral matrices or the neutral fallback with the default tone curve, about 1.3 EV darker
/// and flatter than the same body's photos shot in colour. But the JPEG still shows the camera's tone curve.
/// `transform` is the colour the photo gets otherwise (the spectral matrices or the fallback's); the look keeps it and
/// adds the tone curve alone ([`fit_tone_only_with`]), as [`fit_look_with`] tries the photo's own colour after a profile's. The
/// chroma curve stays neutral: the JPEG has no colourfulness to learn.
fn monochrome_look(raw: &RawImage, bytes: &[u8], transform: &CameraTransform, lens: Option<&LensGeometry>) -> Option<CameraLook> {
    if !file_local_look(raw.format) {
        return None;
    }
    let (sensor, reference, clipped) = proxies(raw, bytes, transform, PROXY, 0.0, lens)?;
    if !monochrome(&reference) {
        return None;
    }
    // Sony's Dynamic Range Optimizer brightens a B/W JPEG as much as a colour one
    let dro = lightcraft_raw::embedded_preview_dynamic_range_optimized(bytes) == Some(true);
    let unclipped = without_clipped(&sensor, &clipped);
    let bright = HighlightCheck { pairs: highlights(&sensor, &reference), dro, grey: true };
    let look = fit_tone_only_with(unclipped.as_ref().unwrap_or(&sensor), &reference, None, &bright)?;
    let look = if dro { with_dro_off_tone(look) } else { look };
    if lightcraft_pipeline::profiling() {
        eprintln!(
            "[profile] {:?} camera look from a black-and-white JPEG: tone {:?}, {} colour, DRO tone replaced {dro}",
            raw.format,
            look.tone,
            if transform.matrix_is_fallback { "fallback" } else { "spectral" }
        );
    }
    Some(look)
}

/// The tone curve of a black-and-white camera JPEG, on top of `colour` (a camera profile's, in the sensor proxy's
/// space) or, without one, of the sensor proxy's own colour. Fitted like [`fit_partial`]'s, by quantile matching of
/// the luminances, on all pixels and then away from edges, and used only when the JPEG shows the same picture
/// ([`PARTIAL_MIN_RANK_CORRELATION`]) and the look cuts the held-out squared luminance error to below
/// [`MIN_IMPROVEMENT`] of the default curve's, in linear and in gamma-encoded display values. A curve narrower than
/// [`NARROW_TONE_RANGE`] must also keep the JPEG's bright areas ([`highlights_hold`], by luminance).
fn fit_tone_only_with(
    sensor: &Rgb32f,
    reference: &Rgb32f,
    colour: Option<(Mat3, Option<HsvTable>)>,
    highlights: &HighlightCheck,
) -> Option<CameraLook> {
    [None, Some(EDGE_CONTRAST)].into_iter().find_map(|edge_limit| fit_tone_only_on(sensor, reference, &colour, edge_limit, highlights))
}

/// [`fit_tone_only_with`] for a photo without Sony's DRO, checked against its own bright areas.
#[cfg(test)]
fn fit_tone_only(sensor: &Rgb32f, reference: &Rgb32f, colour: Option<(Mat3, Option<HsvTable>)>) -> Option<CameraLook> {
    fit_tone_only_with(sensor, reference, colour, &HighlightCheck { pairs: highlights(sensor, reference), dro: false, grey: true })
}

fn fit_tone_only_on(
    sensor: &Rgb32f,
    reference: &Rgb32f,
    colour: &Option<(Mat3, Option<HsvTable>)>,
    edge_limit: Option<f32>,
    highlights: &HighlightCheck,
) -> Option<CameraLook> {
    // every pair counts: the JPEG has no colour to require
    let (pairs, _) = collect_pairs(sensor, reference, f32::NEG_INFINITY, edge_limit, false)?;
    fit_tone_only_pairs(sensor, reference, colour, edge_limit, &pairs, false, highlights)
}

/// A near-black frame may have enough luminance signal for tone but fewer than 256 colour pairs.
/// Try only after every existing look fails. Keep the colour of the source transform,
/// exclude clipped pixels from training, and keep their luminance at least as close to the JPEG
/// as the default curve does. Bright JPEGs need the existing fits to constrain their highlights.
fn fit_dark_tone(sensor: &Rgb32f, reference: &Rgb32f, clipped: &[bool]) -> Option<CameraLook> {
    if (sensor.width, sensor.height) != (reference.width, reference.height)
        || sensor.data.len() != reference.data.len()
        || clipped.len() != sensor.data.len()
        || sensor.data.is_empty()
        || sensor.data.iter().any(|p| !p.iter().all(|v| v.is_finite()))
        || reference.data.iter().any(|p| !p.iter().all(|v| v.is_finite() && *v >= 0.0) || luminance_2020(*p) >= 0.85)
    {
        return None;
    }
    let dark = reference.data.iter().filter(|p| luminance_2020(**p) < 0.015).count();
    if (dark as f64) < reference.data.len() as f64 * 0.9 {
        return None;
    }
    // no JPEG pixel reaches 0.85 here, so the bright-area check has nothing to hold
    let bright = HighlightCheck { pairs: highlights(sensor, reference), dro: false, grey: true };
    [None, Some(EDGE_CONTRAST)].into_iter().find_map(|edge_limit| {
        let pairs: Pairs = sensor
            .data
            .iter()
            .zip(&reference.data)
            .enumerate()
            .filter(|(i, (input, output))| {
                // The same signal floors as the camera toe, without colour ratios or counts.
                !clipped.get(*i).copied().unwrap_or(true)
                    && input.iter().all(|v| *v > 0.0001 && *v < 1.5)
                    && (0.0003..0.85).contains(&luminance_2020(**output))
                    && edge_limit.is_none_or(|limit| local_contrast(sensor, *i) <= limit && local_contrast(reference, *i) <= limit)
            })
            .map(|(_, (input, output))| (input.map(f64::from), output.map(f64::from)))
            .collect();
        if pairs.len() < 256 {
            return None;
        }
        let look = fit_tone_only_pairs(sensor, reference, &None, edge_limit, &pairs, true, &bright)?;
        let (tone, neutral) = (ToneMap::camera(&look.tone, 0.0, 0.0, 0.0), ToneMap::new(0.0, 0.0, 0.0));
        for ((input, target), clipped) in sensor.data.iter().zip(&reference.data).zip(clipped) {
            if *clipped {
                let target = luma(target.map(f64::from));
                let before = luma(displayed(input.map(f64::from), &neutral));
                let after = luma(displayed(input.map(f64::from), &tone));
                if !after.is_finite() || (after - target).abs() > (before - target).abs() {
                    return None;
                }
            }
        }
        Some(look)
    })
}

fn fit_tone_only_pairs(
    sensor: &Rgb32f,
    reference: &Rgb32f,
    colour: &Option<(Mat3, Option<HsvTable>)>,
    edge_limit: Option<f32>,
    pairs: &Pairs,
    dark: bool,
    highlights: &HighlightCheck,
) -> Option<CameraLook> {
    let kind = if dark { "near-black scene" } else { "black-and-white JPEG" };
    let related = rank_correlation_ties(pairs);
    if related < PARTIAL_MIN_RANK_CORRELATION {
        if lightcraft_pipeline::profiling() {
            eprintln!("[profile] camera look from a {kind}: luminance rank correlation {related:.3}, not the same picture");
        }
        return None;
    }
    let (matrix, hue_sat) = colour.clone().unwrap_or((Mat3::IDENTITY, None));
    let correction = hue_sat.as_ref().and_then(HueSat::new);
    let coloured = |x: [f64; 3]| {
        let p = matrix.apply(x);
        correction.as_ref().map_or(p, |c| c.apply(p.map(|v| v as f32)).map(f64::from))
    };
    let tone = if dark {
        // Split this wider set once. The camera toe's separate colour/extra split belongs to
        // the original colour pairs and would put some of these held-out pixels into training.
        fit_tone(
            pairs.iter().enumerate().filter(|(i, _)| i % 3 != 0).map(|(_, (x, y))| (luma(coloured(*x).map(|v| v.max(0.0))), luma(*y))).collect(),
        )?
    } else {
        fit_tone_with_shadows(ToneFit::Quantile, pairs, sensor, reference, edge_limit, coloured)?
    };
    let look = CameraLook { matrix, tone, hue_sat: hue_sat.clone() };
    if !highlights_hold(&look.tone, highlights, coloured) {
        return None;
    }
    let (tone, neutral) = (ToneMap::camera(&look.tone, 0.0, 0.0, 0.0), ToneMap::new(0.0, 0.0, 0.0));
    let encoded = |v: f64| v.max(0.0).powf(1.0 / 2.2);
    let (mut before, mut after, mut before_encoded, mut after_encoded, mut samples) = (0.0, 0.0, 0.0, 0.0, 0);
    for (x, target) in pairs.iter().step_by(3) {
        let (fallback, toned, target) = (luma(displayed(*x, &neutral)), luma(displayed(coloured(*x), &tone)), luma(*target));
        before += (fallback - target).powi(2);
        after += (toned - target).powi(2);
        before_encoded += (encoded(fallback) - encoded(target)).powi(2);
        after_encoded += (encoded(toned) - encoded(target)).powi(2);
        samples += 1;
    }
    if lightcraft_pipeline::profiling() {
        eprintln!(
            "[profile] camera look from a {kind}: holdout luminance RMS {:.5} -> {:.5} ({samples} pixels{})",
            (before / samples.max(1) as f64).sqrt(),
            (after / samples.max(1) as f64).sqrt(),
            if edge_limit.is_some() { ", away from edges" } else { "" }
        );
    }
    let better =
        after.is_finite() && after_encoded.is_finite() && after < before * MIN_IMPROVEMENT && after_encoded < before_encoded * MIN_IMPROVEMENT;
    (samples > 0 && better).then_some(look)
}

/// Linear Rec.2020 D65 → linear ProPhoto RGB D50, the space DNG hue/saturation tables work in.
fn to_prophoto() -> Mat3 {
    PROPHOTO.from_xyz().mul(&bradford(D65, D50)).mul(&REC2020.to_xyz())
}

/// [`HueSat::apply`] restores the luminance in full only if the table kept at least this share of it. A colour
/// the table darkens to [`NO_LUMINANCE_KEPT`] or less (only near-black noise, whose channels go negative) keeps
/// its colour; in between the two are blended, so noisy shadows show no step.
const MIN_LUMINANCE_KEPT: f32 = 0.25;
const NO_LUMINANCE_KEPT: f32 = 0.15;

/// How much of the table's colour a pixel gets when the table kept `share` of its luminance: 0 up to
/// [`NO_LUMINANCE_KEPT`], 1 from [`MIN_LUMINANCE_KEPT`], a smoothstep between.
fn restore_weight(share: f32) -> f32 {
    if !share.is_finite() {
        return 0.0;
    }
    let t = ((share - NO_LUMINANCE_KEPT) / (MIN_LUMINANCE_KEPT - NO_LUMINANCE_KEPT)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// A fitted [`CameraLook::hue_sat`] table, ready to apply to linear Rec.2020 pixels. It changes
/// hue and saturation only: luminance is restored, the camera tone curve owns it.
pub(crate) struct HueSat<'a> {
    table: &'a HsvTable,
    to: [[f32; 3]; 3],
    from: [[f32; 3]; 3],
}

impl<'a> HueSat<'a> {
    pub fn new(table: &'a HsvTable) -> Option<HueSat<'a>> {
        let to = to_prophoto();
        Some(HueSat { table, to: to.to_f32(), from: to.inverse()?.to_f32() })
    }

    #[inline]
    pub fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        let mul = |m: &[[f32; 3]; 3], v: [f32; 3]| -> [f32; 3] { std::array::from_fn(|i| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2]) };
        let out = mul(&self.from, self.table.apply(mul(&self.to, rgb)));
        let (before, after) = (luminance_2020(rgb), luminance_2020(out));
        // restoring the luminance divides by the table's: near black, where noise makes some channels
        // negative, the table can bring it close to 0, and the gain would turn the pixel white (#820)
        if before <= 0.0 || after <= 0.0 || !out.iter().all(|v| v.is_finite()) {
            return rgb;
        }
        let w = restore_weight(after / before);
        if w >= 1.0 {
            return out.map(|v| v * before / after);
        }
        if w <= 0.0 {
            return rgb;
        }
        std::array::from_fn(|i| rgb[i] + (out[i] * before / after - rgb[i]) * w)
    }

    /// [`Self::apply`] to `N` pixels at once (`rgb[channel][lane]`), bit for bit, through
    /// [`HsvTable::apply_lanes`], with the guards as selects.
    #[inline]
    pub fn apply_lanes<const N: usize>(&self, rgb: [[f32; N]; 3]) -> [[f32; N]; 3] {
        let mul = |m: &[[f32; 3]; 3], v: &[[f32; N]; 3]| {
            let mut out = [[0.0f32; N]; 3];
            for (row, out) in m.iter().zip(&mut out) {
                for l in 0..N {
                    out[l] = row[0] * v[0][l] + row[1] * v[1][l] + row[2] * v[2][l];
                }
            }
            out
        };
        let out = mul(&self.from, &self.table.apply_lanes(mul(&self.to, &rgb)));
        let mut restored = rgb;
        for l in 0..N {
            let (input, out) = ([rgb[0][l], rgb[1][l], rgb[2][l]], [out[0][l], out[1][l], out[2][l]]);
            let (before, after) = (luminance_2020(input), luminance_2020(out));
            let untouched = (before <= 0.0) | (after <= 0.0) | !(out[0].is_finite() & out[1].is_finite() & out[2].is_finite());
            let w = restore_weight(after / before);
            for (c, restored) in restored.iter_mut().enumerate() {
                let full = out[c] * before / after;
                let blend = input[c] + (full - input[c]) * w;
                // (`w >= 1` and `w <= 0` exclude each other)
                restored[l] = if untouched | (w <= 0.0) {
                    input[c]
                } else if w >= 1.0 {
                    full
                } else {
                    blend
                };
            }
        }
        restored
    }
}

/// HSV hue (degrees) and saturation; `None` for black or non-finite colours.
fn hue_saturation(p: [f64; 3]) -> Option<(f64, f64)> {
    let max = p[0].max(p[1]).max(p[2]);
    let min = p[0].min(p[1]).min(p[2]);
    if !(max.is_finite() && min.is_finite()) || max <= 0.0 {
        return None;
    }
    let d = max - min;
    if d <= 0.0 {
        return Some((0.0, 0.0));
    }
    let h = if max == p[0] {
        ((p[1] - p[2]) / d).rem_euclid(6.0)
    } else if max == p[1] {
        (p[2] - p[0]) / d + 2.0
    } else {
        (p[0] - p[1]) / d + 4.0
    };
    Some((h * 60.0, d / max))
}

/// Table resolution: 5° hue steps (a coarser grid blurred the lime shirt into neighbouring browns
/// that want the opposite shift), saturation 0, 0.25 … 1. Value is not an axis: tone is fitted separately.
const TABLE_HUES: usize = 72;
const TABLE_SATS: usize = 5;
/// Value levels (sRGB-encoded, as DNG tables with encoding 1): cameras turn dark yellows toward
/// orange but bright yellow-greens toward green, which one shift per hue averages away.
const TABLE_VALS: usize = 5;
/// Kernel widths around each table node (hue in degrees, saturation, encoded value).
const KERNEL_HUE: f64 = 2.5;
const KERNEL_SAT: f64 = 0.15;
const KERNEL_VAL: f64 = 0.12;
/// Kernel weight at which a node keeps half of its estimate; sparse nodes shrink to identity.
const SHRINK_WEIGHT: f64 = 5.0;

/// Fit hue shifts and saturation scales of the training pairs left after `matrix`, per (hue,
/// saturation) node, kernel-weighted and shrunk toward identity where the photo has few samples.
/// Saturation 0 stays identity, so neutrals are never tinted.
fn fit_hue_sat(pairs: &[([f64; 3], [f64; 3])], matrix: &Mat3) -> Option<HsvTable> {
    let to = to_prophoto();
    let samples: Vec<(f64, f64, f64, f64, f64)> = pairs
        .iter()
        .enumerate()
        .filter(|(i, _)| i % 3 != 0)
        .filter_map(|(_, (x, y))| {
            let p = to.apply(matrix.apply(*x));
            let (hp, sp) = hue_saturation(p)?;
            let value = f64::from(lightcraft_color::transfer::linear_to_srgb(p[0].max(p[1]).max(p[2]).min(1.0) as f32));
            let (ht, st) = hue_saturation(to.apply(*y))?;
            // hue is meaningless near neutral
            if sp < 0.08 || st < 0.02 {
                return None;
            }
            let shift = ((ht - hp + 180.0).rem_euclid(360.0) - 180.0).clamp(-30.0, 30.0);
            let log_scale = (st / sp).ln().clamp(-0.7, 0.7);
            Some((hp, sp.min(1.0), value, shift, log_scale))
        })
        .collect();
    if samples.len() < 64 {
        return None;
    }
    // Samples by table hue step: a node only looks at hues within its kernel's reach.
    let step = 360.0 / TABLE_HUES as f64;
    let mut by_hue: Vec<Vec<(f64, f64, f64, f64, f64)>> = vec![Vec::new(); TABLE_HUES];
    for sample in samples {
        if let Some(bin) = by_hue.get_mut(((sample.0.rem_euclid(360.0) / step) as usize).min(TABLE_HUES - 1)) {
            bin.push(sample);
        }
    }
    let reach = (4.0 * KERNEL_HUE / step).ceil() as usize + 1;
    use rayon::prelude::*;
    let data: Vec<[f32; 3]> = (0..TABLE_VALS * TABLE_HUES * TABLE_SATS)
        .into_par_iter()
        .map(|index| {
            let (v, h, s) = (index / (TABLE_HUES * TABLE_SATS), index / TABLE_SATS % TABLE_HUES, index % TABLE_SATS);
            if s == 0 {
                return [0.0, 1.0, 1.0];
            }
            let (val, hue, sat) = (v as f64 / (TABLE_VALS - 1) as f64, h as f64 * step, s as f64 / (TABLE_SATS - 1) as f64);
            let (mut weight, mut shift, mut log_scale) = (0.0, 0.0, 0.0);
            for offset in 0..=2 * reach {
                let Some(bin) = by_hue.get((h + TABLE_HUES * 2 + offset - reach) % TABLE_HUES) else { continue };
                for &(hp, sp, vp, dh, ls) in bin {
                    let dhue = (hp - hue + 180.0).rem_euclid(360.0) - 180.0;
                    if dhue.abs() > 4.0 * KERNEL_HUE {
                        continue;
                    }
                    let d2 = (dhue / KERNEL_HUE).powi(2) + ((sp - sat) / KERNEL_SAT).powi(2) + ((vp - val) / KERNEL_VAL).powi(2);
                    let k = (-0.5 * d2).exp() * sp;
                    weight += k;
                    shift += k * dh;
                    log_scale += k * ls;
                }
            }
            if weight <= 0.0 {
                return [0.0, 1.0, 1.0];
            }
            let shrink = 1.0 / (weight + SHRINK_WEIGHT);
            [(shift * shrink) as f32, (log_scale * shrink).exp() as f32, 1.0]
        })
        .collect();
    data.iter().all(|e| e.iter().all(|v| v.is_finite())).then_some(HsvTable {
        hue_divisions: TABLE_HUES,
        sat_divisions: TABLE_SATS,
        val_divisions: TABLE_VALS,
        data,
        srgb_value: true,
    })
}

/// Kernel width of the chroma curve's nodes (display luminance) and the weight at which a node
/// keeps half of its estimate.
const CHROMA_KERNEL: f64 = 0.08;
const CHROMA_SHRINK: f64 = 2.0;

/// The look's tone curve with a chroma-by-display-luminance curve fitted to `pairs` (two thirds
/// train, one third held out), when that lowers the held-out error. Colourfulness is compared as
/// the distance from neutral of luminance-normalised RGB.
fn fit_chroma(pairs: &[([f64; 3], [f64; 3])], look: &CameraLook) -> Option<CameraTone> {
    let correction = look.hue_sat.as_ref().and_then(HueSat::new);
    let scene = |x: &[f64; 3]| {
        let p = look.matrix.apply(*x);
        correction.as_ref().map_or(p, |c| c.apply(p.map(|v| v as f32)).map(f64::from))
    };
    let tone = ToneMap::camera(&look.tone, 0.0, 0.0, 0.0);
    let predict = |x: &[f64; 3]| displayed(scene(x), &tone);
    let chroma = |p: [f64; 3]| {
        let y = luma(p);
        (y > 0.0).then(|| p.iter().map(|v| (v / y - 1.0).powi(2)).sum::<f64>().sqrt())
    };
    let samples: Vec<(f64, f64, f64)> = pairs
        .iter()
        .enumerate()
        .filter(|(i, _)| i % 3 != 0)
        .filter_map(|(_, (x, y))| {
            let p = predict(x);
            let (cp, cy) = (chroma(p)?, chroma(*y)?);
            (cp > 0.05).then(|| (luma(p), cp, (cy.max(1e-3) / cp).ln().clamp(0.05f64.ln(), 2.5f64.ln())))
        })
        .collect();
    if samples.len() < 64 {
        return None;
    }
    let mut curve = [1.0f32; lightcraft_pipeline::tone::CHROMA_N];
    for (j, node) in curve.iter_mut().enumerate() {
        let at = j as f64 / (lightcraft_pipeline::tone::CHROMA_N - 1) as f64;
        let (mut weight, mut sum) = (0.0, 0.0);
        for &(o, cp, log_ratio) in &samples {
            let k = (-0.5 * ((o - at) / CHROMA_KERNEL).powi(2)).exp() * cp;
            weight += k;
            sum += k * log_ratio;
        }
        *node = (sum / (weight + CHROMA_SHRINK)).exp() as f32;
    }
    let fitted = look.tone.with_chroma(curve)?;
    let with = ToneMap::camera(&fitted, 0.0, 0.0, 0.0);
    let error = |map: &ToneMap| -> f64 {
        pairs
            .iter()
            .step_by(3)
            .map(|(x, y)| {
                let p = displayed(scene(x), map);
                (0..3).map(|c| (p[c] - y[c]).powi(2)).sum::<f64>()
            })
            .sum()
    };
    let (before, after) = (error(&tone), error(&with));
    if lightcraft_pipeline::profiling() {
        eprintln!("[profile] ARW chroma curve {curve:?}: held-out error {before:.4} -> {after:.4}");
    }
    (after.is_finite() && after < before).then_some(fitted)
}

/// How the per-photo tone curve is fitted ([`fit_pairs_ordered`] tries them in order).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ToneFit {
    /// [`fit_tone`]: no flat steps, no pixel-exact registration needed.
    Quantile,
    /// [`fit_tone_conditional`]: the earlier fit, kept as the fallback.
    ConditionalMedian,
}

impl ToneFit {
    fn fit(self, pairs: Vec<(f64, f64)>) -> Option<CameraTone> {
        match self {
            ToneFit::Quantile => fit_tone(pairs),
            ToneFit::ConditionalMedian => fit_tone_conditional(pairs),
        }
    }
}

/// The bright areas a narrow tone curve is checked against ([`highlights_hold`]): every proxy pixel
/// whose camera JPEG luminance is 0.85 or more, with its sensor value as developed (for a clipped
/// pixel a lower bound of the scene). Taken from the proxies before any training filter (clipped
/// pixels, edges, the sensor value range of [`collect_pairs`]), so no retry of the search can
/// leave them out.
fn highlights(sensor: &Rgb32f, reference: &Rgb32f) -> Pairs {
    if sensor.data.len() != reference.data.len() {
        return Vec::new();
    }
    sensor
        .data
        .iter()
        .zip(&reference.data)
        .filter(|(x, y)| x.iter().chain(y.iter()).all(|v| v.is_finite() && *v >= 0.0) && luminance_2020(**y) >= 0.85)
        .map(|(x, y)| (x.map(f64::from), y.map(f64::from)))
        .collect()
}

/// Smallest ratio between the scene luminance of the brightest and of the darkest 1/32 of the
/// pairs a tone curve is fitted to (its last and first knot). Below it the training pairs are
/// nearly uniform (under 0.26 EV from their darker to their brighter parts): the curve would be
/// fitted to a sliver of the tone range and the rest of it extrapolated. Above it the acceptance
/// gates judge the curve. This was 1.5, which rejected low-contrast scenes the curve fits very
/// well: in a hazy ILCE-7RM4 burst the frames at 1.46–1.49 opened with the neutral fallback (no full or partial
/// look, 18 L* darker than the camera JPEG) while the frame at 1.51 fitted at a held-out RMS of
/// 0.013, as the others do once allowed (issue #631). Across 135 public Sony files the lowest is 4.0.
/// A curve under [`NARROW_TONE_RANGE`] must also pass [`highlights_hold`].
const MIN_TONE_RANGE: f32 = 1.2;

/// [`MIN_TONE_RANGE`] before issue #631. A curve fitted to a narrower range than this must also
/// render the photo's bright areas no further from the camera JPEG than the neutral fallback does
/// ([`highlights_hold`]). Neither the fit nor the held-out gates see those areas (camera JPEG
/// luminance 0.85 and up, left out of the training pairs by [`collect_pairs`]), and a curve fitted
/// to a sliver of the tone range extrapolates them: in review, a synthetic frame whose curve passed
/// the gates at a held-out RMS of 0.0004 rendered its bright sky at 0.29 where the camera JPEG has
/// 0.93.
const NARROW_TONE_RANGE: f32 = 1.5;

/// What a narrow tone curve is checked against ([`highlights_hold`]).
struct HighlightCheck {
    /// The frame's [`highlights`].
    pairs: Pairs,
    /// The photo's curve will be lowered to Sony's without DRO ([`with_dro_off_tone`]) after the
    /// fit, so the check looks at it lowered.
    dro: bool,
    /// The camera JPEG is black and white ([`fit_tone_only_with`]): the check compares luminance, as that
    /// fit's gates do, since the JPEG has no colour to match.
    grey: bool,
}

/// Whether a look with this `tone` and `colour` (sensor value to the finish stage's input) keeps
/// the frame's [`highlights`] (camera JPEG luminance 0.85 and up, left out of the fit) no further
/// from the camera JPEG than the neutral fallback does, with the tone curve as the photo will get
/// it (lowered for DRO), in luminance alone for a black-and-white JPEG. Only asked of curves narrower than
/// [`NARROW_TONE_RANGE`]: true for wider ones, and for frames without such pixels.
fn highlights_hold(tone: &CameraTone, check: &HighlightCheck, colour: impl Fn([f64; 3]) -> [f64; 3]) -> bool {
    let knots = tone.knots();
    if knots[31][0] >= knots[0][0] * NARROW_TONE_RANGE {
        return true;
    }
    // the curve as the photo will get it
    let used = if check.dro { dro_off(tone) } else { *tone };
    let (map, neutral) = (ToneMap::camera(&used, 0.0, 0.0, 0.0), ToneMap::new(0.0, 0.0, 0.0));
    let (mut look, mut fallback) = (0.0, 0.0);
    for (x, target) in &check.pairs {
        let (rendered, base) = (displayed(colour(*x), &map), displayed(*x, &neutral));
        if check.grey {
            look += (luma(rendered) - luma(*target)).powi(2);
            fallback += (luma(base) - luma(*target)).powi(2);
            continue;
        }
        for c in 0..3 {
            look += (rendered[c] - target[c]).powi(2);
            fallback += (base[c] - target[c]).powi(2);
        }
    }
    let hold = look.is_finite() && look <= fallback;
    if !hold && lightcraft_pipeline::profiling() {
        eprintln!("[profile] camera look: narrow tone curve renders the bright areas worse than the fallback ({look:.3} > {fallback:.3})");
    }
    hold
}

/// The camera's tone curve from scene/JPEG luminance pairs, by quantile matching: knot `i` pairs
/// the median of the `i`-th 1/32 of the sorted scene luminances with the median of the `i`-th
/// 1/32 of the sorted JPEG luminances. A tone curve is monotone, so it maps each quantile of the
/// scene to the same quantile of the JPEG; matching the two distributions needs no pixel-exact
/// registration and is increasing by construction.
///
/// Binning the pairs by scene luminance and taking the JPEG's median per bin instead (followed by
/// isotonic pooling) fails on real files: camera JPEGs are lens-corrected and sharpened, so on a
/// 96 px proxy of a busy scene neighbouring pairs disagree by more than the gap between bins.
/// The conditional medians then zig-zag, pooling turns them into flat steps (13 of 31 segments
/// on a Z 6II forest, issue #475) that render as posterised patches, and regression toward the
/// mean flattens both ends of the curve (lifted blacks, dull highlights).
fn fit_tone(pairs: Vec<(f64, f64)>) -> Option<CameraTone> {
    if pairs.len() < 128 || !pairs.iter().all(|(x, y)| x.is_finite() && *x > 0.0 && y.is_finite()) {
        return None;
    }
    let mut xs: Vec<f64> = pairs.iter().map(|p| p.0).collect();
    let mut ys: Vec<f64> = pairs.iter().map(|p| p.1).collect();
    xs.sort_by(f64::total_cmp);
    ys.sort_by(f64::total_cmp);
    let n = xs.len();
    let mut knots = [[0.0; 2]; 32];
    for (i, knot) in knots.iter_mut().enumerate() {
        let (a, b) = (i * n / 32, (i + 1) * n / 32);
        let mid = a + (b - a) / 2;
        *knot = [*xs.get(mid)? as f32, *ys.get(mid)? as f32];
    }
    if knots[31][0] < knots[0][0] * MIN_TONE_RANGE {
        return None;
    }
    CameraTone::new(knots)
}

fn median(values: &mut [f64]) -> Option<f64> {
    values.sort_by(f64::total_cmp);
    values.get(values.len() / 2).copied()
}

/// The earlier tone fit, kept as the fallback of [`ToneFit`]: the JPEG's median per equal-population
/// bin of scene luminance, then pooled-adjacent-violators isotonic regression. It follows the
/// per-pixel pairs closely when the two images are registered, which is why it can clear the
/// acceptance gates where [`fit_tone`] just misses them, but on lens-corrected JPEGs of busy scenes
/// the pooling leaves flat steps (issue #475).
fn fit_tone_conditional(mut pairs: Vec<(f64, f64)>) -> Option<CameraTone> {
    if pairs.len() < 128 || !pairs.iter().all(|(x, y)| x.is_finite() && *x > 0.0 && y.is_finite()) {
        return None;
    }
    pairs.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut knots = [[0.0; 2]; 32];
    for (i, knot) in knots.iter_mut().enumerate() {
        let bin = pairs.get(i * pairs.len() / 32..(i + 1) * pairs.len() / 32)?;
        let mut xs: Vec<_> = bin.iter().map(|p| p.0).collect();
        let mut ys: Vec<_> = bin.iter().map(|p| p.1).collect();
        *knot = [median(&mut xs)? as f32, median(&mut ys)? as f32];
    }
    if knots[31][0] < knots[0][0] * MIN_TONE_RANGE {
        return None;
    }
    // Pool adjacent violating bins (isotonic regression): no reversals or arbitrary polynomial.
    let mut blocks: Vec<(f32, usize)> = Vec::new();
    for knot in knots {
        blocks.push((knot[1], 1));
        while blocks.len() >= 2 {
            let (a, an) = *blocks.get(blocks.len() - 2)?;
            let (b, bn) = *blocks.last()?;
            if a <= b {
                break;
            }
            blocks.truncate(blocks.len() - 2);
            blocks.push(((a * an as f32 + b * bn as f32) / (an + bn) as f32, an + bn));
        }
    }
    let mut i = 0;
    for (y, n) in blocks {
        for knot in knots.get_mut(i..i + n)? {
            knot[1] = y;
        }
        i += n;
    }
    CameraTone::new(knots)
}

#[cfg(test)]
mod tests;
