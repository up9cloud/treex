//! What git would ignore, asked of git.
//!
//! treex does not read `.gitignore` files. Ignore rules are not a glob list:
//! they nest, they negate, later rules override earlier ones, and the answer
//! also depends on `core.excludesFile`, `.git/info/exclude` and whether the
//! path is already tracked. `git check-ignore` knows all of that and is on
//! every machine that has a repository worth looking at.
//!
//! One call answers a whole directory. It runs with that directory as its
//! working directory, so git discovers the repository itself — a folder
//! holding a dozen unrelated checkouts works with no extra bookkeeping, and a
//! directory in no repository simply reports nothing.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::LazyLock;

/// The git to use, looked for once.
static GIT: LazyLock<Option<&'static str>> = LazyLock::new(|| {
    let ok = Command::new("git")
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .map(|out| out.status.success() && out.stdout.starts_with(b"git version"))
        .unwrap_or(false);
    ok.then_some("git")
});

/// Whether dimming is possible at all here, i.e. whether git was found.
pub fn available() -> bool {
    GIT.is_some()
}

/// Which of `names` — entries of `dir` — git would ignore.
///
/// Empty when there is no git, when `dir` is in no repository, or when git had
/// nothing to say. Names are passed and read back NUL-separated, so a file
/// called `a b#c` or `中文` is no different from any other.
pub fn ignored(dir: &Path, names: &[OsString]) -> HashSet<OsString> {
    let Some(git) = *GIT else {
        return HashSet::new();
    };
    if names.is_empty() {
        return HashSet::new();
    }
    ask(git, dir, names).unwrap_or_default()
}

fn ask(git: &str, dir: &Path, names: &[OsString]) -> Option<HashSet<OsString>> {
    let mut child = Command::new(git)
        // `check-ignore` consults the index, so a *tracked* file that happens
        // to match a pattern is not reported — which is the right answer for
        // something being drawn on screen. `--no-index` would undo that.
        .args(["check-ignore", "-z", "--stdin"])
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let mut stdin = child.stdin.take().expect("stdin was piped");
    let mut stdout = child.stdout.take().expect("stdout was piped");

    // Written and read at once: a directory may hold thousands of entries and
    // a pipe holds 64 KiB, so writing it all before reading would deadlock.
    let mut out = Vec::new();
    let read = std::thread::scope(|scope| {
        scope.spawn(move || {
            for name in names {
                if write_name(&mut stdin, name).is_err() {
                    return;
                }
            }
        });
        stdout.read_to_end(&mut out)
    });
    read.ok()?;

    // 0 = something is ignored, 1 = nothing is, anything else (128: not a
    // repository) means there is no answer rather than an empty one.
    match child.wait().ok()?.code() {
        Some(0) | Some(1) => Some(
            out.split(|&b| b == 0)
                .filter(|name| !name.is_empty())
                .map(from_bytes)
                .collect(),
        ),
        _ => None,
    }
}

#[cfg(unix)]
fn write_name(to: &mut impl Write, name: &OsStr) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;

    to.write_all(name.as_bytes())?;
    to.write_all(&[0])
}

/// Windows filenames are UTF-16 and git speaks UTF-8 here. A name that does
/// not survive the trip is one git could not have matched anyway.
#[cfg(not(unix))]
fn write_name(to: &mut impl Write, name: &OsStr) -> std::io::Result<()> {
    let Some(name) = name.to_str() else {
        return Ok(());
    };
    to.write_all(name.as_bytes())?;
    to.write_all(&[0])
}

#[cfg(unix)]
fn from_bytes(name: &[u8]) -> OsString {
    use std::os::unix::ffi::OsStringExt;

    OsString::from_vec(name.to_vec())
}

#[cfg(not(unix))]
fn from_bytes(name: &[u8]) -> OsString {
    OsString::from(String::from_utf8_lossy(name).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A repository with the awkward parts: a nested rule, a negation that
    /// loses to a later rule, an ignored directory, and a tracked file that
    /// matches a pattern.
    fn repo() -> Option<tempfile::TempDir> {
        if !available() {
            return None;
        }
        let dir = tempfile::tempdir().unwrap();
        let at = dir.path();
        let git = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(at)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success()
        };
        assert!(git(&["init", "-q", "."]));

        std::fs::write(at.join(".gitignore"), "build/\n*.tmp\n").unwrap();
        std::fs::create_dir_all(at.join("src")).unwrap();
        std::fs::create_dir_all(at.join("build")).unwrap();
        std::fs::write(at.join("keep.rs"), "").unwrap();
        std::fs::write(at.join("junk.tmp"), "").unwrap();
        std::fs::write(at.join("build/thing"), "").unwrap();
        // Later rules win, so the negation before the pattern does not save it.
        std::fs::write(at.join("src/.gitignore"), "!spared.log\n*.log\n").unwrap();
        std::fs::write(at.join("src/noisy.log"), "").unwrap();
        std::fs::write(at.join("src/spared.log"), "").unwrap();
        std::fs::write(at.join("src/main.rs"), "").unwrap();
        // Tracked despite matching a pattern, which is the case `-f` exists
        // for and the one that separates "matches a rule" from "is ignored".
        std::fs::write(at.join("tracked.tmp"), "").unwrap();
        assert!(git(&["add", "-f", "tracked.tmp"]));

        Some(dir)
    }

    fn names(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    fn has(set: &HashSet<OsString>, name: &str) -> bool {
        set.contains(&OsString::from(name))
    }

    #[test]
    fn git_answers_for_the_directory_it_is_asked_about() {
        let Some(dir) = repo() else { return };

        let top = ignored(
            dir.path(),
            &names(&["build", "keep.rs", "junk.tmp", "src", ".gitignore"]),
        );
        assert!(has(&top, "build"), "an ignored directory");
        assert!(has(&top, "junk.tmp"), "a pattern match");
        assert!(!has(&top, "keep.rs"));
        assert!(!has(&top, "src"));
        assert!(!has(&top, ".gitignore"));

        // The nested rule, and the negation that loses to the rule after it.
        let nested = ignored(
            &dir.path().join("src"),
            &names(&["noisy.log", "spared.log", "main.rs"]),
        );
        assert!(has(&nested, "noisy.log"));
        assert!(
            has(&nested, "spared.log"),
            "a later rule overrides an earlier negation — this is why treex \
             does not read these files itself"
        );
        assert!(!has(&nested, "main.rs"));
    }

    #[test]
    fn a_tracked_file_is_not_ignored_however_it_is_spelled() {
        let Some(dir) = repo() else { return };
        let set = ignored(dir.path(), &names(&["tracked.tmp", "junk.tmp"]));
        assert!(has(&set, "junk.tmp"));
        assert!(
            !has(&set, "tracked.tmp"),
            "git does not ignore what it is already tracking, and neither should the tree"
        );
    }

    #[test]
    fn awkward_names_survive_the_round_trip() {
        let Some(dir) = repo() else { return };
        let at = dir.path();
        for name in ["a b.tmp", "hash#name.tmp", "中文檔名.tmp"] {
            std::fs::write(at.join(name), "").unwrap();
        }
        let set = ignored(at, &names(&["a b.tmp", "hash#name.tmp", "中文檔名.tmp"]));
        assert!(has(&set, "a b.tmp"));
        assert!(has(&set, "hash#name.tmp"));
        assert!(has(&set, "中文檔名.tmp"), "{set:?}");
    }

    #[test]
    fn a_directory_in_no_repository_ignores_nothing() {
        if !available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.tmp"), "").unwrap();
        assert!(ignored(dir.path(), &names(&["a.tmp"])).is_empty());
    }

    #[test]
    fn asking_about_nothing_costs_nothing() {
        assert!(ignored(Path::new("/"), &[]).is_empty());
    }
}
