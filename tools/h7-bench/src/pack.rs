//! The bench pack: every workload ROM in one blob, so the `itcm-dtcm-qspi`
//! measurement (plan Stage 2 M2) can read them all from the QSPI NOR, which
//! holds one ADR 0037 image. To the loader and to `rom_image` the pack is
//! just that image's data (its length and FNV-1a 64 hash are the header's,
//! checked by `rom_image::validate` at boot); this file is its inner layout,
//! shared by the firmware (`main.rs`, which parses it) and the host command
//! that writes it (`host/src/main.rs`, `pack <file>`), like `workload.rs`.
//!
//! Layout (little-endian; README.md "QSPI ROM placement"):
//!
//! ```text
//! 0     8 B   MAGIC "H7BPACK1"
//! 8     u32   entry count, = WORKLOADS.len()
//! 12    u32   0
//! 16    ENTRY_BYTES per entry, in WORKLOADS order:
//!         24 B  workload name, NUL-padded
//!         u32   ROM offset from the start of the pack, a multiple of ALIGN
//!         u32   ROM length, > 0
//! ...   the ROMs, in entry order, each at its offset, 0xFF between them
//! ```
//!
//! The parser accepts only this exact shape: the names in report order,
//! the ROMs in order without overlap, after the table, inside the pack.

use crate::workload::WORKLOADS;

pub const MAGIC: [u8; 8] = *b"H7BPACK1";
/// Bytes of the fixed part before the entries.
pub const FIXED_BYTES: usize = 16;
/// Bytes of one entry.
pub const ENTRY_BYTES: usize = 32;
/// Bytes of an entry's name field.
pub const NAME_BYTES: usize = 24;
/// Every ROM starts at a multiple of this (one NOR page; also a whole
/// number of 32-byte cache lines).
pub const ALIGN: usize = 256;
/// Where the ROMs may start: the end of the table.
pub const TABLE_BYTES: usize = FIXED_BYTES + ENTRY_BYTES * WORKLOADS.len();

/// Why a pack was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackError {
    /// Shorter than its table.
    Short,
    BadMagic,
    /// The entry count is not the number of workloads.
    Count,
    /// Entry `n`'s name is not workload `n`'s.
    Name(usize),
    /// Entry `n`'s offset is not a multiple of [`ALIGN`], is inside the
    /// table or overlaps the previous ROM.
    Offset(usize),
    /// Entry `n`'s ROM is empty or runs past the end of the pack.
    Length(usize),
}

fn u32_at(data: &[u8], at: usize) -> usize {
    let mut w = [0u8; 4];
    w.copy_from_slice(&data[at..at + 4]);
    u32::from_le_bytes(w) as usize
}

/// The name field for `name`: the bytes, NUL-padded. `None` if it does
/// not fit.
pub fn name_field(name: &str) -> Option<[u8; NAME_BYTES]> {
    let b = name.as_bytes();
    let mut f = [0u8; NAME_BYTES];
    f.get_mut(..b.len())?.copy_from_slice(b);
    Some(f)
}

/// The ROMs of a pack, in [`WORKLOADS`] order.
pub fn parse(data: &[u8]) -> Result<[&[u8]; WORKLOADS.len()], PackError> {
    if data.len() < TABLE_BYTES {
        return Err(PackError::Short);
    }
    if data[..8] != MAGIC {
        return Err(PackError::BadMagic);
    }
    if u32_at(data, 8) != WORKLOADS.len() || u32_at(data, 12) != 0 {
        return Err(PackError::Count);
    }
    let mut roms: [&[u8]; WORKLOADS.len()] = [&[]; WORKLOADS.len()];
    // The first byte a ROM may use.
    let mut free = TABLE_BYTES;
    for (i, w) in WORKLOADS.iter().enumerate() {
        let e = FIXED_BYTES + i * ENTRY_BYTES;
        if name_field(w.name).is_none_or(|n| data[e..e + NAME_BYTES] != n) {
            return Err(PackError::Name(i));
        }
        let (off, len) = (
            u32_at(data, e + NAME_BYTES),
            u32_at(data, e + NAME_BYTES + 4),
        );
        if !off.is_multiple_of(ALIGN) || off < free {
            return Err(PackError::Offset(i));
        }
        let end = off.checked_add(len).ok_or(PackError::Length(i))?;
        if len == 0 || end > data.len() {
            return Err(PackError::Length(i));
        }
        roms[i] = &data[off..end];
        free = end;
    }
    Ok(roms)
}
