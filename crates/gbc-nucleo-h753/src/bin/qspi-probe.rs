//! The `qspi-probe` image (plan Stage 2 M2): first contact with the
//! shield's QSPI NOR, read commands only. Every 2 s it prints, over the
//! UART, a report of what the NOR answers in 1-line SPI mode:
//!
//! - `jedec`: the JEDEC ID (0x9F, 3 bytes) at a slow clock (9.8 MHz) and
//!   at the first-contact clock (33.1 MHz, [`qspi::PRESCALER_SAFE`]), and
//!   whether the two agree;
//! - `rems`: manufacturer and device ID (0x90, address 0, 2 bytes);
//! - `sfdp`: the SFDP header and parameter headers (0x5A, 3-byte address,
//!   8 dummy cycles, JESD216), then each parameter table's dwords (`ptab`
//!   lines), the basic flash parameter table (ID 0xFF00) first;
//! - `status`: status register 1 (0x05), and the further status or
//!   configuration registers of the manufacturer the JEDEC ID names (a
//!   command is sent only to a manufacturer whose datasheet family defines
//!   it as a register read: 0x35 is a register read on Winbond and
//!   GigaDevice parts but "enter QPI" on Macronix and ISSI ones).
//!
//! Nothing is written to the NOR. Every line starts with
//! `gbc-nucleo-h753`; the banner every fourth report.
#![no_std]
#![no_main]

use core::fmt::Write;

use gbc_nucleo_h753::qspi::{self, Qspi, Read};
use gbc_nucleo_h753::{self as board, CPU_HZ, Uart, cycles, led, wait_since};

const PREFIX: &str = "gbc-nucleo-h753";
const IMAGE: &str = "qspi-probe";
/// 198.4 / 24 = 8.27 MHz: the slow clock the ID is cross-checked at.
const PRESCALER_SLOW: u8 = 23;
/// JESD216: the SFDP read, 3-byte address, 8 dummy cycles.
const SFDP: Read = Read::single(0x5A).dummy(8);
/// Parameter headers printed at most (the header's NPH is a count - 1).
const MAX_HEADERS: usize = 8;
/// Parameter table bytes read at most (64 dwords).
const MAX_TABLE: usize = 256;

/// `bytes` as hex, two digits each, no separator.
fn hex(out: &mut Uart, bytes: &[u8]) {
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
}

/// One register read: `name=<hex>` or `name=err:<error>`.
fn reg(out: &mut Uart, q: &mut Qspi, name: &str, cmd: Read, buf: &mut [u8]) {
    let _ = write!(out, " {name}=");
    match q.read(&cmd, buf) {
        Ok(()) => hex(out, buf),
        Err(e) => {
            let _ = write!(out, "err:{e:?}");
        }
    }
}

/// The further status/configuration register reads for a manufacturer
/// (JEDEC ID byte 0, JEP106 bank 1), as (name, instruction, bytes). Only
/// commands the manufacturer's serial NOR families define as reads.
fn status_reads(mfr: u8) -> &'static [(&'static str, u8, usize)] {
    match mfr {
        // Macronix MX25L/MX25R: RDCR (0x15, configuration register; 2
        // bytes on some parts), RDSCUR (0x2B, security register).
        0xC2 => &[("rdcr_15", 0x15, 2), ("rdscur_2b", 0x2B, 1)],
        // Winbond W25Q, GigaDevice GD25Q: status registers 2 and 3.
        0xEF | 0xC8 => &[("sr2_35", 0x35, 1), ("sr3_15", 0x15, 1)],
        // Micron MT25Q/N25Q: flag status, nonvolatile and volatile
        // configuration.
        0x20 => &[
            ("fsr_70", 0x70, 1),
            ("nvcr_b5", 0xB5, 2),
            ("vcr_85", 0x85, 1),
        ],
        // Cypress/Infineon S25FL: status register 2, configuration register.
        0x01 | 0x34 => &[("sr2_07", 0x07, 1), ("cr1_35", 0x35, 1)],
        // ISSI IS25LP/WP: function register, read parameters.
        0x9D => &[("rdfr_48", 0x48, 1), ("rdrp_61", 0x61, 1)],
        _ => &[],
    }
}

/// One full report, numbered `n`.
fn report(out: &mut Uart, q: &mut Qspi, n: u32) {
    let _ = writeln!(
        out,
        "{PREFIX} qspi report n={n} ker_hz={} slow_prescaler={PRESCALER_SLOW} \
         safe_prescaler={} safe_clk_hz={}\r",
        qspi::KER_HZ,
        qspi::PRESCALER_SAFE,
        qspi::KER_HZ / (u32::from(qspi::PRESCALER_SAFE) + 1),
    );

    // JEDEC ID at both clocks.
    let mut slow = [0u8; 3];
    let mut fast = [0u8; 3];
    let rs = q
        .set_prescaler(PRESCALER_SLOW)
        .and_then(|()| q.read(&Read::single(0x9F), &mut slow));
    let rf = q
        .set_prescaler(qspi::PRESCALER_SAFE)
        .and_then(|()| q.read(&Read::single(0x9F), &mut fast));
    let _ = write!(out, "{PREFIX} qspi jedec slow=");
    match rs {
        Ok(()) => hex(out, &slow),
        Err(e) => {
            let _ = write!(out, "err:{e:?}");
        }
    }
    let _ = write!(out, " fast=");
    match rf {
        Ok(()) => hex(out, &fast),
        Err(e) => {
            let _ = write!(out, "err:{e:?}");
        }
    }
    let _ = writeln!(
        out,
        " match={} clk_hz={}\r",
        u8::from(rs.is_ok() && rf.is_ok() && slow == fast),
        q.clock_hz()
    );

    let _ = write!(out, "{PREFIX} qspi rems");
    reg(out, q, "id_90", Read::single(0x90).at(0), &mut [0; 2]);
    let _ = writeln!(out, "\r");

    // SFDP: the 8-byte header, then the parameter headers.
    let mut hdr = [0u8; 8 + 8 * MAX_HEADERS];
    let _ = write!(out, "{PREFIX} qspi sfdp");
    let nph = match q.read(&SFDP.at(0), &mut hdr[..8]) {
        Ok(()) => (usize::from(hdr[6]) + 1).min(MAX_HEADERS),
        Err(e) => {
            let _ = writeln!(out, " err:{e:?}\r");
            0
        }
    };
    if nph > 0 {
        let _ = write!(
            out,
            " sig_ok={} nph={nph} hdr=",
            u8::from(hdr[..4] == *b"SFDP")
        );
        let r = q.read(&SFDP.at(8), &mut hdr[8..8 + 8 * nph]);
        hex(out, &hdr[..8 + 8 * nph]);
        if let Err(e) = r {
            let _ = write!(out, " err:{e:?}");
        }
        let _ = writeln!(out, "\r");
        if hdr[..4] == *b"SFDP" && r.is_ok() {
            for p in hdr[8..8 + 8 * nph].as_chunks::<8>().0 {
                let id = u16::from(p[7]) << 8 | u16::from(p[0]);
                let len = (usize::from(p[3]) * 4).min(MAX_TABLE);
                let ptr = u32::from(p[4]) | u32::from(p[5]) << 8 | u32::from(p[6]) << 16;
                let _ = write!(
                    out,
                    "{PREFIX} qspi ptab id={id:#06x} rev={}.{} dwords={} ptr={ptr:#08x}",
                    p[2], p[1], p[3]
                );
                let mut t = [0u8; MAX_TABLE];
                if len == 0 {
                    let _ = writeln!(out, "\r");
                    continue;
                }
                match q.read(&SFDP.at(ptr), &mut t[..len]) {
                    Ok(()) => {
                        let _ = write!(out, " d=");
                        for (i, d) in t[..len].as_chunks::<4>().0.iter().enumerate() {
                            let w = u32::from_le_bytes(*d);
                            let _ = write!(out, "{}{w:08x}", if i == 0 { "" } else { "," });
                        }
                        // The density (BFPT dword 2, JESD216): bit 31 clear,
                        // bits = value + 1; set, bits = 2^value.
                        if id == 0xFF00 && len >= 8 {
                            let d2 = u32::from_le_bytes([t[4], t[5], t[6], t[7]]);
                            let bits = if d2 & (1 << 31) == 0 {
                                u64::from(d2) + 1
                            } else {
                                1u64 << (d2 & 0x7FFF_FFFF).min(63)
                            };
                            let _ = write!(out, " density_bits={bits}");
                        }
                    }
                    Err(e) => {
                        let _ = write!(out, " err:{e:?}");
                    }
                }
                let _ = writeln!(out, "\r");
            }
        }
    }

    // Status registers: 0x05 for every part, the rest by manufacturer,
    // only when both clocks read the same JEDEC ID.
    let _ = write!(out, "{PREFIX} qspi status");
    reg(out, q, "sr1_05", Read::single(0x05), &mut [0; 1]);
    if rs.is_ok() && rf.is_ok() && slow == fast {
        for &(name, ins, len) in status_reads(fast[0]) {
            reg(out, q, name, Read::single(ins), &mut [0; 2][..len]);
        }
    }
    let _ = writeln!(out, "\r");
    let _ = writeln!(out, "{PREFIX} qspi end n={n}\r");
}

#[cortex_m_rt::entry]
fn main() -> ! {
    let mut board = board::init(PREFIX);
    let mut q = Qspi::init(qspi::PRESCALER_SAFE);
    let mut n = 0u32;
    loop {
        let t0 = cycles();
        if n.is_multiple_of(4) {
            board.banner(IMAGE);
        }
        report(&mut board.uart, &mut q, n);
        // A report every 2 s, LD1 toggling each second.
        wait_since(t0, CPU_HZ);
        led::toggle();
        wait_since(t0, 2 * CPU_HZ);
        led::toggle();
        n += 1;
    }
}
