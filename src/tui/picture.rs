//! Pictures in the terminal, by asking `chafa` for them.
//!
//! The same arrangement as `bat`: treex links no image decoder. `chafa` draws
//! the picture in half-block characters with a foreground and a background
//! color each, which is ordinary styled text as far as ratatui is concerned —
//! so it lives in the cell grid, scrolls and redraws with everything else, and
//! works inside tmux. The graphics protocols (sixel, kitty) would draw better
//! and do none of those.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::LazyLock;

use crate::highlight::Run;

static CHAFA: LazyLock<bool> = LazyLock::new(|| {
    Command::new("chafa")
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .map(|out| out.status.success() && out.stdout.starts_with(b"Chafa "))
        .unwrap_or(false)
});

pub fn available() -> bool {
    *CHAFA
}

/// The picture at `path`, fitted inside `width` × `height` cells with its
/// proportions kept. `None` when there is no `chafa` or it could not read it.
pub fn draw(path: &Path, width: u16, height: u16) -> Option<(String, Vec<Run>)> {
    if !available() || width == 0 || height == 0 {
        return None;
    }
    let out = Command::new("chafa")
        .args([
            "--format=symbols",
            // Only ▀ and ▄: every font has them, and a cell is then exactly
            // two pixels stacked, which is what the colors are chosen for.
            "--symbols=vhalf",
            "--colors=full",
            "--animate=off",
            // Fill the box asked for: for symbols chafa otherwise stops at
            // four times the picture's own size, and zoom sets its own limits.
            "--scale=max",
            // No queries or cursor moves: the output goes into cells, not to
            // the terminal.
            "--polite=on",
        ])
        .arg(format!("--size={width}x{height}"))
        .arg("--")
        .arg(path)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    crate::highlight::strip(&out.stdout)
}

/// The picture's own width and height in pixels, read off its header. What
/// zoom measures its limits against; `None` leaves them at the defaults.
pub fn dimensions(path: &Path) -> Option<(u32, u32)> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = std::fs::File::open(path).ok()?;
    let mut head = Vec::new();
    (&file)
        .take(crate::media::HEAD as u64)
        .read_to_end(&mut head)
        .ok()?;
    let bytes = |b: &[u8], i: usize, n: usize| b.get(i..i + n).map(<[u8]>::to_vec);
    let be16 = |b: &[u8], i: usize| bytes(b, i, 2).map(|v| u16::from_be_bytes([v[0], v[1]]) as u32);
    let le16 = |b: &[u8], i: usize| bytes(b, i, 2).map(|v| u16::from_le_bytes([v[0], v[1]]) as u32);
    let be32 =
        |b: &[u8], i: usize| bytes(b, i, 4).map(|v| u32::from_be_bytes([v[0], v[1], v[2], v[3]]));
    let le32 =
        |b: &[u8], i: usize| bytes(b, i, 4).map(|v| u32::from_le_bytes([v[0], v[1], v[2], v[3]]));
    let le24 =
        |b: &[u8], i: usize| bytes(b, i, 3).map(|v| u32::from_le_bytes([v[0], v[1], v[2], 0]));

    let size = match crate::media::sniff(&head)? {
        "image/png" => (be32(&head, 16)?, be32(&head, 20)?),
        "image/gif" => (le16(&head, 6)?, le16(&head, 8)?),
        "image/qoi" => (be32(&head, 4)?, be32(&head, 8)?),
        "image/bmp" => (le32(&head, 18)?, (le32(&head, 22)? as i32).unsigned_abs()),
        "image/webp" => match head.get(12..16)? {
            b"VP8 " => (le16(&head, 26)? & 0x3fff, le16(&head, 28)? & 0x3fff),
            b"VP8L" => {
                let bits = le32(&head, 21)?;
                ((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1)
            }
            b"VP8X" => (le24(&head, 24)? + 1, le24(&head, 27)? + 1),
            _ => return None,
        },
        "image/jpeg" => {
            // Segment by segment to the frame header, seeking past the rest:
            // an EXIF block ahead of it can be most of a megabyte.
            let mut at = 2u64;
            loop {
                let mut marker = [0u8; 9];
                file.seek(SeekFrom::Start(at)).ok()?;
                file.read_exact(&mut marker).ok()?;
                if marker[0] != 0xff {
                    return None;
                }
                let kind = marker[1];
                if matches!(kind, 0xc0..=0xcf) && !matches!(kind, 0xc4 | 0xc8 | 0xcc) {
                    break (be16(&marker, 7)?, be16(&marker, 5)?);
                }
                at += 2 + be16(&marker, 2)? as u64;
            }
        }
        // The first image's tags, wherever its directory is.
        "image/tiff" => {
            let little = head.starts_with(b"II");
            let u16_at = |b: &[u8], i| if little { le16(b, i) } else { be16(b, i) };
            let u32_at = |b: &[u8], i| if little { le32(b, i) } else { be32(b, i) };
            let mut directory = vec![0u8; 2 + 12 * 32];
            file.seek(SeekFrom::Start(u32_at(&head, 4)? as u64)).ok()?;
            let n = file.read(&mut directory).ok()?;
            directory.truncate(n);
            let (mut width, mut height) = (None, None);
            for entry in 0..(u16_at(&directory, 0)? as usize).min(32) {
                let at = 2 + entry * 12;
                // SHORT or LONG, either of which the spec allows here.
                let value = match u16_at(&directory, at + 2)? {
                    3 => u16_at(&directory, at + 8)?,
                    _ => u32_at(&directory, at + 8)?,
                };
                match u16_at(&directory, at)? {
                    256 => width = Some(value),
                    257 => height = Some(value),
                    _ => {}
                }
            }
            (width?, height?)
        }
        // HEIF and AVIF: the `ispe` property, wherever the boxes before it put
        // it. A tiled HEIC can list a tile's first; close enough for limits.
        "image/avif" | "image/heic" | "image/heif" => {
            let mut boxes = Vec::new();
            file.seek(SeekFrom::Start(0)).ok()?;
            file.take(64 * 1024).read_to_end(&mut boxes).ok()?;
            let i = boxes.windows(4).position(|w| w == b"ispe")?;
            (be32(&boxes, i + 8)?, be32(&boxes, i + 12)?)
        }
        _ => return None,
    };
    (size.0 > 0 && size.1 > 0).then_some(size)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Against the real thing, when the machine has one, as with `bat`.
    #[test]
    fn a_real_chafa_draws_a_picture_inside_the_box() {
        if !available() {
            return;
        }
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/samples/picture.png");
        let (text, runs) = draw(&path, 20, 6).expect("chafa should read a PNG");

        let lines: Vec<&str> = text.lines().collect();
        assert!(!lines.is_empty() && lines.len() <= 6, "{lines:?}");
        assert!(lines.iter().all(|l| l.chars().count() <= 20), "{lines:?}");
        assert!(runs.iter().any(|r| r.style.bg.is_some()));
    }

    #[test]
    fn dimensions_come_off_the_header() {
        let samples = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/samples");
        assert_eq!(dimensions(&samples.join("photo.jpg")), Some((160, 90)));
        assert!(dimensions(&samples.join("picture.png")).is_some());
        assert_eq!(dimensions(&samples.join("notes.md")), None);

        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, bytes: &[u8]| {
            let path = dir.path().join(name);
            let mut padded = bytes.to_vec();
            padded.resize(64, 0);
            std::fs::write(&path, padded).unwrap();
            path
        };
        let gif = write("a.gif", b"GIF89a\x40\x01\xf0\x00");
        assert_eq!(dimensions(&gif), Some((320, 240)));
        let mut vp8x = b"RIFF\0\0\0\0WEBPVP8X\0\0\0\0\0\0\0\0".to_vec();
        vp8x.extend([0x3f, 0x01, 0x00, 0xef, 0x00, 0x00]);
        assert_eq!(dimensions(&write("a.webp", &vp8x)), Some((320, 240)));
        let mut avif = b"\0\0\0\x1cftypavif".to_vec();
        avif.extend(b"\0\0\0\0\0\0\0\0\0\0\0\x14ispe\0\0\0\0\0\0\x01\x40\0\0\0\xf0");
        assert_eq!(dimensions(&write("a.avif", &avif)), Some((320, 240)));
    }

    #[test]
    fn what_chafa_cannot_read_is_none() {
        if !available() {
            return;
        }
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        assert!(draw(&path, 20, 6).is_none());
    }
}
