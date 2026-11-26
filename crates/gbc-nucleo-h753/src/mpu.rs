//! The MPU: a fixed default map, set up before the caches are turned on.
//!
//! Region 0 covers the whole 4 GB as no-access, execute-never and
//! strongly-ordered, exactly ES0392 section 2.1.2's workaround (a PLD,
//! even one speculated in the shadow of a mispredicted branch, may
//! line-fill from an address that should fault, which matters for memory
//! that never answers on the bus). The
//! regions above it open what this lib uses, and the default memory map
//! is off (PRIVDEFENA = 0), so every other access faults (HardFault: the
//! MemManage fault is not enabled, so it escalates and the report shows
//! CFSR.MMFSR and MMFAR).
//!
//! Every cacheable region is write-back: ES0392 section 2.1.1 (Arm erratum
//! 1259864, all revisions) can corrupt loads from write-through memory,
//! and its workaround is to make such memory write-back through the MPU.
//! The default map's code region (0x0000_0000 to 0x1FFF_FFFF, so the
//! flash) and the QUADSPI and FMC NAND windows are write-through (RM0433
//! Table 7), which is one more reason the default map stays off.
//!
//! Addresses and sizes: RM0433 Rev 9 section 2.3.2, Table 7. Register
//! layout: Armv7-M ARM (DDI 0403) section B3.5, MPU_RBAR (ADDR [31:5],
//! VALID bit 4, REGION [3:0]) and MPU_RASR (XN 28, AP [26:24], TEX [21:19],
//! S 18, C 17, B 16, SRD [15:8], SIZE [5:1] for 2^(SIZE+1) bytes, ENABLE
//! 0); memory types Table B3-13 (TEX, C, B). A region's base is aligned to
//! its size, and a higher region number wins where regions overlap.
//!
//! Region [`REGION_QSPI`] is the QUADSPI window at 0x9000_0000 (Normal,
//! write-back, read-only, execute-never, sized exactly to the 8 MiB NOR;
//! the rest of the 256 MB bank stays region 0's). It is disabled at
//! [`init`] and opened by [`crate::qspi::Qspi::memory_mapped`] only once
//! the QUADSPI is in memory-mapped mode, and closed again before it leaves
//! it (`qspi_window`; the order is in the [`crate::qspi`] docs).
//!
//! Region [`REGION_FMC`] is the FMC bank 1 LCD window at 0x6000_0000
//! (Device, read/write, execute-never, 2 MiB: the command address
//! 0x6000_0000 and the data address 0x6010_0000, A20 = D/CX; the rest of
//! the 256 MB bank stays region 0's). Disabled at [`init`], opened by
//! [`crate::lcd`] once the FMC bank is configured and enabled, closed
//! before it is reconfigured (`fmc_window`).
//!
//! Region [`REGION_DMA_BUF`] makes the LCD transfer buffer (128 KiB of AXI
//! SRAM, [`crate::lcd_dma`]) Normal non-cacheable. It lies inside region 4,
//! so it adds no access, it only changes the attributes there (a higher
//! region number wins). Disabled at [`init`], set once by
//! [`crate::lcd_dma::LcdDma::new`] (`dma_buffer_window`).
//!
//! Region [`REGION_AUDIO_BUF`] does the same for the DAC's circular sample
//! buffer (2 KiB of AXI SRAM, [`crate::audio_dac`]; DMA1 reads it, and
//! DMA1 cannot reach the TCMs, RM0433 Table 3). Disabled at [`init`], set
//! once by [`crate::audio_dac::AudioDac::new`] (`dma_buffer_window`).
//!
//! The MPU stays on in HardFault and NMI (HFNMIENA, see
//! `CTRL_ENABLE_HFNMIENA`).
//!
//! Not mapped yet, so no-access through region 0 (and no speculative
//! access can reach them):
//! - D2 SRAM1-3 (0x3000_0000) and SRAM4 (0x3800_0000), for the first DMA
//!   buffer that needs them (non-cacheable, or cache maintenance around
//!   the transfers).

use cortex_m::peripheral::MPU;

/// The regions of the QUADSPI window (M2) and the FMC LCD window (M3),
/// see the module docs.
pub const REGION_QSPI: u32 = 6;
/// See [`REGION_QSPI`].
pub const REGION_FMC: u32 = 7;
/// The LCD transfer buffer (M3), see the module docs.
pub const REGION_DMA_BUF: u32 = 8;
/// The audio DAC's sample buffer (M4), see the module docs.
pub const REGION_AUDIO_BUF: u32 = 9;

const XN: u32 = 1 << 28;
/// AP: no access at any privilege level.
const AP_NONE: u32 = 0b000 << 24;
/// AP: read/write at any privilege level.
const AP_RW: u32 = 0b011 << 24;
/// AP: read-only at any privilege level.
const AP_RO: u32 = 0b110 << 24;
/// TEX/C/B: strongly-ordered (TEX 000, C 0, B 0).
const STRONGLY_ORDERED: u32 = 0;
/// TEX/C/B/S: shareable Device (TEX 000, C 0, B 1).
const DEVICE: u32 = (1 << 16) | (1 << 18);
/// TEX/C/B: Normal, not cacheable (TEX 001, C 0, B 0).
const NORMAL_NC: u32 = 0b001 << 19;
/// TEX/C/B: Normal, write-back, no write-allocate (TEX 000, C 1, B 1).
const NORMAL_WB: u32 = (1 << 17) | (1 << 16);
/// TEX/C/B: Normal, write-back, write-allocate (TEX 001, C 1, B 1). Not
/// shareable: on the Cortex-M7 shareable Normal memory is not cached
/// unless CACR.SIWT, which would make it write-through (ES0392 2.1.1).
const NORMAL_WBWA: u32 = (0b001 << 19) | (1 << 17) | (1 << 16);

/// MPU_RASR SIZE for a region of `bytes` (a power of two, at least 32).
const fn size(bytes: u64) -> u32 {
    assert!(bytes.is_power_of_two() && bytes >= 32);
    (bytes.trailing_zeros() - 1) << 1
}

/// (base, RASR without ENABLE) for each region, by region number.
const REGIONS: [(u32, u32); 6] = [
    // 0: everything, no access (ES0392 2.1.2).
    (0x0000_0000, size(1 << 32) | XN | AP_NONE | STRONGLY_ORDERED),
    // 1: ITCM, 64 KB. TCM accesses are never cached (DUI 0646 "Cache"),
    // so the attribute only has to be Normal. Executable: the hot set.
    (0x0000_0000, size(64 << 10) | AP_RW | NORMAL_NC),
    // 2: flash, both banks, 2 MB. Read-only: nothing here programs it.
    (0x0800_0000, size(2 << 20) | AP_RO | NORMAL_WB),
    // 3: DTCM, 128 KB: stack, statics, the core's tables.
    (0x2000_0000, size(128 << 10) | XN | AP_RW | NORMAL_NC),
    // 4: AXI SRAM, 512 KB: the `.axisram` buffers and the fault stack.
    (0x2400_0000, size(512 << 10) | XN | AP_RW | NORMAL_WBWA),
    // 5: the peripherals, 0x4000_0000 to 0x5FFF_FFFF (with D3's PWR,
    // RCC, GPIO and DBGMCU at 0x58.. and 0x5C..; RM0433 Table 8).
    (0x4000_0000, size(512 << 20) | XN | AP_RW | DEVICE),
];

/// MPU_CTRL (Armv7-M B3.5.8): ENABLE, HFNMIENA, and PRIVDEFENA = 0.
///
/// HFNMIENA = 1 (M1 review F-4): the MPU stays on in the HardFault and
/// NMI handlers. With HFNMIENA = 0 they would run under the default memory
/// map, where the FMC banks (0x6000_0000, the LCD from M3) and the QUADSPI
/// bank (0x9000_0000) are Normal memory (RM0433 Table 7), so a speculative
/// read there (ES0392 2.1.2) could strobe the LCD or hit the QUADSPI
/// outside memory-mapped mode. Under the MPU map they stay no-access. The
/// fault path only needs what the map already opens: its code and
/// `.rodata` in flash (region 2) or ITCM (region 1), its stack
/// (`.fault_stack`, AXI SRAM, region 4), the prefix static in DTCM
/// (region 3), the exception frame (DTCM or AXI SRAM, read only after a
/// range check) and RCC, GPIOB, GPIOD and USART3 (region 5). A fault
/// before [`init`] runs with the MPU off, where HFNMIENA has no effect.
/// The cost: a fault inside the HardFault handler itself (say the handler
/// touching an unmapped address) locks the core up instead of running on
/// the default map; the handler touches nothing outside the list above.
const CTRL_ENABLE_HFNMIENA: u32 = 0b011;

/// Programs [`REGIONS`], disables every other region, and enables the MPU.
/// Call before the caches are enabled, so no line is ever filled under the
/// default map's attributes.
pub(crate) fn init(mpu: &mut MPU) {
    cortex_m::asm::dmb();
    // MPU_TYPE.DREGION: 16 on the H753's Cortex-M7.
    let regions = (mpu._type.read() >> 8) & 0xFF;
    // SAFETY: MPU_CTRL, MPU_RBAR and MPU_RASR are written as Armv7-M
    // B3.5 lays them out. With the MPU off while the regions are written
    // (CTRL = 0), no access in between sees a half-built map; the regions
    // cover the flash this code runs from, the DTCM stack and every
    // peripheral this lib touches, so nothing it accesses afterwards
    // faults.
    unsafe {
        mpu.ctrl.write(0);
        for n in 0..regions {
            // VALID (bit 4) selects region `n` through RBAR's REGION field.
            match REGIONS.get(n as usize) {
                Some(&(base, rasr)) => {
                    mpu.rbar.write(base | (1 << 4) | n);
                    mpu.rasr.write(rasr | 1);
                }
                // Not in the table (the QSPI window until `qspi_window`
                // opens it, the FMC one): disabled, in case a debugger or
                // an earlier image left it enabled.
                None => {
                    mpu.rbar.write((1 << 4) | n);
                    mpu.rasr.write(0);
                }
            }
        }
        mpu.ctrl.write(CTRL_ENABLE_HFNMIENA);
    }
    // Armv7-M B3.5.10 (MPU_CTRL): a DSB and an ISB so the new map applies
    // to the next access and the next instruction fetch.
    cortex_m::asm::dsb();
    cortex_m::asm::isb();
}

/// What [`qspi_window`] maps at 0x9000_0000.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Window {
    /// Region [`REGION_QSPI`] disabled: region 0's no-access applies.
    Off,
    /// Normal, write-back cached (the ROM's mapping).
    Cached,
    /// Normal, not cacheable (for measuring one bus access).
    Uncached,
}

/// The QUADSPI window: 0x9000_0000, exactly the 8 MiB NOR
/// ([`crate::qspi::NOR_BYTES`]), read-only at both privilege levels,
/// execute-never (nothing runs from the ROM image; and XN keeps the
/// I-side from fetching there speculatively). Normal memory, so unaligned
/// loads work (Armv7-M A3.2.1; Device and strongly-ordered memory fault
/// on them) and the core can read it as a slice. Write-back, no
/// write-allocate (TEX 000 C 1 B 1): read-only, so the allocation policy
/// never matters, and write-back is what ES0392 2.1.1 asks for. Not
/// shareable (shareable Normal is not cached on the Cortex-M7).
///
/// The other 248 MB of the QUADSPI bank (0x9080_0000 to 0x9FFF_FFFF) stay
/// region 0's (no-access, execute-never, strongly-ordered; a higher region
/// number wins only where it overlaps), so no access, speculative or
/// not, reaches the QUADSPI outside the NOR. That is the layout ST gives
/// for the Cortex-M7 (an ST moderator on community.st.com, quoting AN4861
/// section 4.6: the unused part of the 256 MB space "must be set to
/// strongly ordered XN"; **unverified** against AN4861 or AN4760 itself,
/// neither is local). ST's AN4760 example makes the mapped part strongly
/// ordered too; that is not an option here, as the core needs unaligned
/// reads.
const QSPI_RASR: u32 = size(crate::qspi::NOR_BYTES as u64) | XN | AP_RO;
const QSPI_BASE: u32 = crate::qspi::WINDOW as u32;
const _: () = assert!(QSPI_BASE.is_multiple_of(crate::qspi::NOR_BYTES));

/// Opens, re-attributes or closes the QUADSPI window on [`REGION_QSPI`].
/// Only [`crate::qspi`] calls it: `Cached` or `Uncached` only while the
/// QUADSPI is in memory-mapped mode, `Off` before it leaves it.
pub(crate) fn qspi_window(w: Window) {
    let rasr = match w {
        Window::Off => 0,
        Window::Cached => QSPI_RASR | NORMAL_WB | 1,
        Window::Uncached => QSPI_RASR | NORMAL_NC | 1,
    };
    // Every access before the change completes under the old map.
    cortex_m::asm::dsb();
    // SAFETY: RBAR (with VALID and the region number) and RASR of one
    // region, written as Armv7-M B3.5 lays them out; the MPU may be
    // updated while enabled (B3.5.4). The region only adds access to the
    // window, or removes it, and the caller ensures the QUADSPI answers
    // there whenever it is open. One thread, no interrupt: the two writes
    // are not interleaved with another update.
    unsafe {
        let mpu = &*MPU::PTR;
        mpu.rbar.write(QSPI_BASE | (1 << 4) | REGION_QSPI);
        mpu.rasr.write(rasr);
    }
    // B3.5.10: the new attributes apply from the next access and the next
    // instruction fetch.
    cortex_m::asm::dsb();
    cortex_m::asm::isb();
}

/// The FMC LCD window: 0x6000_0000, 2 MiB ([`crate::lcd::WINDOW`],
/// [`crate::lcd::WINDOW_BYTES`]), read/write, execute-never, Device (TEX
/// 000 C 0 B 1; RM0433 section 22.6: the FMC banks are Normal memory in
/// the default map, which allows speculative reads, merged writes and
/// reordering, none of which an 8080 command/data register tolerates).
/// Device keeps the accesses in program order and at their size, and
/// never speculates; it lets the write buffer post writes, which
/// strongly-ordered would not (Armv7-M A3.5.5 and A3.7). Unaligned
/// accesses to Device memory fault (A3.2.1), which also keeps clear of
/// ES0392 2.6.4 (unaligned FMC reads unsupported).
const FMC_RASR: u32 = size(crate::lcd::WINDOW_BYTES as u64) | XN | AP_RW | DEVICE;
const FMC_BASE: u32 = crate::lcd::WINDOW as u32;
const _: () = assert!(FMC_BASE.is_multiple_of(crate::lcd::WINDOW_BYTES));

/// Opens or closes the FMC LCD window on [`REGION_FMC`]. Only
/// [`crate::lcd`] calls it: open only while FMC bank 1 is enabled and
/// configured (an access to a disabled bank is an AXI error, RM0433
/// 22.7.6 FMC_BCRx MBKEN).
pub(crate) fn fmc_window(open: bool) {
    let rasr = if open { FMC_RASR | 1 } else { 0 };
    // Every access before the change completes under the old map.
    cortex_m::asm::dsb();
    // SAFETY: as in `qspi_window`: one region's RBAR and RASR, written as
    // Armv7-M B3.5 lays them out, while the MPU is enabled (B3.5.4); the
    // region only adds or removes access to the window, and the caller
    // ensures the FMC answers there whenever it is open. One thread, no
    // interrupt.
    unsafe {
        let mpu = &*MPU::PTR;
        mpu.rbar.write(FMC_BASE | (1 << 4) | REGION_FMC);
        mpu.rasr.write(rasr);
    }
    cortex_m::asm::dsb();
    cortex_m::asm::isb();
}

/// Whether the MPU is enabled (MPU_CTRL.ENABLE).
pub fn enabled() -> bool {
    // SAFETY: a read of MPU_CTRL, a core register.
    unsafe { (*MPU::PTR).ctrl.read() & 1 != 0 }
}

/// Makes `bytes` (a power of two, at least 32) at `base` (aligned to
/// `bytes`, inside AXI SRAM) Normal, not cacheable, read/write,
/// execute-never on `region` ([`REGION_DMA_BUF`] for the LCD buffer,
/// [`REGION_AUDIO_BUF`] for the DAC's): the buffer a DMA reads while the
/// CPU writes it, so no cache maintenance is needed per frame (plan Stage 2
/// M3: the 100 KB of writes also do not evict the core's D-cache lines) or
/// per audio refill.
/// Region 4 (AXI SRAM, write-back) already allows the access; this region
/// only overrides the attributes.
///
/// Armv7-M (B3.5, and the Cortex-M7 TRM "MPU"): a cache line allocated
/// under the old attributes must not survive the change. The range is
/// cleaned and invalidated before the region is enabled (any line the CPU
/// or a speculative read allocated) and again after it (a line a
/// speculative read could have allocated in between: clean, since nothing
/// writes the buffer before this returns).
///
/// The buffer is then zeroed with 64-bit stores (M3 review P-7: first writes
/// to uninitialised 64-bit-ECC SRAM must be whole words).
pub(crate) fn dma_buffer_window(region: u32, base: usize, bytes: usize) {
    assert!(region == REGION_DMA_BUF || region == REGION_AUDIO_BUF);
    assert!(bytes.is_power_of_two() && bytes >= 32 && base.is_multiple_of(bytes));
    assert!(
        base >= 0x2400_0000 && base + bytes <= 0x2408_0000,
        "not in AXI SRAM"
    );
    // SAFETY: SCB is a core peripheral; the by-address cache maintenance
    // operations only write the maintenance registers (DCCIMVAC), and no
    // other code runs cache maintenance concurrently (one thread; the
    // ISRs, `lcd_dma::on_te` and `audio_dac::on_dma`, do none).
    let scb = unsafe { &mut cortex_m::Peripherals::steal().SCB };
    scb.clean_invalidate_dcache_by_address(base, bytes);
    let rasr = size(bytes as u64) | XN | AP_RW | NORMAL_NC | 1;
    cortex_m::asm::dsb();
    // SAFETY: as in `qspi_window`: one region's RBAR and RASR, written as
    // Armv7-M B3.5 lays them out, while the MPU is enabled (B3.5.4). The
    // region covers memory region 4 already opens, so it grants no new
    // access. Called before the TE or the DMA interrupt is enabled
    // (`LcdDma::new`, `AudioDac::new`), from the main thread, so no other
    // MPU update interleaves with the two writes.
    unsafe {
        let mpu = &*MPU::PTR;
        mpu.rbar.write(base as u32 | (1 << 4) | region);
        mpu.rasr.write(rasr);
    }
    cortex_m::asm::dsb();
    cortex_m::asm::isb();
    scb.clean_invalidate_dcache_by_address(base, bytes);
    // M3 review P-7: AXI SRAM is 64-bit ECC (RM0433 3.2) and not zeroed at
    // boot (NOLOAD), so the first sub-64-bit store to an uninitialised word
    // is a read-modify-write of a word with a bad ECC. Write every word once
    // with 64-bit stores before any 32-bit access (the buffer is not cached
    // now: the stores go straight to the SRAM; `strd` on Armv7-M).
    // An explicit `strd` (a compiler is free to split a `u64` store).
    let mut p = base;
    for _ in 0..bytes / 8 {
        // SAFETY: `p` stays inside the region just made non-cacheable (AXI
        // SRAM, 8-byte aligned: `base` is aligned to `bytes`); nothing else
        // references the buffer yet (the callers are the singletons'
        // `new`). The asm stores two zero registers and advances `p`.
        unsafe {
            core::arch::asm!(
                "strd {z}, {z}, [{p}], #8",
                z = in(reg) 0u32,
                p = inout(reg) p,
                options(nostack, preserves_flags),
            );
        }
    }
    cortex_m::asm::dsb();
}

/// Region `n`'s (MPU_RBAR, MPU_RASR) as they read now (for a report).
pub fn region(n: u32) -> (u32, u32) {
    cortex_m::interrupt::free(|_| {
        // SAFETY: MPU_RNR selects the region the RBAR/RASR reads return;
        // with interrupts off no other code changes RNR in between (the
        // region writes in this module go through RBAR's VALID field,
        // which also sets RNR, and run in the main thread only).
        unsafe {
            let mpu = &*MPU::PTR;
            mpu.rnr.write(n);
            (mpu.rbar.read(), mpu.rasr.read())
        }
    })
}
