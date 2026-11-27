//! gbc-core can't use this crate (a dependency there needs the human, even
//! a dev one), so `crates/gbc-core/tests/hard_rules.rs` keeps its own copy
//! of the checker. This fails if any shared item differs from `src/lib.rs`,
//! comments, blank lines and `pub` aside.

use std::path::Path;

const ITEMS: [&str; 11] = [
    "enum Tok",
    "fn tokenize",
    "fn raw_string_prefix",
    "fn is_ident",
    "fn is_punct",
    "fn test_mask",
    "fn violations",
    "fn rust_files",
    "fn has_inner_attr",
    "fn manifest_escape_hatches",
    "fn dependency_entries",
];

/// The item starting at the line `[pub ]<head>` up to its closing `}` in
/// column 0, with comments, blank lines and `pub` removed.
fn item(src: &str, head: &str) -> String {
    let mut out = String::new();
    let mut on = false;
    for line in src.lines() {
        let l = line.strip_prefix("pub ").unwrap_or(line);
        if !on && l.starts_with(head) && !l.starts_with(&format!("{head}_")) {
            on = true;
        }
        if on {
            let t = line.trim();
            if !t.is_empty() && !t.starts_with("//") {
                out.push_str(l.trim_end());
                out.push('\n');
            }
            if line == "}" {
                return out;
            }
        }
    }
    panic!("`{head}` not found");
}

#[test]
fn cores_copy_of_the_checker_matches() {
    let here =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs")).unwrap();
    let core = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../crates/gbc-core/tests/hard_rules.rs"),
    )
    .unwrap();
    for head in ITEMS {
        let (a, b) = (item(&here, head), item(&core, head));
        assert!(a.lines().count() > 2, "`{head}` too short: {a:?}");
        assert_eq!(
            a, b,
            "`{head}` in gbc-core/tests/hard_rules.rs differs from tools/hard-rules/src/lib.rs; \
             gbc-core's copy needs the human to change (AGENTS.md)"
        );
    }
}
