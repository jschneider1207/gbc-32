//! The build scripts' literal memory map (Q11): `crates/gbc-nucleo-h753/
//! build.sh` and `tools/h7-bench/build.sh` check image addresses against
//! `DTCM_LO`..`AXI_HI` shell literals, and the board build.sh against the
//! fault-stack and guard sizes; this keeps them equal to what memory-x
//! generates.

use memory_x::{Chip, Data, FAULT_STACK_BYTES, STACK_GUARD_BYTES, memory_x};

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

/// `(origin, end)` of the region line starting with `name` (`ORIGIN = 0x..,
/// LENGTH = <n>K`).
fn region(text: &str, name: &str) -> (u64, u64) {
    let line = text
        .lines()
        .find(|l| l.trim_start().starts_with(name))
        .unwrap_or_else(|| panic!("no {name} region in\n{text}"));
    let num = |key: &str| {
        let rest = &line[line.find(key).unwrap() + key.len()..];
        rest.split([',', ' ']).next().unwrap().to_string()
    };
    let origin = u64::from_str_radix(num("ORIGIN = 0x").as_str(), 16).unwrap();
    let len = num("LENGTH = ");
    let len: u64 = len.strip_suffix('K').unwrap().parse::<u64>().unwrap() * 1024;
    (origin, origin + len)
}

/// The value of the first `NAME=<value>` assignment in `script`.
fn var(script: &str, text: &str, name: &str) -> u64 {
    let key = format!("{name}=");
    let i = text
        .match_indices(&key)
        .map(|(i, _)| i)
        .find(|&i| i == 0 || text[..i].ends_with([' ', '\n']))
        .unwrap_or_else(|| panic!("{script}: no {key}"));
    let v = text[i + key.len()..].split([' ', '\n']).next().unwrap();
    match v.strip_prefix("0x") {
        Some(h) => u64::from_str_radix(h, 16).unwrap(),
        None => v.parse().unwrap(),
    }
}

#[test]
fn build_scripts_match_the_generated_map() {
    // The DTCM layout names both regions in full: DTCM as RAM, all of AXI
    // SRAM as AXI.
    let mx = memory_x(Chip::H753, Data::Dtcm, None);
    let (dtcm, axi) = (region(&mx, "RAM "), region(&mx, "AXI "));
    for script in ["crates/gbc-nucleo-h753/build.sh", "tools/h7-bench/build.sh"] {
        let text = std::fs::read_to_string(format!("{ROOT}/{script}")).unwrap();
        let got = (
            (var(script, &text, "DTCM_LO"), var(script, &text, "DTCM_HI")),
            (var(script, &text, "AXI_LO"), var(script, &text, "AXI_HI")),
        );
        assert_eq!(got, (dtcm, axi), "{script}: (DTCM, AXI) ranges");
    }
    let script = "crates/gbc-nucleo-h753/build.sh";
    let text = std::fs::read_to_string(format!("{ROOT}/{script}")).unwrap();
    assert_eq!(
        var(script, &text, "FAULT_STACK_BYTES"),
        u64::from(FAULT_STACK_BYTES)
    );
    assert_eq!(
        var(script, &text, "STACK_GUARD_BYTES"),
        u64::from(STACK_GUARD_BYTES)
    );
}
