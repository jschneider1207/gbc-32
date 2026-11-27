//! The link's default rate (Q11): one Rust constant, and the shell scripts
//! that cannot import it carry the same literal; `parse_baud`.

use gbc_link::{DEFAULT_BAUD, parse_baud};

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

/// Every `NAME=${VAR:-<default>}` baud default in the scripts, and the
/// usage texts that print it, equal [`DEFAULT_BAUD`].
#[test]
fn scripts_and_usage_carry_the_default() {
    let want = DEFAULT_BAUD.to_string();
    for (path, prefix, end) in [
        ("scripts/play-on-board.sh", "baud=${PLAY_BAUD:-", "}"),
        (
            "crates/gbc-nucleo-h753/run-loader-roundtrip.sh",
            "baud=${RUN_LOADER_BAUD:-",
            "}",
        ),
        (
            "crates/gbc-nucleo-h753/run-save-roundtrip.sh",
            "baud=${RUN_SAVE_BAUD:-",
            "}",
        ),
        (
            "crates/gbc-nucleo-h753/run-player-save.sh",
            "baud=${RUN_SAVE_BAUD:-",
            "}",
        ),
        (
            "crates/gbc-nucleo-h753/run-replay.sh",
            "load \"$rom\" --baud ",
            " ",
        ),
        ("scripts/gbc-pc.sh", "[--baud ", "]"),
        ("tools/gbc-pc/src/main.rs", "[--baud ", "]"),
    ] {
        let text = std::fs::read_to_string(format!("{ROOT}/{path}")).unwrap();
        let found: Vec<&str> = text
            .match_indices(prefix)
            .map(|(i, _)| {
                let rest = &text[i + prefix.len()..];
                &rest[..rest.find(end).unwrap()]
            })
            // `[--baud N]` in a usage line is a placeholder, not a default.
            .filter(|f| *f != "N")
            .collect();
        assert!(!found.is_empty(), "{path}: no {prefix:?}");
        for f in found {
            assert_eq!(f, want, "{path}: {prefix}{f}{end}, want {want}");
        }
    }
}

#[test]
fn parse_baud_defaults_and_parses() {
    assert_eq!(parse_baud(None, DEFAULT_BAUD), 2_000_000);
    assert_eq!(parse_baud(Some("115200"), DEFAULT_BAUD), 115_200);
    assert_eq!(parse_baud(Some("0"), 7), 0);
    const C: u32 = parse_baud(Some("4000000"), 1);
    assert_eq!(C, 4_000_000);
}

#[test]
#[should_panic(expected = "not a number")]
fn parse_baud_refuses_a_non_number() {
    let _ = parse_baud(Some("2M"), DEFAULT_BAUD);
}

#[test]
#[should_panic(expected = "empty")]
fn parse_baud_refuses_empty() {
    let _ = parse_baud(Some(""), DEFAULT_BAUD);
}
