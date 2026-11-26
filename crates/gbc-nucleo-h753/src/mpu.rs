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
//! Not mapped yet, so no-access through region 0 (and no speculative
//! access can reach them):
//! - M2: the QUADSPI window at 0x9000_0000 (Normal, write-back, sized
//!   exactly to the 8 MiB NOR; the rest of the 256 MB window stays
//!   region 0's) on [`REGION_QSPI`];
//! - M3: the FMC bank 1 LCD window at 0x6000_0000 (Device or
//!   strongly-ordered, so the command and data writes are neither cached,
//!   merged nor reordered) on [`REGION_FMC`];
//! - D2 SRAM1-3 (0x3000_0000) and SRAM4 (0x3800_0000), for the first DMA
//!   buffer that needs them (non-cacheable, or cache maintenance around
//!   the transfers).

use cortex_m::peripheral::MPU;

/// The region numbers M2 and M3 will use (see the module docs).
pub const REGION_QSPI: u32 = 6;
/// See [`REGION_QSPI`].
pub const REGION_FMC: u32 = 7;

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

/// MPU_CTRL: ENABLE, with HFNMIENA = 0 (the MPU is off in the HardFault
/// handler, which then sees the default map, whatever faulted) and
/// PRIVDEFENA = 0.
const CTRL_ENABLE: u32 = 1;

/// Programs [`REGIONS`] and enables the MPU. Call before the caches are
/// enabled, so no line is ever filled under the default map's attributes.
pub(crate) fn init(mpu: &mut MPU) {
    cortex_m::asm::dmb();
    // SAFETY: MPU_CTRL, MPU_RBAR and MPU_RASR are written as Armv7-M
    // B3.5 lays them out. With the MPU off while the regions are written
    // (CTRL = 0), no access in between sees a half-built map; the regions
    // cover the flash this code runs from, the DTCM stack and every
    // peripheral this lib touches, so nothing it accesses afterwards
    // faults.
    unsafe {
        mpu.ctrl.write(0);
        for (n, &(base, rasr)) in REGIONS.iter().enumerate() {
            // VALID (bit 4) selects region `n` through RBAR's REGION field.
            mpu.rbar.write(base | (1 << 4) | n as u32);
            mpu.rasr.write(rasr | 1);
        }
        mpu.ctrl.write(CTRL_ENABLE);
    }
    // Armv7-M B3.5.10 (MPU_CTRL): a DSB and an ISB so the new map applies
    // to the next access and the next instruction fetch.
    cortex_m::asm::dsb();
    cortex_m::asm::isb();
}

/// Whether the MPU is enabled (MPU_CTRL.ENABLE).
pub fn enabled() -> bool {
    // SAFETY: a read of MPU_CTRL, a core register.
    unsafe { (*MPU::PTR).ctrl.read() & 1 != 0 }
}
