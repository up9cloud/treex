//! What a file is, from its first bytes.
//!
//! One table for every picture, video and sound treex will hand to a viewer,
//! keyed on content rather than name: a `.dat` that is really an MP4 plays,
//! and a `.png` that is really text is not offered as a picture. The answer is
//! a MIME type, because that is what the browser needs to be told, and its
//! top level — `image`, `video`, `audio` — is what decides which viewer.
//!
//! Recognizing a format is not the same as anyone being able to show it: a
//! browser cannot draw TIFF or play WMV, and `chafa` was built with whichever
//! loaders it was built with. The viewers try, and say so when they cannot.

/// How much of a file `sniff` wants to see. MPEG-TS needs its second packet,
/// at 188; Ogg names its codec a little way into the first page.
pub const HEAD: usize = 512;

/// The MIME type of a picture, video or sound starting with `head`, or `None`
/// for anything else — text, archives, executables.
pub fn sniff(head: &[u8]) -> Option<&'static str> {
    let at = |i: usize, magic: &[u8]| head.get(i..).is_some_and(|h| h.starts_with(magic));
    let has = |magic: &[u8]| head.windows(magic.len()).any(|w| w == magic);
    let le32 = |i: usize| {
        head.get(i..i + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };

    for (offset, magic, mime) in FIXED {
        if at(*offset, magic) {
            return Some(mime);
        }
    }

    if at(0, b"RIFF") {
        return match head.get(8..12)? {
            b"WEBP" => Some("image/webp"),
            b"AVI " => Some("video/x-msvideo"),
            b"WAVE" => Some("audio/wav"),
            _ => None,
        };
    }
    if at(0, b"FORM") && (at(8, b"AIFF") || at(8, b"AIFC")) {
        return Some("audio/aiff");
    }
    if at(4, b"ftyp") {
        return Some(iso_media(head));
    }
    if at(0, b"\x1a\x45\xdf\xa3") {
        return Some(if has(b"webm") {
            "video/webm"
        } else {
            "video/x-matroska"
        });
    }
    if at(0, b"OggS") {
        return Some(if has(b"\x80theora") {
            "video/ogg"
        } else {
            "audio/ogg"
        });
    }
    // Two letters is too little to go on alone: the header size has to be one
    // that BMP actually uses.
    if at(0, b"BM") && matches!(le32(14), Some(12 | 40 | 52 | 56 | 64 | 108 | 124)) {
        return Some("image/bmp");
    }
    if at(0, b"\0\0\x01\0") && head.get(4).is_some_and(|&n| n > 0) && head.get(5) == Some(&0) {
        return Some("image/x-icon");
    }
    if head.first() == Some(&0x47) && head.get(188) == Some(&0x47) {
        return Some("video/mp2t");
    }
    // An MPEG audio frame with no ID3 tag in front of it. Layer bits of zero
    // are AAC's ADTS header, which shares the sync word.
    if let [0xff, second, ..] = head {
        if second & 0xe0 == 0xe0 {
            return Some(if second & 0x06 == 0 {
                "audio/aac"
            } else {
                "audio/mpeg"
            });
        }
    }
    None
}

const FIXED: &[(usize, &[u8], &str)] = &[
    (0, b"\x89PNG\r\n\x1a\n", "image/png"),
    (0, b"\xff\xd8\xff", "image/jpeg"),
    (0, b"GIF87a", "image/gif"),
    (0, b"GIF89a", "image/gif"),
    (0, b"II*\0", "image/tiff"),
    (0, b"MM\0*", "image/tiff"),
    (0, b"qoif", "image/qoi"),
    (0, b"\xff\x0a", "image/jxl"),
    (0, b"\0\0\0\x0cJXL \r\n\x87\n", "image/jxl"),
    (0, b"FLV\x01", "video/x-flv"),
    // ASF, which is WMV and WMA alike; the header object's GUID.
    (
        0,
        b"\x30\x26\xb2\x75\x8e\x66\xcf\x11\xa6\xd9\x00\xaa\x00\x62\xce\x6c",
        "video/x-ms-asf",
    ),
    (0, b"\0\0\x01\xba", "video/mpeg"),
    (0, b"\0\0\x01\xb3", "video/mpeg"),
    (0, b"ID3", "audio/mpeg"),
    (0, b"fLaC", "audio/flac"),
    (0, b"#!AMR", "audio/amr"),
];

/// The ISO base media family — MP4, MOV, 3GP, M4A, HEIF and AVIF are all one
/// container, told apart by the brands in its `ftyp` box.
fn iso_media(head: &[u8]) -> &'static str {
    let size = head
        .get(0..4)
        .map_or(0, |b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize);
    let major = head.get(8..12).unwrap_or_default();
    // The compatible brands follow the minor version, to the end of the box.
    let compatible = head.get(16..size.min(head.len())).unwrap_or_default();
    let names = |brand: &[u8]| major == brand || compatible.chunks(4).any(|c| c == brand);

    match major {
        b"avif" | b"avis" => "image/avif",
        b"heic" | b"heix" | b"heim" | b"heis" | b"hevc" | b"hevx" => "image/heic",
        // The generic image brands say which codec only among the others.
        b"mif1" | b"msf1" if names(b"avif") => "image/avif",
        b"mif1" | b"msf1" if names(b"heic") => "image/heic",
        b"mif1" | b"msf1" => "image/heif",
        b"qt  " => "video/quicktime",
        b"M4A " | b"M4B " | b"M4P " | b"F4A " | b"F4B " => "audio/mp4",
        [b'3', b'g', b'2', _] => "video/3gpp2",
        [b'3', b'g', _, _] => "video/3gpp",
        _ => "video/mp4",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ftyp(major: &[u8], compatible: &[&[u8]]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend(((16 + 4 * compatible.len()) as u32).to_be_bytes());
        b.extend(b"ftyp");
        b.extend(major);
        b.extend([0, 0, 0, 0]);
        for c in compatible {
            b.extend(*c);
        }
        b
    }

    #[test]
    fn each_family_is_told_apart() {
        let mut ts = vec![0u8; 200];
        ts[0] = 0x47;
        ts[188] = 0x47;
        let mut bmp = b"BM".to_vec();
        bmp.resize(18, 0);
        bmp[14] = 40;
        let cases: &[(&[u8], &str)] = &[
            (b"\x89PNG\r\n\x1a\n....", "image/png"),
            (b"II*\0\x08\0\0\0", "image/tiff"),
            (b"RIFF\0\0\0\0AVI LIST", "video/x-msvideo"),
            (b"RIFF\0\0\0\0WAVEfmt ", "audio/wav"),
            (b"RIFF\0\0\0\0WEBPVP8 ", "image/webp"),
            (
                b"\x1a\x45\xdf\xa3\x01\0\0\0\x42\x82\x88matroska",
                "video/x-matroska",
            ),
            (b"\x1a\x45\xdf\xa3\x01\0\0\0\x42\x82\x84webm", "video/webm"),
            (
                b"\x30\x26\xb2\x75\x8e\x66\xcf\x11\xa6\xd9\x00\xaa\x00\x62\xce\x6c",
                "video/x-ms-asf",
            ),
            (b"ID3\x04\0", "audio/mpeg"),
            (b"\xff\xfb\x90\x64", "audio/mpeg"),
            (b"\xff\xf1\x50\x80", "audio/aac"),
            (b"OggS\0\x02....................\x01vorbis", "audio/ogg"),
            (b"OggS\0\x02....................\x80theora", "video/ogg"),
            (&ts, "video/mp2t"),
            (&bmp, "image/bmp"),
        ];
        for (head, want) in cases {
            assert_eq!(
                sniff(head),
                Some(*want),
                "{:?}",
                String::from_utf8_lossy(head)
            );
        }
    }

    #[test]
    fn the_iso_family_goes_by_its_brands() {
        let cases: &[(Vec<u8>, &str)] = &[
            (ftyp(b"isom", &[b"isom", b"avc1"]), "video/mp4"),
            (ftyp(b"mp42", &[]), "video/mp4"),
            (ftyp(b"qt  ", &[b"qt  "]), "video/quicktime"),
            (ftyp(b"3gp4", &[]), "video/3gpp"),
            (ftyp(b"3g2a", &[]), "video/3gpp2"),
            (ftyp(b"M4A ", &[b"M4A ", b"mp42"]), "audio/mp4"),
            (ftyp(b"heic", &[b"mif1", b"heic"]), "image/heic"),
            (ftyp(b"mif1", &[b"mif1", b"heic"]), "image/heic"),
            (ftyp(b"mif1", &[b"mif1", b"avif"]), "image/avif"),
            (ftyp(b"mif1", &[b"mif1"]), "image/heif"),
            (ftyp(b"avif", &[]), "image/avif"),
        ];
        for (head, want) in cases {
            assert_eq!(
                sniff(head),
                Some(*want),
                "{:?}",
                String::from_utf8_lossy(head)
            );
        }
    }

    #[test]
    fn text_and_other_binaries_are_not_media() {
        for head in [
            &b"BMW and other cars"[..],
            b"%PDF-1.4",
            b"PK\x03\x04",
            b"\x7fELF",
            b"RIFF\0\0\0\0ACON",
            b"<svg xmlns='http://www.w3.org/2000/svg'/>",
            b"",
        ] {
            assert_eq!(sniff(head), None, "{:?}", String::from_utf8_lossy(head));
        }
    }
}
