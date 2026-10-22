//! EXIF and XMP image resources in Photoshop PSD and PSB files.

use crate::container::Embedded;

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, length: usize) -> Option<&'a [u8]> {
        let end = self.offset.checked_add(length)?;
        let bytes = self.bytes.get(self.offset..end)?;
        self.offset = end;
        Some(bytes)
    }

    fn u16(&mut self) -> Option<u16> {
        let bytes = self.take(2)?;
        Some(u16::from_be_bytes([*bytes.first()?, *bytes.get(1)?]))
    }

    fn u32(&mut self) -> Option<u32> {
        let bytes = self.take(4)?;
        Some(u32::from_be_bytes([*bytes.first()?, *bytes.get(1)?, *bytes.get(2)?, *bytes.get(3)?]))
    }
}

pub(crate) fn embedded(bytes: &[u8]) -> Embedded {
    parse(bytes).unwrap_or_default()
}

pub(crate) fn parse(bytes: &[u8]) -> Option<Embedded> {
    let mut reader = Reader { bytes, offset: 0 };
    if reader.take(4)? != b"8BPS" || !matches!(reader.u16()?, 1 | 2) {
        return None;
    }
    // The fixed header after the version is reserved bytes, channels, dimensions, depth and colour mode.
    reader.take(6 + 2 + 4 + 4 + 2 + 2)?;
    let colour_mode_length = usize::try_from(reader.u32()?).ok()?;
    reader.take(colour_mode_length)?;
    let resource_length = usize::try_from(reader.u32()?).ok()?;
    let resources = reader.take(resource_length)?;
    parse_resources(resources)
}

fn parse_resources(bytes: &[u8]) -> Option<Embedded> {
    let mut reader = Reader { bytes, offset: 0 };
    let (mut exif, mut xmp) = (None, None);
    while reader.offset < bytes.len() {
        if reader.take(4)? != b"8BIM" {
            return None;
        }
        let id = reader.u16()?;
        let name_length = usize::from(*reader.take(1)?.first()?);
        let pascal_length = 1usize.checked_add(name_length)?.checked_add(1)? & !1;
        reader.take(pascal_length.checked_sub(1)?)?;
        let data_length = usize::try_from(reader.u32()?).ok()?;
        let data = reader.take(data_length)?;
        if !data_length.is_multiple_of(2) {
            reader.take(1)?;
        }
        match id {
            1058 => exif = Some(data),
            1060 => xmp = Some(data),
            _ => {}
        }
    }

    Some(Embedded {
        exif: exif.map(crate::strip_exif_header).map(<[u8]>::to_vec),
        xmp: xmp.map(|bytes| String::from_utf8_lossy(bytes).trim_end_matches('\0').to_string()),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::embedded;
    use crate::container::Embedded;
    use crate::{DateTime, Gps, Metadata};

    fn resource(id: u16, name: &[u8], data: &[u8]) -> Vec<u8> {
        let mut out = b"8BIM".to_vec();
        out.extend_from_slice(&id.to_be_bytes());
        out.push(u8::try_from(name.len()).unwrap());
        out.extend_from_slice(name);
        if !(name.len() + 1).is_multiple_of(2) {
            out.push(0);
        }
        out.extend_from_slice(&u32::try_from(data.len()).unwrap().to_be_bytes());
        out.extend_from_slice(data);
        if !data.len().is_multiple_of(2) {
            out.push(0);
        }
        out
    }

    fn image(version: u16, colour_data: &[u8], resources: &[u8]) -> Vec<u8> {
        let mut out = b"8BPS".to_vec();
        out.extend_from_slice(&version.to_be_bytes());
        out.extend_from_slice(&[0; 6]);
        out.extend_from_slice(&3u16.to_be_bytes()); // channels
        out.extend_from_slice(&1u32.to_be_bytes()); // height
        out.extend_from_slice(&1u32.to_be_bytes()); // width
        out.extend_from_slice(&8u16.to_be_bytes()); // depth
        out.extend_from_slice(&3u16.to_be_bytes()); // RGB
        out.extend_from_slice(&u32::try_from(colour_data.len()).unwrap().to_be_bytes());
        out.extend_from_slice(colour_data);
        out.extend_from_slice(&u32::try_from(resources.len()).unwrap().to_be_bytes());
        out.extend_from_slice(resources);
        out
    }

    fn dated_metadata() -> Metadata {
        Metadata {
            capture_time: DateTime::parse_exif("2023:05:14 14:40:39"),
            gps: Some(Gps { latitude: 45.0, longitude: 7.0, altitude: None }),
            ..Default::default()
        }
    }

    fn dated_image(version: u16) -> Vec<u8> {
        let exif = crate::write_exif(&dated_metadata());
        image(version, &[], &resource(1058, b"", &exif))
    }

    #[test]
    fn psd_capture_time_is_extracted() {
        let expected = dated_metadata().capture_time;
        let actual = crate::extract(&dated_image(1));
        assert_eq!(actual.capture_time, expected);
        assert_eq!(actual.gps, dated_metadata().gps);
    }

    #[test]
    fn psb_capture_time_is_extracted() {
        let expected = dated_metadata().capture_time;
        let actual = crate::extract(&dated_image(2));
        assert_eq!(actual.capture_time, expected);
        assert_eq!(actual.gps, dated_metadata().gps);
    }

    #[test]
    fn psd_extracts_xmp_without_exif() {
        let metadata = Metadata {
            capture_time: DateTime::parse_iso("2024-02-03T04:05:06Z"),
            gps: Some(Gps { latitude: -33.9, longitude: 151.2, altitude: Some(25.0) }),
            title: Some("Embedded XMP".into()),
            ..Default::default()
        };
        let xmp = crate::write_xmp(&metadata, None);
        let file = image(1, &[], &resource(1060, b"", xmp.as_bytes()));
        let actual = crate::extract(&file);
        assert_eq!(actual.capture_time, metadata.capture_time);
        assert_eq!(actual.gps, metadata.gps);
        assert_eq!(actual.title, metadata.title);
    }

    #[test]
    fn psd_normalizes_the_optional_exif_resource_prefix() {
        let mut exif = b"Exif\0\0".to_vec();
        exif.extend(crate::write_exif(&dated_metadata()));
        let file = image(1, &[], &resource(1058, b"", &exif));
        assert_eq!(crate::extract(&file).capture_time, dated_metadata().capture_time);
    }

    #[test]
    fn try_extract_rejects_structurally_malformed_psd_exif() {
        for version in [1, 2] {
            let file = image(version, &[], &resource(1058, b"", b"not a TIFF stream"));
            assert!(matches!(crate::try_extract(&file), Err(crate::ExtractError::MalformedPsd)));
        }
    }

    #[test]
    fn try_extract_accepts_unknown_exif_tags_in_psd() {
        let mut ifd0 = lightcraft_tiff::IfdBuilder::new();
        ifd0.set(0xc001, lightcraft_tiff::Value::Ascii("vendor metadata".into()));
        let exif = lightcraft_tiff::TiffWriter::default().write(&[ifd0]).unwrap();
        let file = image(1, &[], &resource(1058, b"", &exif));
        assert!(crate::try_extract(&file).is_ok());
    }

    #[test]
    fn psd_skips_unknown_resources_with_pascal_name_and_payload_padding() {
        let exif = crate::write_exif(&dated_metadata());
        let mut resources = resource(4000, b"xy", &[1, 2, 3]);
        resources.extend(resource(1058, b"", &exif));
        let file = image(1, &[8, 7, 6], &resources);
        assert_eq!(crate::extract(&file).capture_time, dated_metadata().capture_time);
    }

    #[test]
    fn psd_truncation_and_hostile_lengths_return_no_embedded_metadata() {
        let file = dated_image(1);
        for end in 0..file.len() {
            assert_eq!(embedded(&file[..end]), Embedded::default(), "truncated at {end}");
        }

        let mut hostile_colour_length = file.clone();
        hostile_colour_length[26..30].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(embedded(&hostile_colour_length), Embedded::default());

        let mut hostile_resource_length = file.clone();
        hostile_resource_length[30..34].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(embedded(&hostile_resource_length), Embedded::default());

        let mut hostile_payload_length = file;
        hostile_payload_length[42..46].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(embedded(&hostile_payload_length), Embedded::default());
    }

    #[test]
    fn psd_rejects_unknown_versions_and_incomplete_resource_records() {
        let file = dated_image(1);
        let mut unknown_version = file.clone();
        unknown_version[4..6].copy_from_slice(&3u16.to_be_bytes());
        assert_eq!(embedded(&unknown_version), Embedded::default());

        let mut truncated_record = file;
        truncated_record.pop();
        assert_eq!(embedded(&truncated_record), Embedded::default());
    }
}
