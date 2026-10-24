//! GIF (first frame) and BMP via the `image` crate's pure-Rust decoders.

use crate::convert::{Buf, Meta, Model, Raw, check_size, finish};
use crate::{DecodeOptions, Decoded, Error, Format, Result};
use image::{DynamicImage, ImageDecoder};

/// Canvases up to this many pixels are still decoded by [`header`] (at most 16 MB), so a truncated
/// small file is refused at import as before.
const HEADER_DECODE_MAX_PIXELS: u64 = 4_000_000;

/// Stored size for the import probe (issue #2553). The probe used to decode these in full, and a
/// few-dozen-byte file declaring a gigapixel canvas allocated gigabytes. The size now comes from the
/// header and must be one the file's bytes can fill; only a canvas small enough to be cheap is
/// decoded (to catch truncation). Orientation is always 1: neither format carries EXIF here.
pub(crate) fn header(bytes: &[u8], format: Format) -> Result<(u32, u32, u16)> {
    let err = |e: image::ImageError| Error::Malformed(format, e.to_string());
    let cursor = std::io::Cursor::new(bytes);
    let (w, h) = match format {
        Format::Gif => image::codecs::gif::GifDecoder::new(cursor).map_err(err)?.dimensions(),
        Format::Bmp => image::codecs::bmp::BmpDecoder::new(cursor).map_err(err)?.dimensions(),
        _ => return Err(Error::Unsupported(format, "not handled by this decoder")),
    };
    if w == 0 || h == 0 {
        return Err(Error::Malformed(format, "zero dimension".into()));
    }
    check_fillable(format, bytes, w, h)?;
    if u64::from(w).saturating_mul(u64::from(h)) <= HEADER_DECODE_MAX_PIXELS {
        let d = decode(bytes, format, &DecodeOptions::fit(64, 64))?;
        return Ok((d.source_width, d.source_height, d.orientation));
    }
    Ok((w, h, 1))
}

/// The decoder's allocation limit: the RGBA buffer of the largest image `opts` allows. It bounds the
/// decoders' own frame buffers only: `DynamicImage::from_decoder` allocates the canvas without
/// consulting it, so the canvas is bounded by `check_size` and [`check_fillable`] beforehand (a
/// mostly blank one by [`SPARSE_MAX_PIXELS`]).
fn limits(opts: &DecodeOptions) -> image::Limits {
    let mut l = image::Limits::default();
    l.max_alloc = Some(opts.max_pixels.saturating_mul(4));
    l
}

/// The largest canvas allowed that the file's pixel data leaves mostly blank: a GIF screen larger
/// than its first frame, or an RLE BMP whose runs can't cover it. Its decode still allocates the
/// whole canvas (16 MB at this size), so it is held well below `max_pixels`.
const SPARSE_MAX_PIXELS: u64 = 4_000_000;

/// Refuses a canvas larger than the file's bytes can describe, before the decoder allocates it
/// (issue #2553). Uncompressed BMP rows are stored in full at 1 bit per pixel or more, so the file
/// holds at least one byte per 8 pixels. A GIF frame's pixels come from its LZW data alone; a code
/// expands to at most 4096 pixels and takes 12 bits once the table holds that many (fewer bits only
/// while its strings are shorter), so at most 2731 pixels per byte. That bounds the first frame's
/// own size; the screen around it may be larger, and is blank there.
///
/// An RLE BMP has no such bound, since an end-of-bitmap code or a delta leaves the rest blank. Its
/// runs cover at most 255 pixels per 2 bytes, so a canvas beyond 128 pixels per byte is mostly
/// blank. Mostly blank canvases are limited to [`SPARSE_MAX_PIXELS`].
fn check_fillable(format: Format, bytes: &[u8], w: u32, h: u32) -> Result<()> {
    let area = |w: u32, h: u32| u64::from(w).saturating_mul(u64::from(h));
    let unfillable = |what: &str, w: u32, h: u32, len: usize| {
        Error::Malformed(format, format!("a {w} × {h} {what} is far larger than its {len} bytes of pixel data can hold"))
    };
    let canvas = area(w, h);
    // the pixels the file's data can cover
    let covered = match format {
        Format::Gif => {
            // without a frame there is nothing to decode, and nothing to bound the canvas by
            let Some((fw, fh, len)) = gif_first_frame(bytes) else {
                return Err(Error::Malformed(format, "no readable first frame".into()));
            };
            if area(fw, fh) > (len as u64).saturating_mul(2_731) {
                return Err(unfillable("frame", fw, fh, len));
            }
            area(fw, fh)
        }
        Format::Bmp if bmp_is_rle(bytes) => (bytes.len() as u64).saturating_mul(128),
        _ => {
            if canvas > (bytes.len() as u64).saturating_mul(8) {
                return Err(unfillable("canvas", w, h, bytes.len()));
            }
            canvas
        }
    };
    if canvas > covered && canvas > SPARSE_MAX_PIXELS {
        return Err(Error::Malformed(
            format,
            format!(
                "a {w} × {h} canvas is mostly blank (its pixel data covers at most {covered} pixels), and such pictures are limited to {SPARSE_MAX_PIXELS} pixels"
            ),
        ));
    }
    Ok(())
}

/// Whether a BMP's pixels are run-length encoded (RLE8 or RLE4, in a header of 40 bytes or more).
fn bmp_is_rle(bytes: &[u8]) -> bool {
    let u32_at = |i: usize| bytes.get(i..i + 4).and_then(|b| b.try_into().ok()).map(u32::from_le_bytes);
    u32_at(14).is_some_and(|size| size >= 40) && matches!(u32_at(30), Some(1 | 2))
}

/// A GIF's first frame: its width and height, and the bytes of LZW data present (its palettes,
/// comments and other extensions describe no pixels). `None` when the blocks before the frame
/// can't be walked.
fn gif_first_frame(bytes: &[u8]) -> Option<(u32, u32, usize)> {
    let u16_at = |i: usize| bytes.get(i..i.checked_add(2)?).and_then(|b| b.try_into().ok()).map(u16::from_le_bytes);
    let table = |flags: u8| if flags & 0x80 != 0 { 3usize << ((flags & 7) + 1) } else { 0 };
    let mut pos = 13 + table(*bytes.get(10)?);
    loop {
        match *bytes.get(pos)? {
            // extension: introducer, label, sub-blocks
            0x21 => pos = sub_blocks(bytes, pos.saturating_add(2)).1,
            // image descriptor (separator, left, top, width, height, flags), local colour table,
            // LZW minimum code size, data sub-blocks
            0x2C => {
                let (fw, fh) = (u16_at(pos.saturating_add(5))?, u16_at(pos.saturating_add(7))?);
                let start = pos.saturating_add(10).saturating_add(table(*bytes.get(pos.saturating_add(9))?)).saturating_add(1);
                return Some((u32::from(fw), u32::from(fh), sub_blocks(bytes, start).0));
            }
            _ => return None,
        }
    }
}

/// The GIF sub-blocks starting at `pos`: the data bytes present, and the position after them.
fn sub_blocks(bytes: &[u8], mut pos: usize) -> (usize, usize) {
    let mut data = 0usize;
    while let Some(&n) = bytes.get(pos) {
        let start = pos.saturating_add(1);
        if n == 0 {
            return (data, start);
        }
        let end = start.saturating_add(usize::from(n));
        data = data.saturating_add(end.min(bytes.len()).saturating_sub(start));
        pos = end;
    }
    (data, pos)
}

pub(crate) fn decode(bytes: &[u8], format: Format, opts: &DecodeOptions) -> Result<Decoded> {
    if format == Format::Gif {
        gated_first_frame(bytes, opts)?;
    }
    let err = |e: image::ImageError| Error::Malformed(format, e.to_string());
    let cursor = std::io::Cursor::new(bytes);
    let (img, icc) = match format {
        Format::Gif => {
            let mut d = image::codecs::gif::GifDecoder::new(cursor).map_err(err)?;
            let (w, h) = d.dimensions();
            check_size(format, w as u64, h as u64, opts)?;
            check_fillable(format, bytes, w, h)?;
            d.set_limits(limits(opts)).map_err(err)?;
            let icc = d.icc_profile().ok().flatten();
            (DynamicImage::from_decoder(d).map_err(err)?, icc)
        }
        Format::Bmp => {
            let mut d = image::codecs::bmp::BmpDecoder::new(cursor).map_err(err)?;
            let (w, h) = d.dimensions();
            check_size(format, w as u64, h as u64, opts)?;
            check_fillable(format, bytes, w, h)?;
            d.set_limits(limits(opts)).map_err(err)?;
            let icc = d.icc_profile().ok().flatten();
            (DynamicImage::from_decoder(d).map_err(err)?, icc)
        }
        _ => return Err(Error::Unsupported(format, "not handled by this decoder")),
    };
    let (w, h) = (img.width(), img.height());
    let alpha = img.color().has_alpha();
    let gray = !img.color().has_color();
    let (model, alpha, buf) = match (gray, alpha) {
        (true, false) => (Model::Gray, false, img.into_luma8().into_raw()),
        (true, true) => (Model::Gray, true, img.into_luma_alpha8().into_raw()),
        (false, false) => (Model::Rgb, false, img.into_rgb8().into_raw()),
        (false, true) => (Model::Rgb, true, img.into_rgba8().into_raw()),
    };
    let raw = Raw { width: w as usize, height: h as usize, model, alpha, premultiplied: false, buf: Buf::U8(buf), bit_depth: 8 };
    finish(format, raw, Meta { icc, ..Default::default() }, (w, h), opts)
}

/// Bounds a GIF's first image descriptor: the decoder composites frames into the logical screen
/// (bounded by [`check_size`] on the canvas) but allocates its own frame buffers at the frame's
/// declared size, which a small file can inflate far past the canvas. The canvas check cannot
/// see that size, so it is read from the block headers here.
fn gated_first_frame(bytes: &[u8], opts: &DecodeOptions) -> Result<()> {
    // The constants of the header grammar: signature, logical screen descriptor (with its global
    // colour table), extension blocks (0x21, skipped by their sub-block chains) and image
    // descriptors (0x2C).
    const SCREEN: usize = 13;
    if bytes.len() < SCREEN {
        return Ok(()); // too broken for a frame; the decoder reports the damage itself
    }
    let gct_entries = 1usize << (u32::from(bytes[10] & 0x07) + 1); // at most 256
    let gct_bytes = if (bytes[10] & 0x80) != 0 { gct_entries * 3 } else { 0 };
    let mut p = SCREEN.saturating_add(gct_bytes);
    while let Some(&kind) = bytes.get(p) {
        match kind {
            0x2C => {
                // Image descriptor: two LE coordinates then two LE edges, after the separator.
                let end = p.checked_add(10).filter(|&e| e <= bytes.len());
                let Some(d) = end.and_then(|e| bytes.get(p + 1..e)) else {
                    break; // cut short mid-descriptor: the decoder reports that
                };
                let (w, h) = (u16::from_le_bytes([d[4], d[5]]) as u64, u16::from_le_bytes([d[6], d[7]]) as u64);
                return check_size(Format::Gif, w, h, opts);
            }
            0x21 => {
                // Extension: a label byte, then <len,data> sub-blocks ended by a zero length.
                p = p.saturating_add(2);
                loop {
                    let Some(&len) = bytes.get(p) else {
                        return Ok(()); // ran past the data: decode reports the truncation
                    };
                    p = p.saturating_add(1);
                    if len == 0 {
                        break;
                    }
                    p = p.saturating_add(len as usize);
                }
            }
            _ => p = p.saturating_add(1),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gif(blocks: &[u8]) -> Vec<u8> {
        let mut b = b"GIF89a".to_vec();
        // logical screen: 2×2, no global colour table, background 0, aspect 0
        b.extend_from_slice(&[2, 0, 2, 0, 0x00, 0, 0]);
        b.extend_from_slice(blocks);
        b
    }

    /// `image`'s GIF decoder allocates its frame buffer at the image descriptor's declared
    /// size, which the canvas bound ([`crate::decode`]'s `max_pixels` on the logical screen)
    /// does not cover. A 20-byte file used to buy a 4 GB allocation before the gate.
    #[test]
    fn a_first_frame_over_the_bound_is_refused_without_allocating_it() {
        let bytes = gif(&[0x2C, 0, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0xFF, 0, 0x00]);
        match crate::decode(&bytes, DecodeOptions::default()) {
            Err(Error::TooLarge(w, h)) => assert_eq!((w, h), (65535, 65535)),
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// A frame within the bound still reaches the decoder (which reports the truncated data);
    /// the gate may only refuse on size.
    #[test]
    fn a_first_frame_within_the_bound_is_not_refused() {
        let bytes = gif(&[0x2C, 0, 0, 0, 0, 1, 0, 1, 0, 0, 0x00]);
        let r = crate::decode(&bytes, DecodeOptions::default());
        assert!(matches!(r, Err(Error::Malformed(..)) | Err(Error::TooLarge(..))), "{r:?}");
    }

    /// A GIF whose blocks can't be walked to a first frame (here a comment that runs to the end of the
    /// file) is refused, instead of the whole file being counted as pixel data for its 400 MP screen.
    /// The `image` decoder also stops at such a file when it opens it, but the canvas bound doesn't
    /// rely on the two walks agreeing.
    #[test]
    fn a_gif_with_no_readable_first_frame_is_refused() {
        let mut b = b"GIF89a".to_vec();
        b.extend(20_000u16.to_le_bytes());
        b.extend(20_000u16.to_le_bytes());
        b.extend([0, 0, 0, 0x21, 0xFE]);
        for _ in 0..600 {
            b.push(255);
            b.extend([b'x'; 255]);
        }
        let r = check_fillable(Format::Gif, &b, 20_000, 20_000);
        assert!(matches!(&r, Err(Error::Malformed(_, m)) if m.contains("no readable first frame")), "{r:?}");
    }
}
