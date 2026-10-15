# Nikon bodies: sensor data, image area and edge bands

> **Last updated:** 2026-10-11 · **Change:** created (every Nikon raw on raw.pixls.us; 138 CC0 samples measured with `cargo xtask raw-frame`) · **Scope:** Nikon NEF / NRW

Nikon raws hold more sensor data than the picture: a few lines of extra picture around the camera's own frame and,
on many bodies, optical black or masked lines that are not picture at all. Newer bodies record the picture's area in
the maker note (`CropArea`, 0x0045) and LightCraft crops to it. Older ones record nothing, so the decoder keeps the
whole data (less trailing dark columns), and masked lines inside it are developed as a coloured or dark bar on that
edge ([#976](https://github.com/storytold/lightcraft/issues/976), [#914](https://github.com/storytold/lightcraft/issues/914)).
Such a body gets an entry in `MEASURED_BODIES` (`crates/raw/src/vendor/nef.rs`): the frame of its camera JPEG inside
the data, applied only to that model, data size and sample depth. Bodies whose data holds only picture beyond the
camera frame get no entry and render as before.

## Summary

- **Record `CropArea`:** 22 models, 53 data sizes: handled.
- **Without `CropArea`:** 66 models in 88 groups (model, data size, depth), plus 7 groups without a CC0 sample:
  - measured entry (cropped to the camera frame): 36
  - fine (only picture beyond the camera frame): 13
  - bar, pending more files: 3
  - dark lines, pending more files: 4
  - no clean alignment: 25
  - not decoded: 7

Status: ✅ **measured entry** · ✅ **fine** (a few extra pixels of picture) · ⚠️ **bar** (masked lines inside the crop:
a coloured or dark edge) · ❓ **check** (dark lines inside the crop: optical black, or shadow in that sample) ·
❓ **unmeasured** (no clean alignment: a compact's JPEG is distortion-corrected, or the samples are soft or dark) ·
**not decoded** (a raw variant LightCraft can't read yet).

## Pending

Bodies whose crop keeps lines without picture but that lack the evidence for an entry (see *Adding a body*). Each
needs more files with detail across the frame, from raw.pixls.us or contributed (any licence: only the measurement
is committed, never the file).

| Model | Data | Bits | In the crop today | Needs |
|---|---|---:|---|---|
| NIKON 1 V1 | 3904 × 2606 | 12 | dark lines in crop (optical black or a dark sample): bottom 2 | 2 more clean file(s) |
| NIKON 1 V2 | 4620 × 3084 | 12 | dark lines in crop (optical black or a dark sample): bottom 3 | 1 more clean file(s) |
| NIKON D200 | 3904 × 2616 | 12 | masked lines in crop: left 1, right 1 | 1 more clean file(s) |
| NIKON D3100 | 4672 × 3084 | 12 | masked lines in crop: left 5, right 13 | 2 more clean file(s) |
| NIKON D3200 | 6080 × 4012 | 12 | masked lines in crop: right 45 | 2 more clean file(s) |
| NIKON D750 | 6032 × 4032 | 12 | dark lines in crop (optical black or a dark sample): left 8, right 8, top 8 | 1 more clean file(s) |
| NIKON D750 | 6032 × 4032 | 14 | dark lines in crop (optical black or a dark sample): left 4, right 8, top 8 | 1 more clean file(s) |
| NIKON D50 | 3040 × 2014 | 12 | masked lines at the edges: right 1 | files that align cleanly |

## Bodies without `CropArea`

One row per model, sensor data size and sample depth (each covers the compression modes listed). *Frame*: where the
camera's full-size JPEG sits in the data and how many measured files agree (bit depths pooled). *Files*: samples of
the group on raw.pixls.us. *Sample*: the first file measured, in `corpus/raw/nikon-inventory/`.

| Model | Data | Bits | Files | Modes | Frame | Status | Notes | Sample |
|---|---|---:|---:|---|---|---|---|---|
| COOLPIX A | 4992 × 3280 | 14 | 1 | lossy 2 | (8, 8), 1 of 1 files | ✅ measured entry | crop (8, 8, 4928, 3264); siblings: NIKON D5100 (3), NIKON D7000 (1) | `coolpix-a-4992x3280-14bit.nef` |
| COOLPIX A1000 | 4608 × 3456 | 12 | 1 | uncompressed | unclear (4608×3456, score 0.21) | ❓ unmeasured | no masked edge lines found | `coolpix-a1000-4608x3456-12bit.nrw` |
| COOLPIX B700 | 5200 × 3902 | 12 | 1 | uncompressed | unclear (5184×3888, score 0.44) | ❓ unmeasured | no masked edge lines found | `coolpix-b700-5200x3902-12bit.nrw` |
| COOLPIX P1000 | 4624 × 3470 | 12 | 1 | uncompressed | unclear (4608×3456, score 0.30) | ❓ unmeasured | no masked edge lines found | `coolpix-p1000-4624x3470-12bit.nrw` |
| COOLPIX P1100 | 4624 × 3470 | 12 | 1 | uncompressed | unclear (4608×3456, score 0.20) | ❓ unmeasured | no masked edge lines found | `coolpix-p1100-4624x3470-12bit.nrw` |
| COOLPIX P330 | 4032 × 3024 | 12 | 1 | HE / other | — | not decoded | Nikon compressed NEF without a linearization table (maker note 0x96) | `coolpix-p330-4032x3024-12bit.nrw` |
| COOLPIX P7700 | 4032 × 3024 | 12 | 1 | HE / other | — | not decoded | Nikon compressed NEF without a linearization table (maker note 0x96) | `coolpix-p7700-4032x3024-12bit.nrw` |
| COOLPIX P7800 | 4032 × 3024 | 12 | 1 | HE / other | — | not decoded | Nikon compressed NEF without a linearization table (maker note 0x96) | `coolpix-p7800-4032x3024-12bit.nrw` |
| COOLPIX P950 | 4624 × 3470 | 12 | 1 | uncompressed | unclear (4608×3456, score 0.21) | ❓ unmeasured | no masked edge lines found | `coolpix-p950-4624x3470-12bit.nrw` |
| E5700 | 2576 × 1924 | 12 | 1 | uncompressed | no full-size JPEG | ❓ unmeasured | no masked edge lines found | `e5700-2576x1924-12bit.nef` |
| E8400 | 3280 × 2454 | 12 | 1 | uncompressed | no full-size JPEG | ❓ unmeasured | no masked edge lines found | `e8400-3280x2454-12bit.nef` |
| E8800 | 3280 × 2454 | 12 | 1 | uncompressed | no full-size JPEG | ❓ unmeasured | no masked edge lines found | `e8800-3280x2454-12bit.nef` |
| NIKON 1 AW1 | 4620 × 3084 | 12 | 1 | lossy 2 | (12, 10), 1 of 1 files | ✅ fine | only picture beyond the camera frame | `nikon-1-aw1-4620x3084-12bit.nef` |
| NIKON 1 J1 | 3904 × 2606 | 12 | 3 | lossy 2 | no full-size JPEG | ❓ unmeasured | no masked edge lines found | `nikon-1-j1-3904x2606-12bit.nef` |
| NIKON 1 J2 | 3904 × 2606 | 12 | 1 | lossy 2 | no full-size JPEG | ❓ unmeasured | no masked edge lines found | `nikon-1-j2-3904x2606-12bit.nef` |
| NIKON 1 J3 | 4620 × 3084 | 12 | 1 | lossy 2 | (6, 6), 1 of 1 files | ✅ fine | only picture beyond the camera frame | `nikon-1-j3-4620x3084-12bit.nef` |
| NIKON 1 J4 | 5248 × 3504 | 12 | 1 | lossy 2 | unclear (5232×3488, score 0.42) | ❓ unmeasured | no masked edge lines found | `nikon-1-j4-5248x3504-12bit.nef` |
| NIKON 1 J5 | 5584 × 3726 | 12 | 2 | lossy 2 | unclear (5568×3712, score 0.30) | ❓ unmeasured | no masked edge lines found | `nikon-1-j5-5584x3726-12bit.nef` |
| NIKON 1 S1 | 3948 × 2604 | 12 | 1 | lossy 2 | no full-size JPEG | ❓ unmeasured | no masked edge lines found | `nikon-1-s1-3948x2604-12bit.nef` |
| NIKON 1 V1 | 3904 × 2606 | 12 | 1 | lossy 2 | (12, 6), 1 of 1 files | ❓ check | dark lines in crop (optical black or a dark sample): bottom 2; pending: 2 more clean file(s) | `nikon-1-v1-3904x2606-12bit.nef` |
| NIKON 1 V2 | 4620 × 3084 | 12 | 1 | lossy 2 | (6, 6), 1 of 1 files | ❓ check | dark lines in crop (optical black or a dark sample): bottom 3; pending: 1 more clean file(s) | `nikon-1-v2-4620x3084-12bit.nef` |
| NIKON 1 V3 | 5248 × 3504 | 12 | 1 | lossy 2 | (8, 8), 1 of 1 files | ✅ fine | only picture beyond the camera frame | `nikon-1-v3-5248x3504-12bit.nef` |
| NIKON D100 | 3034 × 2024 | 12 | 1 | uncompressed | — | not decoded | Nikon compressed NEF without a linearization table (maker note 0x96) | `nikon-d100-3034x2024-12bit.nef` |
| NIKON D1H | 2012 × 1324 | 12 | 1 | uncompressed | no full-size JPEG | ❓ unmeasured | no masked edge lines found | `nikon-d1h-2012x1324-12bit.nef` |
| NIKON D1X | 4028 × 1324 | 12 | 2 | compressed, uncompressed | — | not decoded | Nikon compressed NEF without a linearization table (maker note 0x96) | `nikon-d1x-4028x1324-12bit.nef` |
| NIKON D200 | 3904 × 2616 | 12 | 2 | lossy 1, uncompressed | (16, 12), 2 of 2 files | ⚠️ bar | masked lines in crop: left 1, right 1; pending: 1 more clean file(s) | `nikon-d200-3904x2616-12bit.nef` |
| NIKON D2H | 2496 × 1648 | 12 | 2 | lossy 1, uncompressed | no full-size JPEG | ❓ unmeasured | no masked edge lines found | `nikon-d2h-2496x1648-12bit.nef` |
| NIKON D2Hs | 2496 × 1648 | 12 | 1 | uncompressed | (16, 9), 1 of 1 files | ✅ fine | only picture beyond the camera frame | `nikon-d2hs-2496x1648-12bit.nef` |
| NIKON D2X | 4320 × 2868 | 12 | 2 | lossy 1, uncompressed | (16, 10), 1 of 1 files | ✅ fine | only picture beyond the camera frame | `nikon-d2x-4320x2868-12bit.nef` |
| NIKON D2Xs | 4320 × 2868 | 12 | 2 | lossy 1, uncompressed | (16, 10), 2 of 2 files | ✅ measured entry | crop (16, 10, 4288, 2848); siblings: NIKON D2X (1) | `nikon-d2xs-4320x2868-12bit.nef` |
| NIKON D3 | 4288 × 2844 | 12 | 3 | lossless, lossy 2, uncompressed | (15, 6), 6 of 6 files | ✅ measured entry | crop (15, 6, 4256, 2832) | `nikon-d3-4288x2844-12bit.nef` |
| NIKON D3 | 4288 × 2844 | 14 | 3 | lossless, lossy 2, uncompressed | (15, 6), 6 of 6 files | ✅ measured entry | crop (15, 6, 4256, 2832) | `nikon-d3-4288x2844-14bit.nef` |
| NIKON D300 | 4352 × 2868 | 12 | 3 | lossless, lossy 2, uncompressed | (15, 10), 6 of 6 files | ✅ measured entry | crop (15, 10, 4288, 2848) | `nikon-d300-4352x2868-12bit.nef` |
| NIKON D300 | 4352 × 2868 | 14 | 3 | lossless, lossy 2, uncompressed | (15, 10), 6 of 6 files | ✅ measured entry | crop (15, 10, 4288, 2848) | `nikon-d300-4352x2868-14bit.nef` |
| NIKON D3000 | 3904 × 2616 | 12 | 1 | lossy 1 | (14, 9), 1 of 1 files | ✅ measured entry | crop (14, 9, 3872, 2592); siblings: NIKON D40X (1), NIKON D60 (1), NIKON D80 (1) | `nikon-d3000-3904x2616-12bit.nef` |
| NIKON D300S | 4352 × 2868 | 12 | 3 | lossless, lossy 2, uncompressed | (15, 10), 4 of 4 files | ✅ measured entry | crop (15, 10, 4288, 2848) | `nikon-d300s-4352x2868-12bit.nef` |
| NIKON D300S | 4352 × 2868 | 14 | 3 | lossless, lossy 2, uncompressed | (15, 10), 4 of 4 files | ✅ measured entry | crop (15, 10, 4288, 2848) | `nikon-d300s-4352x2868-14bit.nef` |
| NIKON D3100 | 4672 × 3084 | 12 | 1 | lossy 2 | (24, 6), 1 of 1 files | ⚠️ bar | masked lines in crop: left 5, right 13; pending: 2 more clean file(s) | `nikon-d3100-4672x3084-12bit.nef` |
| NIKON D3200 | 6080 × 4012 | 12 | 1 | lossy 2 | (8, 6), 1 of 1 files | ⚠️ bar | masked lines in crop: right 45; pending: 2 more clean file(s) | `nikon-d3200-6080x4012-12bit.nef` |
| NIKON D3300 | 6016 × 4016 | 12 | 1 | lossy 2 | (8, 8), 1 of 1 files | ✅ fine | only picture beyond the camera frame | `nikon-d3300-6016x4016-12bit.nef` |
| NIKON D3S | 4288 × 2844 | 12 | 3 | lossless, lossy 2, uncompressed | (15, 6), 6 of 6 files | ✅ measured entry | crop (15, 6, 4256, 2832) | `nikon-d3s-4288x2844-12bit.nef` |
| NIKON D3S | 4288 × 2844 | 14 | 3 | lossless, lossy 2, uncompressed | (15, 6), 6 of 6 files | ✅ measured entry | crop (15, 6, 4256, 2832) | `nikon-d3s-4288x2844-14bit.nef` |
| NIKON D3X | 6080 × 4044 | 14 | 2 | lossless, uncompressed | unclear (6048×4032, score 0.50) | ❓ unmeasured | no masked edge lines found | `nikon-d3x-6080x4044-14bit.nef` |
| NIKON D4 | 4992 × 3292 | 12 | 3 | lossless, lossy 2, uncompressed | unclear (4928×3280, score 0.61) | ✅ measured entry | crop (8, 6, 4928, 3280) | `nikon-d4-4992x3292-12bit.nef` |
| NIKON D4 | 4992 × 3292 | 14 | 3 | lossless, lossy 2, uncompressed | unclear (4928×3280, score 0.61) | ✅ measured entry | crop (8, 6, 4928, 3280) | `nikon-d4-4992x3292-14bit.nef` |
| NIKON D40 | 3040 × 2014 | 12 | 1 | lossy 1 | (16, 9), 1 of 1 files | ✅ fine | only picture beyond the camera frame | `nikon-d40-3040x2014-12bit.nef` |
| NIKON D40X | 3904 × 2616 | 12 | 1 | lossy 1 | (14, 9), 1 of 1 files | ✅ measured entry | crop (14, 9, 3872, 2592); siblings: NIKON D3000 (1), NIKON D60 (1), NIKON D80 (1) | `nikon-d40x-3904x2616-12bit.nef` |
| NIKON D4S | 2464 × 1640 | 12 | 1 | HE / other | — | not decoded | Nikon compressed NEF without a linearization table (maker note 0x96) | `nikon-d4s-2464x1640-12bit.nef` |
| NIKON D4S | 4936 × 3288 | 12 | 3 | lossless, lossy 2, uncompressed | (4, 4), 3 of 3 files | ✅ measured entry | crop (4, 4, 4928, 3280) | `nikon-d4s-4936x3288-12bit.nef` |
| NIKON D4S | 4936 × 3288 | 14 | 3 | lossless, lossy 2, uncompressed | (4, 4), 3 of 3 files | ✅ measured entry | crop (4, 4, 4928, 3280) | `nikon-d4s-4936x3288-14bit.nef` |
| NIKON D50 | 3040 × 2014 | 12 | 1 | lossy 1 | unclear (3008×2000, score 0.74) | ❓ unmeasured | masked edge lines: right 1 | `nikon-d50-3040x2014-12bit.nef` |
| NIKON D5000 | 4352 × 2868 | 12 | 2 | lossy 2 | unclear (4288×2848, score 0.58) | ❓ unmeasured | no masked edge lines found | `nikon-d5000-4352x2868-12bit.nef` |
| NIKON D5100 | 4992 × 3280 | 14 | 3 | lossless, lossy 2, uncompressed | (8, 8), 3 of 3 files | ✅ measured entry | crop (8, 8, 4928, 3264) | `nikon-d5100-4992x3280-14bit.nef` |
| NIKON D5200 | 6036 × 4020 | 14 | 2 | lossy 2 | unclear (6000×4000, score 0.55) | ❓ unmeasured | no masked edge lines found | `nikon-d5200-6036x4020-14bit.nef` |
| NIKON D5300 | 6016 × 4016 | 12 | 1 | lossy 2 | (8, 8), 2 of 2 files | ✅ fine | only picture beyond the camera frame | `nikon-d5300-6016x4016-12bit.nef` |
| NIKON D5300 | 6016 × 4016 | 14 | 1 | lossy 2 | (8, 8), 2 of 2 files | ✅ fine | only picture beyond the camera frame | `nikon-d5300-6016x4016-14bit.nef` |
| NIKON D5500 | 6016 × 4016 | 12 | 1 | lossy 2 | (8, 8), 2 of 2 files | ✅ fine | only picture beyond the camera frame | `nikon-d5500-6016x4016-12bit.nef` |
| NIKON D5500 | 6016 × 4016 | 14 | 2 | lossy 2 | (8, 8), 2 of 2 files | ✅ fine | only picture beyond the camera frame | `nikon-d5500-6016x4016-14bit.nef` |
| NIKON D5600 | 6016 × 4016 | 12 | 2 | lossy 2 | (8, 8), 4 of 4 files | ✅ measured entry | crop (8, 8, 6000, 4000) | `nikon-d5600-6016x4016-12bit.nef` |
| NIKON D5600 | 6016 × 4016 | 14 | 2 | lossy 2 | (8, 8), 4 of 4 files | ✅ measured entry | crop (8, 8, 6000, 4000) | `nikon-d5600-6016x4016-14bit.nef` |
| NIKON D60 | 3904 × 2616 | 12 | 1 | lossy 1 | (14, 9), 1 of 1 files | ✅ measured entry | crop (14, 9, 3872, 2592); siblings: NIKON D3000 (1), NIKON D40X (1), NIKON D80 (1) | `nikon-d60-3904x2616-12bit.nef` |
| NIKON D600 | 6080 × 4028 | 12 | 2 | lossless, lossy 2 | (8, 6), 4 of 4 files | ✅ measured entry | crop (8, 6, 6016, 4016) | `nikon-d600-6080x4028-12bit.nef` |
| NIKON D600 | 6080 × 4028 | 14 | 2 | lossless, lossy 2 | (8, 6), 4 of 4 files | ✅ measured entry | crop (8, 6, 6016, 4016) | `nikon-d600-6080x4028-14bit.nef` |
| NIKON D610 | 6080 × 4028 | 12 | 2 | lossless, lossy 2 | (8, 6), 4 of 4 files | ✅ measured entry | crop (8, 6, 6016, 4016) | `nikon-d610-6080x4028-12bit.nef` |
| NIKON D610 | 6080 × 4028 | 14 | 2 | lossless, lossy 2 | (8, 6), 4 of 4 files | ✅ measured entry | crop (8, 6, 6016, 4016) | `nikon-d610-6080x4028-14bit.nef` |
| NIKON D70 | 3040 × 2014 | 12 | 1 | lossy 1 | (15, 7), 1 of 1 files | ✅ fine | only picture beyond the camera frame | `nikon-d70-3040x2014-12bit.nef` |
| NIKON D700 | 4288 × 2844 | 12 | 3 | lossless, lossy 2, uncompressed | (15, 6), 6 of 6 files | ✅ measured entry | crop (15, 6, 4256, 2832) | `nikon-d700-4288x2844-12bit.nef` |
| NIKON D700 | 4288 × 2844 | 14 | 3 | lossless, lossy 2, uncompressed | (15, 6), 6 of 6 files | ✅ measured entry | crop (15, 6, 4256, 2832) | `nikon-d700-4288x2844-14bit.nef` |
| NIKON D7000 | 4992 × 3280 | 12 | 2 | lossless, lossy 2 | (8, 8), 1 of 5 files | ✅ measured entry | crop (8, 8, 4928, 3264); siblings: COOLPIX A (1), NIKON D5100 (3) | `nikon-d7000-4992x3280-12bit.nef` |
| NIKON D7000 | 4992 × 3280 | 14 | 3 | lossless, lossy 2 | (8, 8), 1 of 5 files | ✅ measured entry | crop (8, 8, 4928, 3264); siblings: COOLPIX A (1), NIKON D5100 (3) | `nikon-d7000-4992x3280-14bit.nef` |
| NIKON D70s | 3040 × 2014 | 12 | 1 | lossy 1 | unclear (3008×2000, score 0.76) | ❓ unmeasured | no masked edge lines found | `nikon-d70s-3040x2014-12bit.nef` |
| NIKON D7100 | 6036 × 4020 | 12 | 2 | lossless, lossy 2 | unclear (6000×4000, score 0.23) | ❓ unmeasured | no masked edge lines found | `nikon-d7100-6036x4020-12bit.nef` |
| NIKON D7100 | 6036 × 4020 | 14 | 2 | lossless, lossy 2 | unclear (6000×4000, score 0.24) | ❓ unmeasured | no masked edge lines found | `nikon-d7100-6036x4020-14bit.nef` |
| NIKON D7200 | 6016 × 4016 | 12 | 2 | lossless, lossy 2 | unclear (6000×4000, score 0.33) | ❓ unmeasured | no masked edge lines found | `nikon-d7200-6016x4016-12bit.nef` |
| NIKON D7200 | 6016 × 4016 | 14 | 2 | lossless, lossy 2 | unclear (6000×4000, score 0.31) | ❓ unmeasured | no masked edge lines found | `nikon-d7200-6016x4016-14bit.nef` |
| NIKON D750 | 6032 × 4032 | 12 | 2 | lossless, lossy 2 | (8, 8), 2 of 4 files | ❓ check | dark lines in crop (optical black or a dark sample): left 8, right 8, top 8; pending: 1 more clean file(s) | `nikon-d750-6032x4032-12bit.nef` |
| NIKON D750 | 6032 × 4032 | 14 | 2 | lossless, lossy 2 | (8, 8), 2 of 4 files | ❓ check | dark lines in crop (optical black or a dark sample): left 4, right 8, top 8; pending: 1 more clean file(s) | `nikon-d750-6032x4032-14bit.nef` |
| NIKON D80 | 3904 × 2616 | 12 | 1 | lossy 1 | (14, 9), 1 of 1 files | ✅ measured entry | crop (14, 9, 3872, 2592); siblings: NIKON D3000 (1), NIKON D40X (1), NIKON D60 (1) | `nikon-d80-3904x2616-12bit.nef` |
| NIKON D80 | 3900 × 2611 | 16 | 1 | compressed | no full-size JPEG | ❓ unmeasured | no masked edge lines found | `nikon-d80-3900x2611-16bit.dng` |
| NIKON D800 | 7424 × 4924 | 12 | 3 | lossless, lossy 2, uncompressed | (8, 6), 6 of 6 files | ✅ measured entry | crop (8, 6, 7360, 4912) | `nikon-d800-7424x4924-12bit.nef` |
| NIKON D800 | 7424 × 4924 | 14 | 3 | lossless, lossy 2, uncompressed | (8, 6), 6 of 6 files | ✅ measured entry | crop (8, 6, 7360, 4912) | `nikon-d800-7424x4924-14bit.nef` |
| NIKON D800E | 7424 × 4924 | 14 | 2 | lossless, uncompressed | (8, 6), 2 of 2 files | ✅ measured entry | crop (8, 6, 7360, 4912); siblings: NIKON D800 (6) | `nikon-d800e-7424x4924-14bit.nef` |
| NIKON D810 | 3680 × 2456 | 12 | 1 | HE / other | — | not decoded | Nikon compressed NEF without a linearization table (maker note 0x96) | `nikon-d810-3680x2456-12bit.nef` |
| NIKON D810 | 7380 × 4928 | 12 | 3 | HE / other, lossless, lossy 2 | (12, 8), 3 of 3 files | ✅ measured entry | crop (12, 8, 7360, 4912) | `nikon-d810-7380x4928-12bit.nef` |
| NIKON D810 | 7380 × 4928 | 14 | 3 | lossless, lossy 2, uncompressed | (12, 8), 3 of 3 files | ✅ measured entry | crop (12, 8, 7360, 4912) | `nikon-d810-7380x4928-14bit.nef` |
| NIKON D90 | 4352 × 2868 | 12 | 1 | lossy 2 | (11, 10), 1 of 1 files | ✅ fine | only picture beyond the camera frame | `nikon-d90-4352x2868-12bit.nef` |
| NIKON Df | 4992 × 3292 | 12 | 3 | lossless, lossy 2, uncompressed | (8, 6), 6 of 6 files | ✅ measured entry | crop (8, 6, 4928, 3280) | `nikon-df-4992x3292-12bit.nef` |
| NIKON Df | 4992 × 3292 | 14 | 3 | lossless, lossy 2, uncompressed | (8, 6), 6 of 6 files | ✅ measured entry | crop (8, 6, 4928, 3280) | `nikon-df-4992x3292-14bit.nef` |
| COOLPIX P340 | 4032 × 3024 | 12 | — | — | — | no CC0 sample | only non-CC0 samples on raw.pixls.us | — |
| COOLPIX P6000 | 4240 × 3174 | 12 | — | — | — | no CC0 sample | only non-CC0 samples on raw.pixls.us | — |
| COOLPIX P7000 | 3664 × 2742 | 12 | — | — | — | no CC0 sample | only non-CC0 samples on raw.pixls.us | — |
| COOLPIX P7100 | 3664 × 2744 | 12 | — | — | — | no CC0 sample | only non-CC0 samples on raw.pixls.us | — |
| E5400 | 2608 × 1950 | 12 | — | — | — | no CC0 sample | only non-CC0 samples on raw.pixls.us | — |
| NIKON 1 S2 | 4608 × 3080 | 12 | — | — | — | no CC0 sample | only non-CC0 samples on raw.pixls.us | — |
| NIKON D1 | 2012 × 1324 | 12 | — | — | — | no CC0 sample | only non-CC0 samples on raw.pixls.us | — |

## Bodies that record `CropArea`

Read from the maker note of every sample (the first 1 MB of each file); the decoder crops to it.

| Model | Data | Bits | CropArea (x, y, width, height) |
|---|---|---:|---|
| NIKON D3400 | 6016 × 4016 | 12 | (8, 8, 6000, 4000) |
| NIKON D3500 | 6016 × 4016 | 12 | (8, 8, 6000, 4000) |
| NIKON D5 | 5584 × 3728 | 14 | (8, 8, 5568, 3712) |
| NIKON D500 | 5600 × 3728 | 12 | (16, 8, 5568, 3712) |
| NIKON D500 | 5600 × 3728 | 14 | (16, 8, 5568, 3712) |
| NIKON D500 | 4800 × 3192 | 12 | (15, 6, 4770, 3180) |
| NIKON D500 | 4200 × 2792 | 12 | (12, 4, 4176, 2784) |
| NIKON D6 | 5584 × 3728 | 14 | (8, 8, 5568, 3712) |
| NIKON D6 | 4786 × 3194 | 12 | (8, 7, 4770, 3180) |
| NIKON D6 | 4188 × 2796 | 12 | (6, 6, 4176, 2784) |
| NIKON D6 | 5584 × 3728 | 12 | (8, 8, 5568, 3712) |
| NIKON D7500 | 5600 × 3728 | 12 | (16, 8, 5568, 3712) |
| NIKON D7500 | 5600 × 3728 | 14 | (16, 8, 5568, 3712) |
| NIKON D780 | 6064 × 4040 | 12 | (8, 8, 6048, 4024) |
| NIKON D780 | 6064 × 4040 | 14 | (8, 8, 6048, 4024) |
| NIKON D850 | 6216 × 4136 | 12 | (12, 4, 6192, 4128) |
| NIKON D850 | 7104 × 4728 | 12 | (15, 5, 7074, 4716) |
| NIKON D850 | 8288 × 5520 | 12 | (16, 8, 8256, 5504) |
| NIKON D850 | 8288 × 5520 | 14 | (16, 8, 8256, 5504) |
| NIKON Z 30 | 5600 × 3728 | 12 | (16, 8, 5568, 3712) |
| NIKON Z 30 | 5600 × 3728 | 14 | (16, 8, 5568, 3712) |
| NIKON Z 5 | 6040 × 4032 | 14 | (12, 8, 6016, 4016) |
| NIKON Z 5 | 6040 × 4032 | 12 | (12, 8, 6016, 4016) |
| NIKON Z 50 | 5600 × 3728 | 14 | (16, 8, 5568, 3712) |
| NIKON Z 50 | 5600 × 3728 | 12 | (16, 8, 5568, 3712) |
| NIKON Z 6 | 6064 × 4040 | 14 | (8, 8, 6048, 4024) |
| NIKON Z 6 | 6064 × 4040 | 12 | (8, 8, 6048, 4024) |
| NIKON Z 6 | 5196 × 3462 | 12 | (10, 7, 5176, 3448) |
| NIKON Z 6 | 4548 × 3030 | 12 | (10, 6, 4528, 3018) |
| NIKON Z 6_2 | 6064 × 4040 | 14 | (8, 8, 6048, 4024) |
| NIKON Z 6_2 | 6064 × 4040 | 12 | (8, 8, 6048, 4024) |
| NIKON Z 7 | 8288 × 5520 | 14 | (16, 8, 8256, 5504) |
| NIKON Z 7 | 8288 × 5520 | 12 | (16, 8, 8256, 5504) |
| NIKON Z 7 | 7104 × 4730 | 12 | (15, 7, 7074, 4716) |
| NIKON Z 7 | 6216 × 4140 | 12 | (12, 6, 6192, 4128) |
| NIKON Z 7_2 | 8288 × 5520 | 14 | (16, 8, 8256, 5504) |
| NIKON Z 7_2 | 8288 × 5520 | 12 | (16, 8, 8256, 5504) |
| NIKON Z 8 | 8280 × 5520 | 14 | (12, 8, 8256, 5504) |
| NIKON Z 9 | 8280 × 5520 | 14 | (12, 8, 8256, 5504) |
| NIKON Z f | 6064 × 4040 | 14 | (8, 4, 6048, 4032) |
| NIKON Z fc | 5600 × 3728 | 14 | (16, 8, 5568, 3712) |
| NIKON Z fc | 5600 × 3728 | 12 | (16, 8, 5568, 3712) |
| NIKON Z50_2 | 5600 × 3728 | 14 | (16, 8, 5568, 3712) |
| NIKON Z50_2 | 3728 × 3728 | 14 | (8, 8, 3712, 3712) |
| NIKON Z50_2 | 5600 × 3144 | 14 | (16, 8, 5568, 3128) |
| NIKON Z5_2 | 4048 × 4040 | 14 | (8, 4, 4032, 4032) |
| NIKON Z5_2 | 6064 × 3416 | 14 | (8, 8, 6048, 3400) |
| NIKON Z5_2 | 4000 × 2672 | 14 | (8, 8, 3984, 2656) |
| NIKON Z5_2 | 6064 × 4040 | 14 | (8, 4, 6048, 4032) |
| NIKON Z6_3 | 4048 × 4040 | 14 | (8, 4, 4032, 4032) |
| NIKON Z6_3 | 6064 × 3416 | 14 | (8, 8, 6048, 3400) |
| NIKON Z6_3 | 4000 × 2672 | 14 | (8, 8, 3984, 2656) |
| NIKON Z6_3 | 6064 × 4040 | 14 | (8, 4, 6048, 4032) |

## Adding a body

1. Gather files of the body: several per data size, with detail across the frame (a blank sky, fog or a lens cap
   has nothing to align on), shot with in-camera distortion control off.
2. Run `cargo xtask raw-frame FILE…`. It prints, per model and data size, how many files align cleanly on the same
   frame and, once there is enough evidence, the `MeasuredBody` lines.
3. Evidence: at least 3 clean files that agree exactly, either of the model itself (12- and 14-bit pooled: both
   read out the same area), or at least one of its own plus *siblings*, other models with the same readout (data
   size and JPEG size) whose own files agree on the same frame. The same data size alone is not enough: the D200
   and D80 share 3904 × 2616 data but frame it at (16, 12) and (14, 9).
4. Add an entry only when the decoder's crop keeps lines without picture (*edge bands* and *beyond the camera frame*
   in the tool's output), paste it into `MEASURED_BODIES` with a comment naming the evidence, and update this file.
   `measured_bodies_are_consistent` checks every entry lies inside its data and is listed once.

## How this survey was made, and how to reproduce it

The evidence is committed next to this file, in [`nikon-bodies/`](nikon-bodies/):

- [`headers.json`](nikon-bodies/headers.json): every Nikon raw on raw.pixls.us (index of 2026-10-11, 291 files) with its
  URL, sha256, raw image size, depth and compression and the maker note's `CropArea`, `CropHiSpeed` and
  `NEFCompression`, read from the first 1 MB of each file.
- [`samples.json`](nikon-bodies/samples.json): the 138 CC0 files measured: one per model, data size and depth without
  `CropArea`, and every other CC0 sample of the groups that showed lines without picture (URL and sha256 each).
- [`frames.jsonl`](nikon-bodies/frames.jsonl): `cargo xtask raw-frame --json` on those files: frame, score, margin,
  edge bands and what the decoder's crop keeps beyond the frame.

To reproduce, or to re-measure after a decoder change:

```sh
cargo xtask corpus --download nikon   # 138 files, ~2.7 GB, into corpus/raw/nikon-inventory/ (sha256 checked)
cargo xtask raw-frame corpus/raw/nikon-inventory/*.* corpus/raw/nikon-inventory/extra/*
cargo xtask raw-frame --json corpus/raw/nikon-inventory/*.* corpus/raw/nikon-inventory/extra/* > docs/nikon-bodies/frames.jsonl
```

The first `raw-frame` prints each group's verdict and `MeasuredBody` entries; the counts in the tables above are its
output. Only CC0 files are listed; the D4's 14-bit entry comes from its contributor's own measurement. The statuses
above reflect the decoder with this file's entries.
