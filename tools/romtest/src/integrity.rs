//! The committed integrity list of `test-roms/`
//! (`acceptance/test-roms.sha256`, ADR 0007 amendments).
//!
//! Without it, deleting a failing ROM makes it stop matching its glob, and
//! deleting a reference PNG turns a failure into `no-reference`: either way
//! the gate would pass. Before anything runs, every listed file must exist
//! with the listed SHA-256, the list must belong to the manifest's release,
//! and every ROM a manifest glob matches must be listed. Checks may only
//! load listed files, and every ROM and reference PNG is hashed again
//! from the bytes actually loaded ([`RomHashes::check_bytes`],
//! `checks::Ctx::verify`).
//!
//! The format is `sha256sum` output (`<hex>  <path>`, paths relative to
//! `test-roms/`, sorted), plus `#` comment lines, one of which is
//! `# release: <version>`. `scripts/hash-test-roms.sh` writes it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use sha2::{Digest, Sha256};

use crate::manifest::{Check, Manifest};

/// The list file name; it lives next to the manifest.
pub const FILE_NAME: &str = "test-roms.sha256";

const RELEASE_PREFIX: &str = "# release:";
const REGENERATE: &str = "re-fetch with scripts/fetch-test-roms.sh; regenerate the list with \
     scripts/hash-test-roms.sh only when the pinned release changes";
/// How many offending paths an error message names.
const SHOWN: usize = 10;

pub type Sha256Hash = [u8; 32];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RomHashes {
    pub release: String,
    /// Relative path → SHA-256.
    pub files: BTreeMap<String, Sha256Hash>,
}

impl RomHashes {
    pub fn contains(&self, path: &str) -> bool {
        self.files.contains_key(path)
    }

    /// Checks the bytes a run actually loaded from `path` (relative to
    /// `test-roms/`) against the list. The up-front [`verify`] can't
    /// cover a file that changes between that check and the read, so
    /// every ROM and reference PNG is hashed again as it is loaded.
    pub fn check_bytes(&self, path: &str, bytes: &[u8]) -> Result<(), String> {
        let Some(want) = self.files.get(path) else {
            return Err(format!(
                "{path} is not in the test-roms integrity list, so it can't be trusted"
            ));
        };
        let got: Sha256Hash = Sha256::digest(bytes).into();
        if &got != want {
            return Err(format!(
                "{path} changed after the integrity check: the bytes loaded don't match its \
                 SHA-256 in the integrity list ({REGENERATE})"
            ));
        }
        Ok(())
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let mut release = None;
        let mut files = BTreeMap::new();
        for (i, line) in text.lines().enumerate() {
            let err = |msg: &str| Err(format!("line {}: {msg}", i + 1));
            if let Some(r) = line.strip_prefix(RELEASE_PREFIX) {
                if release.replace(r.trim().to_owned()).is_some() {
                    return err("more than one release line");
                }
                continue;
            }
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            let Some((hex, path)) = line.split_once("  ") else {
                return err("expected `<sha256>  <path>`");
            };
            let Some(hash) = parse_hex(hex) else {
                return err("bad SHA-256 (64 lowercase hex digits)");
            };
            if path.is_empty()
                || path.starts_with('/')
                || path.starts_with('\\')
                || path
                    .split('/')
                    .any(|s| s.is_empty() || s == "." || s == "..")
            {
                return err(&format!(
                    "bad path {path:?} (must be relative to test-roms/)"
                ));
            }
            if files.insert(path.to_owned(), hash).is_some() {
                return err(&format!("{path:?} listed twice"));
            }
        }
        let release = release
            .filter(|r| !r.is_empty())
            .ok_or_else(|| format!("no `{RELEASE_PREFIX} <version>` line"))?;
        if files.is_empty() {
            return Err("no files listed".into());
        }
        Ok(Self { release, files })
    }

    /// The list in the format `scripts/hash-test-roms.sh` writes.
    #[cfg(test)]
    pub fn render(&self) -> String {
        use std::fmt::Write as _;
        let mut s = format!("# test\n{RELEASE_PREFIX} {}\n", self.release);
        for (path, hash) in &self.files {
            let _ = writeln!(s, "{}  {path}", to_hex(hash));
        }
        s
    }
}

pub fn parse_hex(s: &str) -> Option<Sha256Hash> {
    if s.len() != 64 || !s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

pub fn to_hex(h: &Sha256Hash) -> String {
    h.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
pub fn hash_file(path: &Path) -> Result<Sha256Hash, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    Ok(Sha256::digest(&bytes).into())
}

/// Loads the list at `path`. A missing list is an error: without it the
/// ROM set can't be trusted.
pub fn load(path: &Path) -> Result<RomHashes, String> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        format!(
            "reading the test-roms integrity list {}: {e} (generate it with scripts/hash-test-roms.sh)",
            path.display()
        )
    })?;
    RomHashes::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// What [`verify`] checked.
#[derive(Clone, Copy, Debug)]
pub struct Verified {
    pub files: usize,
    pub bytes: u64,
}

/// Checks `test-roms/` against the list (see the module docs).
/// `on_disk` is every file under `roms_dir`; `matched_roms` is every ROM
/// any manifest glob matches. Hashes on up to `threads` threads.
pub fn verify<'a>(
    list: &RomHashes,
    release: &str,
    roms_dir: &Path,
    on_disk: &BTreeSet<String>,
    matched_roms: impl IntoIterator<Item = &'a str>,
    threads: usize,
) -> Result<Verified, String> {
    if list.release != release {
        return Err(format!(
            "the test-roms integrity list is for release {:?}, but the manifest and test-roms/.version are {release:?} ({REGENERATE})",
            list.release
        ));
    }
    let missing: Vec<&str> = list
        .files
        .keys()
        .filter(|p| !on_disk.contains(*p))
        .map(String::as_str)
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "{} file(s) in the test-roms integrity list are missing from {}: {} ({REGENERATE})",
            missing.len(),
            roms_dir.display(),
            names(&missing)
        ));
    }
    let mut unlisted: Vec<&str> = matched_roms
        .into_iter()
        .filter(|r| !list.contains(r))
        .collect();
    unlisted.sort_unstable();
    unlisted.dedup();
    if !unlisted.is_empty() {
        return Err(format!(
            "{} ROM(s) matched by the manifest are not in the test-roms integrity list: {} ({REGENERATE})",
            unlisted.len(),
            names(&unlisted)
        ));
    }

    // Hash everything listed: about 170 MB for v7.0, a fraction of a second
    // on a few threads, and it covers every file a check could load.
    let entries: Vec<(&String, &Sha256Hash)> = list.files.iter().collect();
    let next = AtomicUsize::new(0);
    let bad = Mutex::new(Vec::<String>::new());
    let bytes = AtomicUsize::new(0);
    let io_error = Mutex::new(None::<String>);
    std::thread::scope(|s| {
        for _ in 0..threads.clamp(1, entries.len()) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(&(path, want)) = entries.get(i) else {
                        break;
                    };
                    let full = roms_dir.join(path);
                    match std::fs::read(&full) {
                        Ok(data) => {
                            bytes.fetch_add(data.len(), Ordering::Relaxed);
                            let got: Sha256Hash = Sha256::digest(&data).into();
                            if &got != want {
                                bad.lock().expect("lock").push(path.clone());
                            }
                        }
                        Err(e) => {
                            io_error
                                .lock()
                                .expect("lock")
                                .get_or_insert(format!("reading {}: {e}", full.display()));
                        }
                    }
                }
            });
        }
    });
    if let Some(e) = io_error.into_inner().expect("lock") {
        return Err(e);
    }
    let mut bad = bad.into_inner().expect("lock");
    if !bad.is_empty() {
        bad.sort_unstable();
        let bad: Vec<&str> = bad.iter().map(String::as_str).collect();
        return Err(format!(
            "{} file(s) in test-roms/ don't match their SHA-256 in the integrity list: {} ({REGENERATE})",
            bad.len(),
            names(&bad)
        ));
    }
    Ok(Verified {
        files: entries.len(),
        bytes: bytes.into_inner() as u64,
    })
}

/// Checks bytes loaded for a committed reference override (`path`,
/// relative to the manifest's directory) against the SHA-256 pinned in the
/// manifest. `Err` is a runner error (exit 2), as for `test-roms/`.
pub fn check_pinned(path: &str, bytes: &[u8], want: &Sha256Hash) -> Result<(), String> {
    let got: Sha256Hash = Sha256::digest(bytes).into();
    if &got != want {
        return Err(format!(
            "committed reference {path} has SHA-256 {}, but the manifest pins {} \
             (regenerate it only with scripts/recolor-reference.exs, on explicit human approval)",
            to_hex(&got),
            to_hex(want)
        ));
    }
    Ok(())
}

/// Reads and checks every committed reference override in the manifest
/// (whatever the selection, like the `test-roms/` list). A missing file is
/// an error, never `no-reference`. Returns how many were checked.
pub fn verify_pinned(manifest_dir: &Path, manifest: &Manifest) -> Result<usize, String> {
    let mut n = 0;
    for check in manifest.groups.iter().flat_map(|g| &g.checks) {
        if let Check::PinnedScreenshot { path, sha256 } = check {
            let full = manifest_dir.join(path);
            let bytes = std::fs::read(&full)
                .map_err(|e| format!("reading committed reference {}: {e}", full.display()))?;
            check_pinned(path, &bytes, sha256)?;
            n += 1;
        }
    }
    Ok(n)
}

fn names(paths: &[&str]) -> String {
    let mut s = paths
        .iter()
        .take(SHOWN)
        .map(|p| format!("{p:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    if paths.len() > SHOWN {
        s.push_str(&format!(" and {} more", paths.len() - SHOWN));
    }
    s
}

/// Builds the list for every file under `roms_dir` (tests only; the real
/// list comes from `scripts/hash-test-roms.sh`).
#[cfg(test)]
pub fn generate(roms_dir: &Path, release: &str) -> RomHashes {
    let files = crate::manifest::list_files(roms_dir)
        .unwrap()
        .into_iter()
        .map(|p| {
            let h = hash_file(&roms_dir.join(&p)).unwrap();
            (p, h)
        })
        .collect();
    RomHashes {
        release: release.to_owned(),
        files,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn parses_sha256sum_output_with_comments() {
        let text = format!("# hello\n# release: v7.0\n\n{ABC}  a/b c.gb\n{ABC}  .version\n");
        let l = RomHashes::parse(&text).unwrap();
        assert_eq!(l.release, "v7.0");
        assert_eq!(l.files.len(), 2);
        assert_eq!(to_hex(&l.files["a/b c.gb"]), ABC);
        assert_eq!(RomHashes::parse(&l.render()).unwrap(), l);
    }

    #[test]
    fn rejects_bad_lists() {
        let line = format!("{ABC}  a.gb\n");
        let cases = [
            (line.clone(), "release"),
            ("# release: v7.0\n".to_string(), "no files"),
            (
                format!("# release: v7.0\n# release: v7.0\n{line}"),
                "more than one",
            ),
            (format!("# release: v7.0\n{}  a.gb\n", &ABC[1..]), "SHA-256"),
            (
                format!("# release: v7.0\n{}  a.gb\n", ABC.to_uppercase()),
                "SHA-256",
            ),
            (format!("# release: v7.0\n{ABC} a.gb\n"), "expected"),
            (format!("# release: v7.0\n{ABC}  /a.gb\n"), "bad path"),
            (format!("# release: v7.0\n{ABC}  a/../b.gb\n"), "bad path"),
            (format!("# release: v7.0\n{ABC}  \\a\\nb.gb\n"), "bad path"),
            (format!("# release: v7.0\n{line}{line}"), "twice"),
        ];
        for (text, needle) in cases {
            let e = RomHashes::parse(&text).unwrap_err();
            assert!(e.contains(needle), "{text:?} => {e}");
        }
    }

    #[test]
    fn the_committed_list_parses_and_matches_the_manifest() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../acceptance");
        let list = load(&root.join(FILE_NAME)).unwrap();
        let manifest =
            crate::manifest::parse(&std::fs::read_to_string(root.join("stage1.toml")).unwrap())
                .unwrap();
        assert_eq!(list.release, manifest.roms_release);
        assert!(list.contains(".version"));
    }

    #[test]
    fn hashes_match_sha256sum() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("x"), b"abc").unwrap();
        assert_eq!(to_hex(&hash_file(&dir.path().join("x")).unwrap()), ABC);
    }

    #[test]
    fn verify_detects_every_kind_of_tampering() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        std::fs::create_dir_all(d.join("s")).unwrap();
        std::fs::write(d.join("s/a.gb"), b"a").unwrap();
        std::fs::write(d.join("s/a.png"), b"png").unwrap();
        let list = generate(d, "v7.0");
        let disk = |d: &Path| crate::manifest::list_files(d).unwrap();
        let v = |list: &RomHashes, matched: &[&str]| {
            verify(list, "v7.0", d, &disk(d), matched.iter().copied(), 4)
        };

        let ok = v(&list, &["s/a.gb"]).unwrap();
        assert_eq!((ok.files, ok.bytes), (2, 4));
        // Unlisted extra files are fine unless a glob matches them.
        std::fs::write(d.join("s/extra.gb"), b"x").unwrap();
        v(&list, &["s/a.gb"]).unwrap();
        assert!(
            v(&list, &["s/a.gb", "s/extra.gb"])
                .unwrap_err()
                .contains("not in the test-roms integrity list: \"s/extra.gb\"")
        );
        // Release mismatch.
        let mut old = list.clone();
        old.release = "v6.0".into();
        assert!(v(&old, &[]).unwrap_err().contains("\"v6.0\""));
        // Content mismatch.
        std::fs::write(d.join("s/a.png"), b"PNG").unwrap();
        assert!(
            v(&list, &[])
                .unwrap_err()
                .contains("don't match their SHA-256 in the integrity list: \"s/a.png\"")
        );
        // Missing file (a deleted ROM no longer matches any glob).
        std::fs::remove_file(d.join("s/a.gb")).unwrap();
        assert!(v(&list, &[]).unwrap_err().contains("missing"));
    }
}
