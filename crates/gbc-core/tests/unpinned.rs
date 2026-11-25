//! Keeps `docs/reference/unpinned.md` and the `UNPINNED:` comment tags in
//! `crates/gbc-core/src` in step (M10 review E7), so the list cannot rot.
//!
//! A tag is a comment line containing `UNPINNED: <id> (<class>):`, with
//! `<id>` in lower-case letters, digits and `-`, and `<class>` one of `a`,
//! `b`, `c` (the list explains them). A row of the list is a table line
//! whose first cell is the id in backticks, the second the class, the
//! third starts with the file (relative to `src`) in backticks, and the
//! last is `tagged` (a row without a tag in the source is not accepted).
//!
//! The test fails when:
//! - a line of the source mentions `UNPINNED:` without being a well-formed
//!   tag in a comment, or two tags share an id;
//! - a tag has no `tagged` row with the same id, class and file, or such
//!   a row has no tag;
//! - a row's last cell is anything but `tagged`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const TAG: &str = "UNPINNED:";

fn crate_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

fn is_id(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn is_class(s: &str) -> bool {
    matches!(s, "a" | "b" | "c")
}

/// `<id> (<class>):` at the start of `rest` (the text after the tag).
fn parse_tag(rest: &str) -> Option<(String, String)> {
    let rest = rest.strip_prefix(' ')?;
    let (id, rest) = rest.split_once(" (")?;
    let (class, _) = rest.split_once("):")?;
    (is_id(id) && is_class(class)).then(|| (id.to_string(), class.to_string()))
}

/// Tags of one source text: id -> class, and the problems found.
fn tags_in(src: &str, file: &str, errors: &mut Vec<String>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (n, line) in src.lines().enumerate() {
        let Some(at) = line.find(TAG) else { continue };
        let at_line = format!("{file}:{}", n + 1);
        if !line.trim_start().starts_with("//") {
            errors.push(format!("{at_line}: `{TAG}` outside a comment line"));
            continue;
        }
        match parse_tag(&line[at + TAG.len()..]) {
            Some(t) => out.push(t),
            None => errors.push(format!(
                "{at_line}: malformed tag, want `{TAG} <id> (<a|b|c>):`"
            )),
        }
    }
    out
}

struct Row {
    class: String,
    file: String,
}

/// The rows of the list: id -> row.
fn rows_in(doc: &str, errors: &mut Vec<String>) -> BTreeMap<String, Row> {
    let mut rows = BTreeMap::new();
    for (n, line) in doc.lines().enumerate() {
        if !line.starts_with("| `") {
            continue;
        }
        let at_line = format!("unpinned.md:{}", n + 1);
        let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
        let id = cells[0].trim_matches('`');
        let class = cells.get(1).copied().unwrap_or("");
        let file = cells
            .get(2)
            .and_then(|c| c.strip_prefix('`'))
            .and_then(|c| c.split_once('`'))
            .map_or("", |(f, _)| f);
        let last = cells.last().copied().unwrap_or("");
        if cells.len() != 9 || !is_id(id) || !is_class(class) || file.is_empty() {
            errors.push(format!("{at_line}: malformed row"));
            continue;
        }
        if last != "tagged" {
            errors.push(format!("{at_line}: last cell must be `tagged`"));
            continue;
        }
        let row = Row {
            class: class.to_string(),
            file: file.to_string(),
        };
        if rows.insert(id.to_string(), row).is_some() {
            errors.push(format!("{at_line}: second row for `{id}`"));
        }
    }
    rows
}

/// Every disagreement between the tags (id -> (class, file)) and the rows.
fn compare(tags: &BTreeMap<String, (String, String)>, rows: &BTreeMap<String, Row>) -> Vec<String> {
    let mut errors = Vec::new();
    for (id, (class, file)) in tags {
        match rows.get(id) {
            None => errors.push(format!("{file}: tag `{id}` has no row in unpinned.md")),
            Some(r) => {
                if r.class != *class {
                    errors.push(format!("`{id}`: class ({class}) differs from its row's"));
                }
                if r.file != *file {
                    errors.push(format!("`{id}`: tagged in {file}, its row says {}", r.file));
                }
            }
        }
    }
    for (id, r) in rows {
        if !tags.contains_key(id) {
            errors.push(format!("row `{id}`: no such tag in {}", r.file));
        }
    }
    errors
}

#[test]
fn unpinned_tags_and_the_reference_list_agree() {
    let src_dir = crate_dir().join("src");
    let mut files = Vec::new();
    rust_files(&src_dir, &mut files);
    files.sort();
    let mut errors = Vec::new();
    let mut tags = BTreeMap::new();
    for f in &files {
        let rel = f
            .strip_prefix(&src_dir)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let src = std::fs::read_to_string(f).unwrap();
        for (id, class) in tags_in(&src, &rel, &mut errors) {
            if let Some((_, first)) = tags.insert(id.clone(), (class, rel.clone())) {
                errors.push(format!("tag `{id}` twice: {first} and {rel}"));
            }
        }
    }
    let doc = crate_dir().join("../../docs/reference/unpinned.md");
    let doc = std::fs::read_to_string(&doc).unwrap_or_else(|e| panic!("{}: {e}", doc.display()));
    let rows = rows_in(&doc, &mut errors);
    errors.extend(compare(&tags, &rows));
    assert!(!tags.is_empty(), "no `{TAG}` tags found under src");
    assert!(
        errors.is_empty(),
        "{} problem(s) (see docs/reference/unpinned.md):\n{}",
        errors.len(),
        errors.join("\n")
    );
}

#[test]
fn tag_and_row_parsers() {
    assert_eq!(
        parse_tag(" hdma-x1 (c): text"),
        Some(("hdma-x1".into(), "c".into()))
    );
    for bad in [
        " Hdma (c): x",
        " hdma (d): x",
        " hdma: x",
        "hdma (a): x",
        " (a): x",
    ] {
        assert_eq!(parse_tag(bad), None, "{bad}");
    }
    let mut errors = Vec::new();
    let src = "// UNPINNED: one (a): x\nlet s = \"UNPINNED: two (a): x\";\n/// UNPINNED: three\n";
    assert_eq!(
        tags_in(src, "f.rs", &mut errors),
        [("one".into(), "a".into())]
    );
    assert_eq!(errors.len(), 2, "{errors:?}");

    let doc = "| Id |\n|---|\n\
        | `one` | a | `f.rs` `g` | b | f | d | s | r | tagged |\n\
        | `two` | c | `ppu/mod.rs` `h` | b | f | d | s | r | tag pending (STAT walk rework) |\n\
        | `bad` | a | `f.rs` | b | f | d | s | r | later |\n\
        | `short` | a | `f.rs` | tagged |\n";
    let mut errors = Vec::new();
    let rows = rows_in(doc, &mut errors);
    assert_eq!(errors.len(), 3, "{errors:?}");
    assert_eq!(rows.len(), 1);

    let tag = |c: &str, f: &str| (c.to_string(), f.to_string());
    let mut tags = BTreeMap::new();
    tags.insert("one".to_string(), tag("a", "f.rs"));
    assert!(compare(&tags, &rows).is_empty());
    // Each kind of disagreement is reported.
    tags.insert("new".to_string(), tag("a", "f.rs"));
    assert_eq!(compare(&tags, &rows).len(), 1);
    tags.remove("new");
    tags.insert("one".to_string(), tag("b", "g.rs"));
    assert_eq!(compare(&tags, &rows).len(), 2);
    tags.clear();
    assert_eq!(compare(&tags, &rows).len(), 1);
}
