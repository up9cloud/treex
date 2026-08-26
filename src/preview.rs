//! Reading a file for display, under a size limit.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// How much of a file is worth sending to a viewer.
#[derive(Debug, Clone, Copy)]
pub struct PreviewOptions {
    pub max_bytes: u64,
}

pub const DEFAULT_MAX_BYTES: u64 = 1024 * 1024;

impl Default for PreviewOptions {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}

/// What a client already holds, so the server can skip resending it.
///
/// Size and mtime together: either changing means the bytes may have, and
/// neither is expensive to carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    pub size: u64,
    /// Milliseconds since the epoch, or 0 where the platform has no mtime.
    pub mtime: i64,
}

impl Stamp {
    pub fn of(meta: &std::fs::Metadata) -> Self {
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        Self {
            size: meta.len(),
            mtime,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum Preview {
    /// The caller's copy is still good.
    Unchanged,
    Ok {
        #[serde(flatten)]
        stamp: Stamp,
        content: String,
        /// True when the file was valid UTF-8 only after replacing bad bytes.
        lossy: bool,
    },
    TooLarge {
        size: u64,
        limit: u64,
    },
    Binary {
        size: u64,
    },
    Unreadable {
        reason: String,
    },
}

/// How far in to look for a NUL before calling a file text.
const SNIFF: usize = 8192;

/// Formats that are unmistakably not text but can be entirely printable for
/// their first few kilobytes. A minimal PDF has no NUL byte anywhere.
const BINARY_MAGIC: &[&[u8]] = &[
    b"%PDF-",
    b"PK\x03\x04",
    b"\x89PNG",
    b"\xff\xd8\xff",
    b"GIF8",
    b"\x7fELF",
    b"\x1f\x8b",
    b"OggS",
    b"RIFF",
    b"\xca\xfe\xba\xbe",
    b"BZh",
    b"\xfd7zXZ",
    b"MZ",
];

pub fn read(path: &Path, opts: &PreviewOptions, known: Option<Stamp>) -> Preview {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(err) => {
            return Preview::Unreadable {
                reason: err.to_string(),
            }
        }
    };

    if meta.is_dir() {
        return Preview::Unreadable {
            reason: "is a directory".into(),
        };
    }

    let stamp = Stamp::of(&meta);
    if known == Some(stamp) {
        return Preview::Unchanged;
    }

    let size = stamp.size;
    if size > opts.max_bytes {
        return Preview::TooLarge {
            size,
            limit: opts.max_bytes,
        };
    }

    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) => {
            return Preview::Unreadable {
                reason: err.to_string(),
            }
        }
    };

    // A NUL byte early on is the cheap, conventional test — it is what `grep`
    // and `git` use — but it misses formats that stay printable at the start.
    let nul = bytes.iter().take(SNIFF).any(|&b| b == 0);
    if nul || BINARY_MAGIC.iter().any(|m| bytes.starts_with(m)) {
        return Preview::Binary { size };
    }

    match String::from_utf8(bytes) {
        Ok(content) => Preview::Ok {
            stamp,
            content,
            lossy: false,
        },
        Err(err) => Preview::Ok {
            stamp,
            content: String::from_utf8_lossy(err.as_bytes()).into_owned(),
            lossy: true,
        },
    }
}

/// Parses `1m`, `512k`, `2MiB`, `4096`. Suffixes are powers of 1024.
pub fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let digits = s
        .trim_end_matches(|c: char| c.is_ascii_alphabetic())
        .trim_end();
    let suffix = s[digits.len()..].trim().to_ascii_lowercase();

    let value: u64 = digits
        .parse()
        .map_err(|_| format!("{s:?} is not a size: expected something like 1m or 512k"))?;

    let scale: u64 = match suffix.trim_end_matches("ib").trim_end_matches('b') {
        "" => 1,
        "k" => 1024,
        "m" => 1024 * 1024,
        "g" => 1024 * 1024 * 1024,
        other => return Err(format!("unknown size suffix {other:?}")),
    };

    let bytes = value
        .checked_mul(scale)
        .ok_or_else(|| format!("{s:?} overflows"))?;
    // A limit of zero would refuse every file, which is what --no-preview is
    // for and is never what someone typing a number meant.
    if bytes == 0 {
        return Err("a preview limit of zero refuses everything; use --no-preview".into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(max: u64) -> PreviewOptions {
        PreviewOptions { max_bytes: max }
    }

    #[test]
    fn reads_a_text_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "hello\nworld\n").unwrap();

        match read(&path, &opts(1024), None) {
            Preview::Ok {
                stamp,
                content,
                lossy,
            } => {
                assert_eq!(stamp.size, 12);
                assert_eq!(content, "hello\nworld\n");
                assert!(!lossy);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn refuses_a_file_over_the_limit_without_reading_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.bin");
        std::fs::write(&path, vec![b'x'; 4096]).unwrap();

        match read(&path, &opts(1024), None) {
            Preview::TooLarge { size, limit } => {
                assert_eq!(size, 4096);
                assert_eq!(limit, 1024);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_file_exactly_at_the_limit_is_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("edge");
        std::fs::write(&path, vec![b'x'; 1024]).unwrap();
        assert!(matches!(read(&path, &opts(1024), None), Preview::Ok { .. }));
    }

    #[test]
    fn printable_formats_are_still_binary() {
        let dir = tempfile::tempdir().unwrap();
        for (name, magic) in [
            (
                "a.pdf",
                &b"%PDF-1.4 and then some perfectly readable ascii"[..],
            ),
            ("a.zip", &b"PK\x03\x04rest"[..]),
        ] {
            let path = dir.path().join(name);
            std::fs::write(&path, magic).unwrap();
            assert!(
                matches!(read(&path, &opts(1024), None), Preview::Binary { .. }),
                "{name} was offered as text"
            );
        }
    }

    #[test]
    fn nul_bytes_mean_binary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.bin");
        std::fs::write(&path, [0x7f, b'E', b'L', b'F', 0, 0, 0]).unwrap();
        assert!(matches!(
            read(&path, &opts(1024), None),
            Preview::Binary { .. }
        ));
    }

    #[test]
    fn invalid_utf8_is_shown_lossily_rather_than_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("latin1.txt");
        std::fs::write(&path, [b'c', b'a', b'f', 0xe9]).unwrap();

        match read(&path, &opts(1024), None) {
            Preview::Ok { content, lossy, .. } => {
                assert!(lossy);
                assert!(content.starts_with("caf"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn directories_and_missing_files_report_why() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            read(dir.path(), &opts(1024), None),
            Preview::Unreadable { .. }
        ));
        assert!(matches!(
            read(&dir.path().join("nope"), &opts(1024), None),
            Preview::Unreadable { .. }
        ));
    }

    #[test]
    fn an_unchanged_file_is_not_sent_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "hello").unwrap();

        let Preview::Ok { stamp, .. } = read(&path, &opts(1024), None) else {
            panic!("first read must return contents");
        };
        assert!(matches!(
            read(&path, &opts(1024), Some(stamp)),
            Preview::Unchanged
        ));

        // Same length, different contents: mtime is what catches it.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&path, "world").unwrap();
        match read(&path, &opts(1024), Some(stamp)) {
            Preview::Ok { content, .. } => assert_eq!(content, "world"),
            other => panic!("a changed file was reported as {other:?}"),
        }
    }

    #[test]
    fn sizes_parse_with_and_without_suffixes() {
        assert_eq!(parse_size("4096"), Ok(4096));
        assert_eq!(parse_size("1m"), Ok(1024 * 1024));
        assert_eq!(parse_size("1M"), Ok(1024 * 1024));
        assert_eq!(parse_size("512k"), Ok(512 * 1024));
        assert_eq!(parse_size("2MiB"), Ok(2 * 1024 * 1024));
        assert_eq!(parse_size("3gb"), Ok(3 * 1024 * 1024 * 1024));
        assert!(parse_size("0").is_err(), "zero refuses every file");
        assert!(parse_size("").is_err());
        assert!(parse_size("1x").is_err());
        assert!(parse_size("abc").is_err());
    }
}
