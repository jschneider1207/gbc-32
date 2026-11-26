//! Enforces gbc-firmware's rules (ADR 0036): `no_std`, no `alloc` or
//! `std`, `#![forbid(unsafe_code)]`, no `#[path]` or `include!`, no build
//! script or `[lib] path`, and no dependency except `gbc-core` (by path).
//! Everything with `unsafe` or registers belongs in the board crate.
//!
//! The checker (tokenizer, test-code rule, manifest scan) is a copy of
//! `crates/gbc-core/tests/hard_rules.rs`, whose docs describe it in full:
//! the same rules, so the same code. The two crates can't share a test
//! file without `#[path]`, which the checker itself bans in core sources,
//! and gbc-core may not gain a dependency. Keep the copies in step; the
//! checker's own tests are copied too, so a divergent copy fails here.
//!
//! In short: comments and literals are ignored; `std`/`alloc` paths and
//! `extern crate` are allowed only inside an inline
//! `#[cfg(test)] mod <name> { ... }`; `unsafe`, `#[path]` and `include!`
//! are banned everywhere, test code included.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Tok {
    Ident(String),
    Punct(char),
}

/// Rust source to tokens, dropping comments (nested block comments
/// included), whitespace and the contents of string, byte-string, raw
/// string and char literals (each becomes a single `Punct('"')`).
/// Lifetimes become `Punct('\'')` followed by the identifier. Good enough
/// for the patterns above; not a full lexer.
fn tokenize(src: &str) -> Vec<(Tok, usize)> {
    let b: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let mut line = 1;
    let n = b.len();
    let ident_start = |c: char| c.is_alphabetic() || c == '_';
    let ident_char = |c: char| c.is_alphanumeric() || c == '_';
    while i < n {
        let c = b[i];
        if c == '\n' {
            line += 1;
            i += 1;
        } else if c.is_whitespace() {
            i += 1;
        } else if c == '/' && b.get(i + 1) == Some(&'/') {
            while i < n && b[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && b.get(i + 1) == Some(&'*') {
            let mut depth = 0;
            while i < n {
                if b[i] == '/' && b.get(i + 1) == Some(&'*') {
                    depth += 1;
                    i += 2;
                } else if b[i] == '*' && b.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    if b[i] == '\n' {
                        line += 1;
                    }
                    i += 1;
                }
            }
        } else if let Some(len) = raw_string_prefix(&b[i..]) {
            // r"..", r#".."#, br#".."#, cr#".."#: no escapes.
            let start_line = line;
            let hashes = b[i..i + len].iter().filter(|&&c| c == '#').count();
            i += len;
            while i < n {
                if b[i] == '"'
                    && b[i + 1..]
                        .iter()
                        .take(hashes)
                        .filter(|&&c| c == '#')
                        .count()
                        == hashes
                {
                    i += 1 + hashes;
                    break;
                }
                if b[i] == '\n' {
                    line += 1;
                }
                i += 1;
            }
            out.push((Tok::Punct('"'), start_line));
        } else if c == '"' || ((c == 'b' || c == 'c') && b.get(i + 1) == Some(&'"')) {
            let start_line = line;
            i += if c == '"' { 1 } else { 2 };
            while i < n && b[i] != '"' {
                if b[i] == '\\' {
                    i += 1;
                }
                if i < n && b[i] == '\n' {
                    line += 1;
                }
                i += 1;
            }
            i += 1;
            out.push((Tok::Punct('"'), start_line));
        } else if c == '\'' || (c == 'b' && b.get(i + 1) == Some(&'\'')) {
            let q = if c == 'b' { i + 1 } else { i };
            // A char literal is 'x' or '\...'; otherwise a lifetime/label.
            if b.get(q + 1) == Some(&'\\') {
                // Skip the escaped char (it may itself be a quote: `'\''`).
                let mut j = q + 3;
                while j < n && b[j] != '\'' {
                    j += 1;
                }
                i = j + 1;
                out.push((Tok::Punct('"'), line));
            } else if b.get(q + 2) == Some(&'\'') {
                i = q + 3;
                out.push((Tok::Punct('"'), line));
            } else {
                out.push((Tok::Punct('\''), line));
                i = q + 1;
            }
        } else if ident_start(c) {
            let s = i;
            while i < n && ident_char(b[i]) {
                i += 1;
            }
            // Raw identifiers: r#std is the identifier std.
            let word: String = b[s..i].iter().collect();
            if word == "r"
                && b.get(i) == Some(&'#')
                && b.get(i + 1).is_some_and(|&c| ident_start(c))
            {
                let s2 = i + 1;
                i = s2;
                while i < n && ident_char(b[i]) {
                    i += 1;
                }
                out.push((Tok::Ident(b[s2..i].iter().collect()), line));
            } else {
                out.push((Tok::Ident(word), line));
            }
        } else if c.is_ascii_digit() {
            while i < n && (b[i].is_alphanumeric() || b[i] == '_' || b[i] == '.') {
                // Stop before a range `..` after a number.
                if b[i] == '.' && b.get(i + 1) == Some(&'.') {
                    break;
                }
                i += 1;
            }
        } else {
            out.push((Tok::Punct(c), line));
            i += 1;
        }
    }
    out
}

/// Length of a raw string prefix (`r"`, `r#"`, `br##"`, `cr"`, ...) at
/// the start of `b`, up to and including the opening quote.
fn raw_string_prefix(b: &[char]) -> Option<usize> {
    let mut j = 0;
    if matches!(b.first(), Some('b' | 'c')) {
        j += 1;
    }
    if b.get(j) != Some(&'r') {
        return None;
    }
    j += 1;
    while b.get(j) == Some(&'#') {
        j += 1;
    }
    (b.get(j) == Some(&'"')).then_some(j + 1)
}

fn is_ident(t: Option<&(Tok, usize)>, s: &str) -> bool {
    matches!(t, Some((Tok::Ident(w), _)) if w == s)
}

fn is_punct(t: Option<&(Tok, usize)>, c: char) -> bool {
    matches!(t, Some((Tok::Punct(p), _)) if *p == c)
}

/// Marks the tokens inside `#[cfg(test)] mod <name> { ... }` bodies.
fn test_mask(toks: &[(Tok, usize)]) -> Vec<bool> {
    let mut mask = vec![false; toks.len()];
    let mut i = 0;
    while i < toks.len() {
        // `# [ cfg ( test ) ]`
        let is_cfg_test = is_punct(toks.get(i), '#')
            && is_punct(toks.get(i + 1), '[')
            && is_ident(toks.get(i + 2), "cfg")
            && is_punct(toks.get(i + 3), '(')
            && is_ident(toks.get(i + 4), "test")
            && is_punct(toks.get(i + 5), ')')
            && is_punct(toks.get(i + 6), ']');
        if !is_cfg_test {
            i += 1;
            continue;
        }
        // Skip further outer attributes `#[...]`.
        let mut j = i + 7;
        while is_punct(toks.get(j), '#') && is_punct(toks.get(j + 1), '[') {
            let mut depth = 0;
            j += 1;
            while j < toks.len() {
                if is_punct(toks.get(j), '[') {
                    depth += 1;
                } else if is_punct(toks.get(j), ']') {
                    depth -= 1;
                    if depth == 0 {
                        j += 1;
                        break;
                    }
                }
                j += 1;
            }
        }
        if is_ident(toks.get(j), "mod")
            && matches!(toks.get(j + 1), Some((Tok::Ident(_), _)))
            && is_punct(toks.get(j + 2), '{')
        {
            let mut depth = 0;
            let mut k = j + 2;
            while k < toks.len() {
                if is_punct(toks.get(k), '{') {
                    depth += 1;
                } else if is_punct(toks.get(k), '}') {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                mask[k] = true;
                k += 1;
            }
            i = k;
        } else {
            i += 7;
        }
    }
    mask
}

/// Every violation in one file's source, as `line: message`.
fn violations(src: &str) -> Vec<String> {
    let toks = tokenize(src);
    let test = test_mask(&toks);
    let mut out = Vec::new();
    for (i, (tok, line)) in toks.iter().enumerate() {
        let at = |k: usize| toks.get(k);
        let Tok::Ident(w) = tok else {
            // `#[path` / `#![path`
            if is_punct(Some(&toks[i]), '#') {
                let k = if is_punct(at(i + 1), '!') {
                    i + 2
                } else {
                    i + 1
                };
                if is_punct(at(k), '[') && is_ident(at(k + 1), "path") {
                    out.push(format!("{line}: #[path] attribute"));
                }
            }
            continue;
        };
        // `#[cfg_attr(..., path = "...")]`
        if w == "cfg_attr" && is_punct(at(i + 1), '(') {
            let mut depth = 0;
            for k in i + 1..toks.len() {
                if is_punct(at(k), '(') {
                    depth += 1;
                } else if is_punct(at(k), ')') {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                } else if is_ident(at(k), "path") && is_punct(at(k + 1), '=') {
                    out.push(format!("{line}: #[path] attribute (in cfg_attr)"));
                }
            }
        }
        if w == "include" && is_punct(at(i + 1), '!') {
            out.push(format!("{line}: include!"));
        }
        if w == "unsafe" {
            out.push(format!("{line}: `unsafe` (ADR 0009)"));
        }
        if test[i] {
            continue;
        }
        let root = w == "std" || w == "alloc";
        if w == "extern"
            && is_ident(at(i + 1), "crate")
            && (is_ident(at(i + 2), "std") || is_ident(at(i + 2), "alloc"))
        {
            out.push(format!("{line}: extern crate outside a #[cfg(test)] mod"));
        }
        if root {
            // `x::std::...` (e.g. `core::alloc`) is a submodule, not the
            // crate. `::std::...` and a bare `std::...` are the crate.
            let after_path_sep = i >= 2
                && is_punct(at(i - 1), ':')
                && is_punct(at(i - 2), ':')
                && matches!(at(i.wrapping_sub(3)), Some((Tok::Ident(_), _)));
            let path = is_punct(at(i + 1), ':') && is_punct(at(i + 2), ':');
            let used = i >= 1 && is_ident(at(i - 1), "use");
            if (path && !after_path_sep) || used {
                out.push(format!("{line}: `{w}` path outside a #[cfg(test)] mod"));
            }
        }
    }
    out
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

/// Whether `src` has the crate attribute `#![<attr>]` (given as tokens;
/// comments and whitespace don't matter) among its leading inner
/// attributes, i.e. before the first item. An attribute inside a nested
/// `mod x { #![...] }` doesn't count: it wouldn't apply to the crate.
fn has_inner_attr(src: &str, attr: &[&str]) -> bool {
    let toks: Vec<Tok> = tokenize(src).into_iter().map(|t| t.0).collect();
    let want: Vec<Tok> = attr
        .iter()
        .map(|s| match s.parse::<char>() {
            Ok(c) if !c.is_alphanumeric() && c != '_' => Tok::Punct(c),
            _ => Tok::Ident((*s).into()),
        })
        .collect();
    let p = |k: usize, c: char| toks.get(k) == Some(&Tok::Punct(c));
    let mut i = 0;
    while p(i, '#') && p(i + 1, '!') && p(i + 2, '[') {
        let start = i + 3;
        let mut depth = 1;
        let mut k = start;
        while k < toks.len() {
            if p(k, '[') {
                depth += 1;
            } else if p(k, ']') {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            k += 1;
        }
        if k >= toks.len() {
            return false;
        }
        if toks[start..k] == want[..] {
            return true;
        }
        i = k + 1;
    }
    false
}

/// Cargo.toml keys that would let the crate pull in code from outside
/// `src/` unseen by the source scan: `[lib] path` and `[package] build`
/// (also written as dotted keys at the top level).
fn manifest_escape_hatches(toml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut section = String::new();
    for (n, raw) in toml.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            section = line
                .trim_matches(|c| c == '[' || c == ']')
                .chars()
                .filter(|c| !c.is_whitespace() && *c != '"' && *c != '\'')
                .collect();
            continue;
        }
        let key: String = line
            .split('=')
            .next()
            .unwrap_or("")
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '"' && *c != '\'')
            .collect();
        let full = if section.is_empty() {
            key
        } else {
            format!("{section}.{key}")
        };
        if full == "lib.path" || full == "package.build" {
            out.push(format!("{}: {line}", n + 1));
        }
    }
    out
}

/// Entries in any dependency table of a Cargo.toml.
fn dependency_entries(toml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_deps = false;
    for (n, raw) in toml.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            let name: String = line
                .trim_matches(|c| c == '[' || c == ']')
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect();
            in_deps = name.split('.').any(|part| part.ends_with("dependencies"));
            // `[dependencies.foo]` is itself an entry.
            if in_deps && !name.ends_with("dependencies") {
                out.push(format!("{}: {line}", n + 1));
            }
            continue;
        }
        if in_deps
            || line
                .split('=')
                .next()
                .unwrap_or("")
                .contains("dependencies")
        {
            out.push(format!("{}: {line}", n + 1));
        }
    }
    out
}

fn crate_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

#[test]
fn firmware_sources_follow_the_rules() {
    let mut files = Vec::new();
    rust_files(&crate_dir().join("src"), &mut files);
    assert!(files.len() >= 4, "found only {files:?}");
    let mut bad = Vec::new();
    for f in &files {
        let src = std::fs::read_to_string(f).unwrap();
        for v in violations(&src) {
            bad.push(format!("{}:{v}", f.display()));
        }
    }
    assert!(
        bad.is_empty(),
        "gbc-firmware rule violations:\n{}",
        bad.join("\n")
    );
    let lib = std::fs::read_to_string(crate_dir().join("src/lib.rs")).unwrap();
    assert!(
        has_inner_attr(&lib, &["no_std"]),
        "lib.rs must declare #![no_std]"
    );
    assert!(
        has_inner_attr(&lib, &["forbid", "(", "unsafe_code", ")"]),
        "lib.rs must declare #![forbid(unsafe_code)] (ADR 0036)"
    );
}

#[test]
fn firmware_depends_on_gbc_core_only() {
    let toml = std::fs::read_to_string(crate_dir().join("Cargo.toml")).unwrap();
    let deps: Vec<String> = dependency_entries(&toml)
        .into_iter()
        .map(|e| e.split_once(": ").map_or(e.clone(), |(_, l)| l.to_string()))
        .collect();
    assert_eq!(
        deps,
        ["gbc-core = { path = \"../gbc-core\" }"],
        "gbc-firmware may depend on gbc-core only (ADR 0036, D9), in [dependencies]"
    );
    // ... and that one entry is a normal dependency, not a dev or build
    // one: the table heading above it is `[dependencies]`.
    let heading = toml
        .lines()
        .take_while(|l| !l.trim_start().starts_with("gbc-core"))
        .filter(|l| l.trim_start().starts_with('['))
        .last()
        .map(str::trim);
    assert_eq!(heading, Some("[dependencies]"));
}

#[test]
fn firmware_has_no_build_script_or_lib_path() {
    let toml = std::fs::read_to_string(crate_dir().join("Cargo.toml")).unwrap();
    let bad = manifest_escape_hatches(&toml);
    assert!(
        bad.is_empty(),
        "gbc-firmware's Cargo.toml must not set [lib] path or [package] build:\n{}",
        bad.join("\n")
    );
    assert!(
        !crate_dir().join("build.rs").exists(),
        "gbc-firmware must not have a build.rs (it would run arbitrary code and could emit code)"
    );
}

// --- The checker itself. ---

#[test]
fn catches_extern_crate_however_written() {
    for src in [
        "extern crate alloc;",
        "extern   crate\tstd;",
        "extern\ncrate\n  alloc\n;",
        "extern /* hi */ crate // x\n alloc;",
        "extern crate alloc as heap;",
        "#[cfg(test)]\nextern crate std;", // cfg(test) outside a mod
        "#[cfg(any(test))]\nmod t { extern crate std; }",
        "#[cfg(test)] fn f() {} extern crate std;",
        "#[cfg(test)]\nmod t;\nextern crate std;",
    ] {
        assert!(
            violations(src).iter().any(|v| v.contains("extern crate")),
            "{src:?}"
        );
    }
}

#[test]
fn catches_std_and_alloc_paths() {
    for src in [
        "use alloc::vec::Vec;",
        "fn f() { let v = std::vec::Vec::<u8>::new(); }",
        "fn f() { ::alloc::boxed::Box::new(1); }",
        "use std;",
        "use std as s;",
        "type T = std\n::\nstring::String;",
        "fn f() { r#std::mem::drop(1); }",
    ] {
        assert!(
            violations(src).iter().any(|v| v.contains("path")),
            "{src:?}: {:?}",
            violations(src)
        );
    }
}

#[test]
fn catches_path_and_include() {
    for (src, needle) in [
        ("#[path = \"x.rs\"] mod m;", "#[path]"),
        (
            "#[cfg(test)]\nmod t {\n #[path = \"x.rs\"] mod m;\n}",
            "#[path]",
        ),
        ("#![path = \"x\"]", "#[path]"),
        ("#[ path = \"x.rs\"] mod m;", "#[path]"),
        ("#[cfg_attr(all(), path = \"x.rs\")] mod m;", "#[path]"),
        ("include!(\"x.rs\");", "include!"),
        ("#[cfg(test)] mod t { include ! (\"x.rs\"); }", "include!"),
    ] {
        assert!(
            violations(src).iter().any(|v| v.contains(needle)),
            "{src:?}"
        );
    }
}

#[test]
fn catches_unsafe_anywhere() {
    for src in [
        "unsafe fn f() {}",
        "fn f() { unsafe { g() } }",
        "unsafe impl Send for X {}",
        "unsafe trait T {}",
        "unsafe extern \"C\" {}",
        "#[cfg(test)]\nmod tests {\n fn f() { unsafe { g() } }\n}",
        "fn f() { /* hi */ unsafe\n{ g() } }",
        "fn f() { r#unsafe(); }",
    ] {
        assert!(
            violations(src).iter().any(|v| v.contains("unsafe")),
            "{src:?}"
        );
    }
    // Comments, doc comments and literals don't count.
    let ok = "//! no `unsafe` here\n/// unsafe\n// unsafe { }\nconst S: &str = \"unsafe\";";
    assert_eq!(violations(ok), Vec::<String>::new());
}

#[test]
fn crate_attributes_are_found() {
    let forbid = ["forbid", "(", "unsafe_code", ")"];
    assert!(has_inner_attr(
        "#![no_std]\n#![forbid(unsafe_code)]",
        &forbid
    ));
    assert!(has_inner_attr("# ! [ forbid ( unsafe_code ) ]", &forbid));
    assert!(has_inner_attr("#![no_std]", &["no_std"]));
    for src in [
        "#![no_std]",
        "// #![forbid(unsafe_code)]",
        "#[forbid(unsafe_code)] fn f() {}",
        "#![deny(unsafe_code)]",
        "#![forbid(unused)]",
        "const S: &str = \"#![forbid(unsafe_code)]\";",
    ] {
        assert!(!has_inner_attr(src, &forbid), "{src:?}");
    }
}

#[test]
fn leading_inner_attributes_only() {
    let forbid = ["forbid", "(", "unsafe_code", ")"];
    assert!(has_inner_attr(
        "//! docs\n#![no_std]\n#![forbid(unsafe_code)]\nfn f() {}",
        &forbid
    ));
    for src in [
        "mod x { #![forbid(unsafe_code)] }",
        "fn f() {}\n#![forbid(unsafe_code)]",
        "#![no_std]\nmod x {\n#![forbid(unsafe_code)]\n}",
        "#[derive(Debug)] struct S;\n#![forbid(unsafe_code)]",
        "#![no_std",
    ] {
        assert!(!has_inner_attr(src, &forbid), "{src:?}");
    }
}

#[test]
fn escaped_quote_char_literals_lex_as_one_token() {
    for src in ["'\\''", "b'\\''", "'\\\\'", "'\\u{27}'", "'\\n'"] {
        let toks = tokenize(src);
        assert_eq!(toks.len(), 1, "{src:?}: {toks:?}");
        assert_eq!(toks[0].0, Tok::Punct('"'), "{src:?}");
    }
    // Nothing after the literal is swallowed or misread.
    let toks = tokenize("'\\'' x");
    assert_eq!(toks.len(), 2, "{toks:?}");
    assert!(is_ident(toks.get(1), "x"));
    assert!(violations("const Q: u8 = b'\\''; unsafe fn f() {}").len() == 1);
}

#[test]
fn manifest_escape_hatches_are_found() {
    assert!(
        manifest_escape_hatches("[package]\nname = \"x\"\n[lib]\nname = \"y\"\n[features]\n")
            .is_empty()
    );
    // `path` elsewhere is not the lib path.
    assert!(manifest_escape_hatches("[lib]\nname = \"y\"\n[other]\npath = \"x\"\n").is_empty());
    for toml in [
        "[lib]\npath = \"x.rs\"\n",
        "[lib]\n path=\"x.rs\" # c\n",
        "[ lib ]\n\"path\" = \"x.rs\"\n",
        "[package]\nname = \"x\"\nbuild = \"build.rs\"\n",
        "[package]\nbuild = false\n",
        "lib.path = \"x.rs\"\n",
        "package.build = \"b.rs\"\n",
    ] {
        assert!(!manifest_escape_hatches(toml).is_empty(), "{toml:?}");
    }
}

#[test]
fn allows_test_modules_comments_strings_and_core_alloc() {
    let ok = r##"
        //! Uses std in docs: `std::vec::Vec`, extern crate alloc;
        /* extern crate alloc; /* nested */ std::x */
        use core::alloc::Layout;
        const S: &str = "extern crate std; std::vec";
        const R: &str = r#"use alloc::vec; "quoted""#;
        const C: char = '"';
        const Q: char = '\'';
        fn f<'a>(x: &'a u8) -> &'a u8 { x }
        #[cfg(test)]
        #[allow(dead_code)]
        mod tests {
            extern crate std;
            use std::vec::Vec;
            mod inner { fn g() { let _ = alloc::vec![1]; } }
        }
        fn after() {}
    "##;
    assert_eq!(violations(ok), Vec::<String>::new());
    // Code after the test module is checked again.
    let src = "#[cfg(test)] mod t { use std::x; }\nuse std::y;";
    assert_eq!(violations(src).len(), 1, "{:?}", violations(src));
    assert!(violations(src)[0].starts_with("2:"));
}

#[test]
fn dependency_tables_are_found() {
    assert!(dependency_entries("[package]\nname = \"x\"\n[dependencies]\n# none\n").is_empty());
    for toml in [
        "[dependencies]\nfoo = \"1\"\n",
        "[dev-dependencies]\nfoo = \"1\"\n",
        "[build-dependencies]\nfoo = { path = \"x\" }\n",
        "[target.'cfg(unix)'.dependencies]\nfoo = \"1\"\n",
        "[dependencies.foo]\nversion = \"1\"\n",
        "[ dependencies ]\nfoo.workspace = true\n",
        "[package]\ndependencies.foo = \"1\"\n",
    ] {
        assert!(!dependency_entries(toml).is_empty(), "{toml:?}");
    }
}
