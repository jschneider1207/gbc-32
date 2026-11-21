//! Path globs for maniflet expanded = expand_braces(pattern).map_err(|e| format!("bad glob {pattern:?}: {e}"))?;st `roms`/`exclude` and `--filter`
//! (acceptance/README.md, "Manifest schema"):
//!
//! - `*` matches any run of characters within one path segment (not `/`),
//! - `**` as a whole segment matches zero or more segments,
//! - `{a,b}` is alternation; alternatives may contain `/`, wildcards and
//!   nested braces.
//!
//! Every other character is literal (the contract defines no `?` or `[..]`).
//! Paths are relative and use `/` separators.

use std::fmt;

/// Upper bound on brace expansion, so a pathological pattern can't explode.
const MAX_ALTERNATIVES: usize = 4096;

#[derive(Clone, PartialEq, Eq)]
pub struct Glob {
    source: String,
    /// The brace-expanded alternatives, each split into `/` segments.
    alternatives: Vec<Vec<String>>,
}

impl fmt::Debug for Glob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Glob({:?})", self.source)
    }
}

impl fmt::Display for Glob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.source)
    }
}

impl Glob {
    pub fn new(pattern: &str) -> Result<Self, String> {
        let err = |msg: &str| Err(format!("bad glob {pattern:?}: {msg}"));
        if pattern.is_empty() {
            return err("empty pattern");
        }
        let expanded = expand_braces(pattern).map_err(|e| format!("bad glob {pattern:?}: {e}"))?;
        let mut alternatives = Vec::with_capacity(expanded.len());
        for alt in expanded {
            if alt.starts_with('/') {
                return err("must be relative (no leading '/')");
            }
            let segments: Vec<String> = alt.split('/').map(str::to_owned).collect();
            for s in &segments {
                match s.as_str() {
                    "" => return err("empty path segment"),
                    "." | ".." => return err("'.' and '..' segments are not allowed"),
                    _ => {}
                }
            }
            alternatives.push(segments);
        }
        Ok(Self {
            source: pattern.to_owned(),
            alternatives,
        })
    }

    pub fn as_str(&self) -> &str {
        &self.source
    }

    /// If the pattern has no wildcards, the exact paths it names (one per
    /// brace alternative). Such patterns are references to specific files.
    pub fn literal_paths(&self) -> Option<Vec<String>> {
        if self.source.contains('*') {
            return None;
        }
        Some(self.alternatives.iter().map(|a| a.join("/")).collect())
    }

    pub fn matches(&self, path: &str) -> bool {
        let segments: Vec<&str> = path.split('/').collect();
        self.alternatives
            .iter()
            .any(|alt| match_segments(alt, &segments))
    }
}

fn match_segments(pattern: &[String], path: &[&str]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((p, rest)) if p == "**" => (0..=path.len()).any(|i| match_segments(rest, &path[i..])),
        Some((p, rest)) => match path.split_first() {
            Some((s, path_rest)) => match_segment(p, s) && match_segments(rest, path_rest),
            None => false,
        },
    }
}

/// Matches one segment where `*` (or a run of them) is any substring.
fn match_segment(pattern: &str, text: &str) -> bool {
    let (p, t) = (pattern.as_bytes(), text.as_bytes());
    let (mut pi, mut ti) = (0, 0);
    // Classic greedy matcher with backtracking to the last star.
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && p[pi] == b'*' {
            star = Some((pi, ti));
            pi += 1;
        } else if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == b'*')
}

/// Expands `{a,b}` alternation (nested braces allowed) into plain patterns.
fn expand_braces(pattern: &str) -> Result<Vec<String>, String> {
    let bytes = pattern.as_bytes();
    let Some(open) = bytes.iter().position(|&c| c == b'{' || c == b'}') else {
        return Ok(vec![pattern.to_owned()]);
    };
    if bytes[open] == b'}' {
        return Err("unmatched '}'".into());
    }
    // Find the matching close brace and the top-level commas inside it.
    let mut depth = 0usize;
    let mut commas = Vec::new();
    let mut close = None;
    for (i, &c) in bytes.iter().enumerate().skip(open) {
        match c {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(i);
                    break;
                }
            }
            b',' if depth == 1 => commas.push(i),
            _ => {}
        }
    }
    let close = close.ok_or("unmatched '{'")?;
    let (prefix, suffix) = (&pattern[..open], &pattern[close + 1..]);
    let mut bounds = vec![open];
    bounds.extend(&commas);
    bounds.push(close);
    let mut out = Vec::new();
    for w in bounds.windows(2) {
        let alt = &pattern[w[0] + 1..w[1]];
        for e in expand_braces(&format!("{prefix}{alt}{suffix}"))? {
            if out.len() == MAX_ALTERNATIVES {
                return Err(format!("more than {MAX_ALTERNATIVES} alternatives"));
            }
            out.push(e);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(p: &str, path: &str) -> bool {
        Glob::new(p).unwrap().matches(path)
    }

    #[test]
    fn star_stays_within_a_segment() {
        assert!(m("a/*.gb", "a/x.gb"));
        assert!(m("a/*.gb", "a/.gb"));
        assert!(!m("a/*.gb", "a/b/x.gb"));
        assert!(!m("a/*.gb", "a/x.gbc"));
        assert!(m("a/*-*.gb", "a/x-y.gb"));
        assert!(!m("a/*-*.gb", "a/xy.gb"));
        assert!(m("a/*cgb04c*", "a/foo_cgb04c_out1.gbc"));
        assert!(m("*", "x"));
        assert!(!m("*", "x/y"));
    }

    #[test]
    fn double_star_spans_zero_or_more_segments() {
        assert!(m("a/**/*.gb", "a/x.gb"));
        assert!(m("a/**/*.gb", "a/b/x.gb"));
        assert!(m("a/**/*.gb", "a/b/c/x.gb"));
        assert!(!m("a/**/*.gb", "b/x.gb"));
        assert!(m("a/**", "a/x"));
        assert!(m("a/**", "a/b/c"));
        assert!(m("**/x.gb", "x.gb"));
        assert!(m("**/x.gb", "p/q/x.gb"));
        assert!(m("a/**/b/**/c", "a/b/c"));
        assert!(m("a/**/b/**/c", "a/1/b/2/3/c"));
        assert!(!m("a/**/b", "a/1/c"));
    }

    #[test]
    fn braces_alternate_and_nest() {
        let g = "m/acceptance/{timer,instr,interrupts,oam_dma}/**";
        assert!(m(g, "m/acceptance/timer/div_write.gb"));
        assert!(m(g, "m/acceptance/oam_dma/sub/x.gb"));
        assert!(!m(g, "m/acceptance/ppu/x.gb"));
        assert!(!m(g, "m/acceptance/x.gb"));
        assert!(m("a/{b,c/d}/*.gb", "a/c/d/x.gb"));
        assert!(m("a/{x{1,2},y}.gb", "a/x2.gb"));
        assert!(m("a/{x{1,2},y}.gb", "a/y.gb"));
        assert!(!m("a/{x{1,2},y}.gb", "a/x.gb"));
        assert!(m("a/x{,-ds}.gb", "a/x-ds.gb"));
        assert!(m("a/x{,-ds}.gb", "a/x.gb"));
    }

    #[test]
    fn other_characters_are_literal() {
        assert!(m("a/[x]?.gb", "a/[x]?.gb"));
        assert!(!m("a/[x]?.gb", "a/x1.gb"));
    }

    #[test]
    fn literal_paths() {
        let g = Glob::new("a/{b,c}.gb").unwrap();
        assert_eq!(g.literal_paths().unwrap(), ["a/b.gb", "a/c.gb"]);
        assert_eq!(Glob::new("a/*.gb").unwrap().literal_paths(), None);
    }

    #[test]
    fn rejects_bad_patterns() {
        for p in [
            "",
            "/abs/x.gb",
            "a//b",
            "a/../b",
            "./a",
            "a/{b",
            "a/b}",
            "a/{b,c",
            "x/",
        ] {
            assert!(Glob::new(p).is_err(), "{p:?} should be rejected");
        }
    }
}
