//! The frame transfer to the panel (plan Stage 2 M3): MDMA channel 0 copies
//! the 240x216 output buffer ([`gbc_firmware::video::OutFrame`], in wire
//! byte order) to the FMC data address, started from the TE (FMARK)
//! interrupt so the write runs from the start of V-blank, ahead of the
//! panel's scan. Facts, register values and measurements:
//! `docs/reference/lcd-panel.md` "MDMA and TE"; the scheme: ADR 0039.
//!
//! # Scheme
//!
//! One buffer, an interrupt-started transfer, a polled completion:
//!
//! 1. The main loop asks for the buffer ([`LcdDma::buffer`]), which waits
//!    only for the previous transfer to finish (never for TE), converts
//!    the frame into it and calls [`LcdDma::present`]. That sets the
//!    window, programs and enables the MDMA channel (its registers can only
//!    be written while it is disabled), and marks the transfer pending.
//! 2. TE rises at the start of V-blank (ST7789V 8.15, TEON mode 1, M = 0).
//!    The EXTI15_10 interrupt ([`on_te`]) finds the transfer pending,
//!    sends RAMWR (2Ch; it restarts the write pointer at the window's
//!    start), and sets SWRQ. MDMA then streams the 103,680 bytes with no
//!    CPU involvement.
//! 3. The channel disables itself and sets CTCIF at the end; the main loop
//!    sees it the next time it asks ([`LcdDma::busy`], [`LcdDma::wait`]).
//!
//! For measurements, [`LcdDma::start_now`] starts a queued transfer from
//! the main loop instead of TE (the panel may then tear), and
//! [`LcdDma::wait_started`] waits until TE has taken it. [`readback`] is
//! the RAMRD comparison of the window with the buffer (between transfers).
//!
//! The interrupt and not a polled TE because the main loop must not have
//! to sit on the TE line: with the core running between frames it would
//! miss the edge by up to a frame. The completion is polled because
//! nothing has to happen at that moment; the next conversion just must
//! not start before it. The MDMA interrupt stays off in the NVIC (its
//! flags are set either way, RM0433 14.5.2).
//!
//! The window is rows [`Y0`]..=[`Y1`] (52..=267, centred in 320) of all
//! 240 columns, written in the panel's native scan order (MADCTL 0x08:
//! 240-pixel rows, top to bottom), so the write front moves down the panel
//! in the same direction as the scan and, starting in V-blank and writing
//! a row faster than the panel scans one, stays ahead of it for the whole
//! window (the arithmetic is in the reference note).
//!
//! # MDMA channel 0 (RM0433 Rev 9 chapter 14; register offsets 14.5)
//!
//! - TCR [`TCR`] = 0xE3FC_4222: BWM (bufferable writes), SWRM (software
//!   request), TRGM = 10 (one request runs the whole repeated block), PKE
//!   (word reads packed into byte writes, little-endian: the buffer's
//!   memory byte order is the wire order), TLEN = 127 (128-byte buffer),
//!   SBURST = 100 (16-beat reads), DBURST = 000, SSIZE = SINCOS = word,
//!   SINC = increment, DSIZE = byte, DINC = fixed (the data address).
//! - BNDTR [`BNDTR`] = 0x0010_CA80: BNDT = 51,840 bytes per block (at most
//!   65,536), BRC = 1 (two blocks), BRUR = 0 (the source carries on
//!   where the first block ended, the destination stays).
//! - TBR = 0: both ports on the AXI bus (the FMC is an AXI target; MDMA
//!   reaches AXI SRAM and the FMC through the AXI matrix). LAR = 0: no
//!   linked list.
//! - CR = PL very high | EN, then SWRQ from the interrupt.
//!
//! MDMA accesses do not go through the MPU (it belongs to the CPU), so the
//! FMC window's Device attribute does not apply to them; all MDMA
//! destination writes are non-cacheable (14.5.6 BWM note).
//!
//! # The buffer
//!
//! [`DmaBuffer`]: 128 KiB aligned and sized, in `.axisram` (AXI SRAM;
//! NOLOAD), and MPU region [`crate::mpu::REGION_DMA_BUF`] Normal
//! non-cacheable over exactly it, so the CPU's stores reach the SRAM
//! without cache maintenance and the conversion does not evict the core's
//! cached lines. 103,680 bytes used.
//!
//! # TE (PE13)
//!
//! PE13 input without pull (set up by [`Lcd::init`]); SYSCFG_EXTICR4
//! EXTI13 = 0100 (port E, RM0433 12.3.5), EXTI_RTSR1 bit 13 (rising edge),
//! EXTI_CPUIMR1 bit 13 (unmasked for the CPU), cleared in EXTI_CPUPR1
//! (RM0433 20.6); NVIC interrupt 40, EXTI15_10. The image defines the
//! vector and calls [`on_te`] from it:
//!
//! ```ignore
//! #[interrupt]
//! fn EXTI15_10() {
//!     gbc_nucleo_h753::lcd_dma::on_te();
//! }
//! ```
//!
//! An image that links this lib without using [`LcdDma`] needs no handler:
//! the interrupt is only unmasked by [`LcdDma::new`].

use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};

use gbc_firmware::video::{OUT_BYTES, OUT_H, OUT_W, OutFrame};

use crate::lcd::{self, Lcd};
use crate::{cycles, mpu, pac};

/// The window's first row: (320 - 216) / 2.
pub const Y0: u16 = (lcd::HEIGHT - OUT_H as u16) / 2;
/// The window's last row (inclusive).
pub const Y1: u16 = Y0 + OUT_H as u16 - 1;
const _: () = assert!(OUT_W == lcd::WIDTH as usize && Y0 == 52 && Y1 == 267);

/// The output buffer, aligned to and padded to 128 KiB so that one MPU
/// region covers exactly it (the region's size is a power of two and its
/// base aligned to it; nothing else can share the region).
#[repr(C, align(131072))]
pub struct DmaBuffer(pub OutFrame);
/// See [`DmaBuffer`].
pub const DMA_BUFFER_BYTES: usize = 128 << 10;
const _: () = assert!(core::mem::size_of::<DmaBuffer>() == DMA_BUFFER_BYTES);
const _: () = assert!(core::mem::size_of::<OutFrame>() == OUT_BYTES);

/// Bytes per MDMA block: two blocks make the frame.
const BLOCK_BYTES: u32 = OUT_BYTES as u32 / 2;
const _: () = assert!(BLOCK_BYTES * 2 == OUT_BYTES as u32 && BLOCK_BYTES <= 65_536);
const _: () = assert!(BLOCK_BYTES.is_multiple_of(128)); // whole 128-byte buffers

// MDMA_CxTCR fields (RM0433 14.5.6).
const TCR_BWM: u32 = 1 << 31;
const TCR_SWRM: u32 = 1 << 30;
const TCR_TRGM_REPEATED_BLOCK: u32 = 0b10 << 28;
const TCR_PKE: u32 = 1 << 25;
const TCR_TLEN_SHIFT: u32 = 18;
const TCR_SBURST_SHIFT: u32 = 12;
const TCR_SINCOS_SHIFT: u32 = 8;
const TCR_SSIZE_SHIFT: u32 = 4;
const TCR_SINC_INC: u32 = 0b10;
const SIZE_WORD: u32 = 0b10;
/// The transfer configuration (module docs).
pub const TCR: u32 = TCR_BWM
    | TCR_SWRM
    | TCR_TRGM_REPEATED_BLOCK
    | TCR_PKE
    | 127 << TCR_TLEN_SHIFT
    | 0b100 << TCR_SBURST_SHIFT
    | SIZE_WORD << TCR_SINCOS_SHIFT
    | SIZE_WORD << TCR_SSIZE_SHIFT
    | TCR_SINC_INC;
const _: () = assert!(TCR == 0xE3FC_4222);
/// BRC = 1 (two blocks), BNDT = [`BLOCK_BYTES`] (RM0433 14.5.7).
pub const BNDTR: u32 = 1 << 20 | BLOCK_BYTES;
const _: () = assert!(BNDTR == 0x0010_CA80);
// MDMA_CxCR (14.5.5).
const CR_EN: u32 = 1 << 0;
const CR_PL_VERY_HIGH: u32 = 0b11 << 6;
const CR_SWRQ: u32 = 1 << 16;
// MDMA_CxISR / CxIFCR (14.5.2, 14.5.3).
const ISR_TEIF: u32 = 1 << 0;
const ISR_CTCIF: u32 = 1 << 1;
const IFCR_ALL: u32 = 0x1F;

const EXTI_LINE: u32 = 13;
/// RAMWR.
const RAMWR: u8 = 0x2C;

/// What [`on_te`] and the main loop share.
const IDLE: u8 = 0;
const PENDING: u8 = 1;
const RUNNING: u8 = 2;
static STATE: AtomicU8 = AtomicU8::new(IDLE);
/// Cycle counter at the last TE interrupt's entry.
static TE_AT: AtomicU32 = AtomicU32::new(0);
/// Cycle counter right after the SWRQ write of the running transfer, and
/// at the TE interrupt that started it.
static START_AT: AtomicU32 = AtomicU32::new(0);
static START_TE_AT: AtomicU32 = AtomicU32::new(0);
// TE statistics since `te_reset`: edges, first and last edge, and the
// shortest and longest period, in CPU cycles.
static TE_COUNT: AtomicU32 = AtomicU32::new(0);
static TE_FIRST: AtomicU32 = AtomicU32::new(0);
static TE_MIN: AtomicU32 = AtomicU32::new(u32::MAX);
static TE_MAX: AtomicU32 = AtomicU32::new(0);

fn mdma() -> &'static pac::mdma::CH {
    // SAFETY: a shared reference to the memory-mapped register block
    // (0x5200_0000, RM0433 Table 8), valid for the whole program; channel
    // 0 belongs to this module, and its accesses are volatile register
    // accesses. The main loop writes the configuration only while STATE is
    // IDLE and the interrupt writes CR only after taking STATE from
    // PENDING to RUNNING, so the two never write it at once.
    unsafe { &(*pac::MDMA::ptr()).ch0 }
}

fn exti() -> &'static pac::exti::RegisterBlock {
    // SAFETY: as `mdma`; only line 13's bits are written (RTSR1, FTSR1 and
    // CPUIMR1 by read-modify-write in `new`, CPUPR1 by a one-bit write in
    // the interrupt).
    unsafe { &*pac::EXTI::ptr() }
}

/// What a finished transfer measured, in CPU cycles.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Transfer {
    /// From the TE interrupt's entry to the SWRQ write (the interrupt
    /// latency before the entry, 12 cycles plus the vector fetch, is not
    /// counted).
    pub te_to_start: u32,
    /// From the SWRQ write to the moment the main loop saw CTCIF (exact
    /// when the main loop waits in [`LcdDma::wait`]).
    pub start_to_done: u32,
}

/// A transfer error: CxESR and the channel's registers when it stopped.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    /// TEIF: CxESR (TEA[6:0], TED bit 7, TELD 8, TEMD 9, ASE 10, BSE 11),
    /// then SAR, DAR, BNDTR.
    Transfer {
        esr: u32,
        sar: u32,
        dar: u32,
        bndtr: u32,
    },
    /// [`LcdDma::wait`] gave up: the state (pending: no TE came) and
    /// CxISR, CxCR, BNDTR.
    Timeout {
        pending: bool,
        isr: u32,
        cr: u32,
        bndtr: u32,
    },
}

/// The panel with its transfer. Owns the [`Lcd`]: while a transfer is
/// pending or running nothing else may write the FMC.
pub struct LcdDma {
    lcd: Lcd,
    buf: &'static mut DmaBuffer,
    last: Transfer,
}

impl LcdDma {
    /// Takes the panel, sets up the buffer's MPU region, the MDMA clock and
    /// TE on EXTI13, and enables the EXTI15_10 interrupt (the image must
    /// define it, module docs). Panics on a second call.
    pub fn new(lcd: Lcd) -> LcdDma {
        static TAKEN: AtomicBool = AtomicBool::new(false);
        assert!(
            !TAKEN.swap(true, Ordering::AcqRel),
            "LcdDma::new called twice"
        );
        struct Slot(core::cell::UnsafeCell<DmaBuffer>);
        // SAFETY: reachable only through the `TAKEN` guard above, once.
        unsafe impl Sync for Slot {}
        // SAFETY: `.axisram` is the NOLOAD AXI SRAM section memory.x
        // (memory_x::firmware_memory_x) defines for such buffers; not
        // zeroed at boot, which is sound for `u32` words (every bit pattern
        // is a value). The image converts a frame into it before the first
        // transfer.
        #[unsafe(link_section = ".axisram.lcd_dma")]
        static BUF: Slot = Slot(core::cell::UnsafeCell::new(DmaBuffer(
            [[0; OUT_W / 2]; OUT_H],
        )));
        // SAFETY: the guard lets exactly one caller through, so this is the
        // only reference to BUF; MDMA reads it only between `present` and
        // the completion, while `LcdDma` hands out no reference to it.
        let buf = unsafe { &mut *BUF.0.get() };
        mpu::dma_buffer_window(
            mpu::REGION_DMA_BUF,
            buf as *mut DmaBuffer as usize,
            DMA_BUFFER_BYTES,
        );

        // SAFETY: RCC_AHB3ENR (MDMAEN) and APB4ENR (SYSCFGEN) by
        // read-modify-write, the same single thread as `lcd`'s FMCEN; no
        // interrupt touches RCC.
        let rcc = unsafe { &*pac::RCC::ptr() };
        rcc.ahb3enr.modify(|_, w| w.mdmaen().set_bit());
        rcc.apb4enr.modify(|_, w| w.syscfgen().set_bit());
        let _ = rcc.ahb3enr.read();
        let _ = rcc.apb4enr.read();
        let ch = mdma();
        // A channel left enabled by an earlier image (a reset clears it,
        // but a debugger reset may not reset MDMA): disable and wait.
        // SAFETY: CR's EN cleared, the other bits kept.
        ch.cr.modify(|r, w| unsafe { w.bits(r.bits() & !CR_EN) });
        let mut n = 0u32;
        while ch.cr.read().bits() & CR_EN != 0 && n < 1_000_000 {
            n += 1;
        }
        // SAFETY: the clear-flag register, write-1-to-clear.
        ch.ifcr.write(|w| unsafe { w.bits(IFCR_ALL) });

        // TE: PE13 on EXTI13, rising edge only, unmasked for the CPU.
        // SAFETY: SYSCFG's clock is on (above); EXTICR4 EXTI13 = 4, port E
        // (RM0433 12.3.5); the other lines' fields are kept.
        let syscfg = unsafe { &*pac::SYSCFG::ptr() };
        // SAFETY: as above.
        syscfg
            .exticr4
            .modify(|_, w| unsafe { w.exti13().bits(0b0100) });
        let e = exti();
        // SAFETY: line 13's bits set or cleared, the others kept (no other
        // line is used).
        e.rtsr1
            .modify(|r, w| unsafe { w.bits(r.bits() | 1 << EXTI_LINE) });
        // SAFETY: as above.
        e.ftsr1
            .modify(|r, w| unsafe { w.bits(r.bits() & !(1 << EXTI_LINE)) });
        // SAFETY: write-1-to-clear of line 13's pending bit.
        e.cpupr1.write(|w| unsafe { w.bits(1 << EXTI_LINE) });
        // SAFETY: as for RTSR1.
        e.cpuimr1
            .modify(|r, w| unsafe { w.bits(r.bits() | 1 << EXTI_LINE) });
        te_reset();
        cortex_m::asm::dsb();
        // SAFETY: the handler exists (the image defines EXTI15_10, module
        // docs; without it the lib's DefaultHandler reports the interrupt
        // and stops) and touches only this module's atomics, the command
        // register and channel 0's CR, as `mdma` describes.
        unsafe { cortex_m::peripheral::NVIC::unmask(pac::Interrupt::EXTI15_10) };
        LcdDma {
            lcd,
            buf,
            last: Transfer::default(),
        }
    }

    /// The panel, once no transfer is pending or running (waits for that;
    /// an error is dropped here, [`LcdDma::wait`] reports it).
    pub fn lcd(&mut self) -> &mut Lcd {
        while self.busy() {}
        &mut self.lcd
    }

    /// The buffer, once no transfer is pending or running (waits for
    /// that, as [`LcdDma::lcd`]).
    pub fn buffer(&mut self) -> &mut OutFrame {
        while self.busy() {}
        &mut self.buf.0
    }

    /// The panel and the buffer together (for a readback check), once no
    /// transfer is pending or running.
    pub fn parts(&mut self) -> (&mut Lcd, &mut OutFrame) {
        while self.busy() {}
        (&mut self.lcd, &mut self.buf.0)
    }

    /// The buffer without waiting: `None` while a transfer is pending or
    /// running.
    pub fn try_buffer(&mut self) -> Option<&mut OutFrame> {
        if self.busy() {
            None
        } else {
            Some(&mut self.buf.0)
        }
    }

    /// Queues the buffer for the next TE: the window, then the channel
    /// programmed and enabled, waiting for its software request. Waits
    /// for a previous transfer first.
    pub fn present(&mut self) {
        while self.busy() {}
        self.lcd.set_window(0, Y0, lcd::WIDTH - 1, Y1);
        let ch = mdma();
        let src = self.buf as *const DmaBuffer as u32;
        // SAFETY: the channel is disabled (IDLE: `busy` saw it finish or
        // it never ran), so its configuration registers accept writes
        // (14.5.5 EN); the values are the module docs' (TCR, BNDTR, the
        // buffer's address, word aligned, and the data address).
        unsafe {
            ch.ifcr.write(|w| w.bits(IFCR_ALL));
            ch.tcr.write(|w| w.bits(TCR));
            ch.bndtr.write(|w| w.bits(BNDTR));
            ch.sar.write(|w| w.bits(src));
            ch.dar.write(|w| w.bits(lcd::DATA_ADDR as u32));
            ch.brur.write(|w| w.bits(0));
            ch.lar.write(|w| w.bits(0));
            ch.tbr.write(|w| w.bits(0));
            ch.cr.write(|w| w.bits(CR_PL_VERY_HIGH));
            ch.cr.write(|w| w.bits(CR_PL_VERY_HIGH | CR_EN));
        }
        // The buffer's stores (non-cacheable, possibly still in the write
        // buffer) and the window's commands are complete before the
        // interrupt can start the transfer.
        cortex_m::asm::dsb();
        STATE.store(PENDING, Ordering::Release);
    }

    /// Whether a transfer is pending or running; notes its completion.
    pub fn busy(&mut self) -> bool {
        match self.poll() {
            Poll::Busy => true,
            Poll::Idle | Poll::Done(_) | Poll::Failed(_) => false,
        }
    }

    /// Waits up to `timeout` CPU cycles for the transfer to finish (TE
    /// included); `Ok(None)` if none was queued.
    pub fn wait(&mut self, timeout: u32) -> Result<Option<Transfer>, Error> {
        let t0 = cycles();
        loop {
            match self.poll() {
                Poll::Idle => return Ok(None),
                Poll::Done(t) => return Ok(Some(t)),
                Poll::Failed(e) => return Err(e),
                Poll::Busy => {}
            }
            if cycles().wrapping_sub(t0) > timeout {
                let ch = mdma();
                let pending = STATE.load(Ordering::Acquire) == PENDING;
                return Err(Error::Timeout {
                    pending,
                    isr: ch.isr.read().bits(),
                    cr: ch.cr.read().bits(),
                    bndtr: ch.bndtr.read().bits(),
                });
            }
        }
    }

    /// [`LcdDma::wait`] without waiting: `None` while a transfer is
    /// pending or running.
    pub fn try_wait(&mut self) -> Option<Result<Option<Transfer>, Error>> {
        match self.poll() {
            Poll::Busy => None,
            Poll::Idle => Some(Ok(None)),
            Poll::Done(t) => Some(Ok(Some(t))),
            Poll::Failed(e) => Some(Err(e)),
        }
    }

    /// Waits up to `timeout` CPU cycles until a queued transfer has
    /// started (the TE interrupt took it): true if it has, or if none is
    /// pending.
    pub fn wait_started(&self, timeout: u32) -> bool {
        let t0 = cycles();
        while STATE.load(Ordering::Acquire) == PENDING {
            if cycles().wrapping_sub(t0) > timeout {
                return false;
            }
        }
        true
    }

    /// Starts a queued transfer now, without waiting for TE (a
    /// measurement mode: the write front is then anywhere relative to the
    /// scan, so the panel may tear; the GRAM still gets every byte). No
    /// effect unless a transfer is pending. The TE interrupt cannot start
    /// it a second time: both take the state from PENDING with interrupts
    /// off here.
    pub fn start_now(&mut self) {
        cortex_m::interrupt::free(|_| start_pending(cycles()));
    }

    /// The last finished transfer's figures.
    pub fn last(&self) -> Transfer {
        self.last
    }

    /// Channel 0's (TCR, BNDTR, SAR, DAR, CR, ISR, ESR) as they read now.
    pub fn registers(&self) -> [u32; 7] {
        let ch = mdma();
        [
            ch.tcr.read().bits(),
            ch.bndtr.read().bits(),
            ch.sar.read().bits(),
            ch.dar.read().bits(),
            ch.cr.read().bits(),
            ch.isr.read().bits(),
            ch.esr.read().bits(),
        ]
    }

    fn poll(&mut self) -> Poll {
        match STATE.load(Ordering::Acquire) {
            IDLE => return Poll::Idle,
            PENDING => return Poll::Busy,
            _ => {}
        }
        let ch = mdma();
        let isr = ch.isr.read().bits();
        let now = cycles();
        if isr & ISR_TEIF != 0 {
            let e = Error::Transfer {
                esr: ch.esr.read().bits(),
                sar: ch.sar.read().bits(),
                dar: ch.dar.read().bits(),
                bndtr: ch.bndtr.read().bits(),
            };
            // SAFETY: disable (a transfer error already cleared EN,
            // 14.5.5) and clear the flags, write-1-to-clear.
            unsafe {
                ch.cr.write(|w| w.bits(0));
                ch.ifcr.write(|w| w.bits(IFCR_ALL));
            }
            STATE.store(IDLE, Ordering::Release);
            return Poll::Failed(e);
        }
        if isr & ISR_CTCIF != 0 && ch.cr.read().bits() & CR_EN == 0 {
            // The FMC's write FIFO may still hold the last bytes; the next
            // FMC access (a command) queues behind them, so nothing waits.
            let start = START_AT.load(Ordering::Relaxed);
            self.last = Transfer {
                te_to_start: start.wrapping_sub(START_TE_AT.load(Ordering::Relaxed)),
                start_to_done: now.wrapping_sub(start),
            };
            // SAFETY: write-1-to-clear.
            ch.ifcr.write(|w| unsafe { w.bits(IFCR_ALL) });
            STATE.store(IDLE, Ordering::Release);
            return Poll::Done(self.last);
        }
        Poll::Busy
    }
}

enum Poll {
    Idle,
    Busy,
    Done(Transfer),
    Failed(Error),
}

/// The TE interrupt's body (EXTI15_10; module docs): clears the pending
/// bit, notes the edge, and starts a pending transfer.
#[inline]
pub fn on_te() {
    let now = cycles();
    // SAFETY: write-1-to-clear of line 13's pending bit; the DSB makes the
    // clear reach EXTI before the interrupt returns (no second entry).
    exti().cpupr1.write(|w| unsafe { w.bits(1 << EXTI_LINE) });
    start_pending(now);
    let n = TE_COUNT.load(Ordering::Relaxed);
    if n == 0 {
        TE_FIRST.store(now, Ordering::Relaxed);
    } else {
        let d = now.wrapping_sub(TE_AT.load(Ordering::Relaxed));
        if d < TE_MIN.load(Ordering::Relaxed) {
            TE_MIN.store(d, Ordering::Relaxed);
        }
        if d > TE_MAX.load(Ordering::Relaxed) {
            TE_MAX.store(d, Ordering::Relaxed);
        }
    }
    TE_AT.store(now, Ordering::Relaxed);
    TE_COUNT.store(n.wrapping_add(1), Ordering::Relaxed);
    cortex_m::asm::dsb();
}

/// Starts the pending transfer, if one is: RAMWR, then SWRQ (module
/// docs). `now`: the cycle count of the event that started it (the TE
/// interrupt's entry). Called from the TE interrupt, or by
/// [`LcdDma::start_now`] with interrupts off; the state swap lets only one
/// of them through.
#[inline]
fn start_pending(now: u32) {
    if STATE
        .compare_exchange(PENDING, RUNNING, Ordering::Acquire, Ordering::Relaxed)
        .is_ok()
    {
        // SAFETY: CMD_ADDR is inside the FMC window, open while the bank
        // is configured (`Lcd` exists, owned by the `LcdDma` that set
        // PENDING); an aligned byte store to Device memory. The main loop
        // does not touch the FMC while the state is not IDLE (`LcdDma`
        // holds the `Lcd`).
        unsafe { core::ptr::write_volatile(lcd::CMD_ADDR as *mut u8, RAMWR) };
        // RAMWR has left the CPU (the FMC takes AXI writes in order, so
        // MDMA's first byte follows it on the bus).
        cortex_m::asm::dsb();
        // SAFETY: the channel was programmed and enabled by `present`;
        // SWRQ with PL and EN kept (14.5.5).
        mdma()
            .cr
            .write(|w| unsafe { w.bits(CR_PL_VERY_HIGH | CR_EN | CR_SWRQ) });
        START_TE_AT.store(now, Ordering::Relaxed);
        START_AT.store(cycles(), Ordering::Relaxed);
    }
}

/// A readback check's result ([`readback`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Readback {
    /// Pixels whose three bytes differ from [`lcd::expected_readback`]
    /// under [`lcd::READBACK_MASK`].
    pub mismatches: u32,
    /// The first one: (x, y, got, want).
    pub first: Option<(u16, u16, [u8; 3], [u8; 3])>,
}

/// Reads the window ([`Y0`]..=[`Y1`], all columns) back from the panel's
/// GRAM (RAMRD, 3 bytes per pixel) and compares it with `buf` (plan M3
/// "Readback self-test"). Takes the panel and buffer from
/// [`LcdDma::parts`] (no transfer may run). About 70 ms at [`lcd::SPEC`].
pub fn readback(l: &mut Lcd, buf: &OutFrame) -> Readback {
    l.set_window(0, Y0, lcd::WIDTH - 1, Y1);
    l.begin_readback();
    let mut r = Readback::default();
    for (y, row) in buf.iter().enumerate() {
        for (i, &w) in row.iter().enumerate() {
            // The two pixels of a word: memory bytes hi0 lo0 hi1 lo1.
            let b = w.to_le_bytes();
            let px = [
                u16::from_be_bytes([b[0], b[1]]),
                u16::from_be_bytes([b[2], b[3]]),
            ];
            for (k, p) in px.into_iter().enumerate() {
                let got = [l.read_data(), l.read_data(), l.read_data()];
                let want = lcd::expected_readback(p);
                if (0..3).any(|j| got[j] & lcd::READBACK_MASK != want[j] & lcd::READBACK_MASK) {
                    r.mismatches += 1;
                    if r.first.is_none() {
                        r.first = Some(((i * 2 + k) as u16, y as u16, got, want));
                    }
                }
            }
        }
    }
    l.end_read();
    r
}

/// Writes the bitwise inverse of `buf` into the window by CPU stores (the
/// display self-test's sentinel, M3 review T-2): called with the frame
/// about to be transferred already in `buf`, so every pixel of the window
/// then differs from what the transfer must write, and a transfer that
/// drops, shortens or misplaces any part of it leaves pixels that
/// [`readback`] counts as mismatches (without it, a frame equal to the
/// previous one passes whatever the transfer did). Inverting every bit of
/// a pixel changes every readback byte under [`lcd::READBACK_MASK`]
/// (asserted below for all 65,536 values). Takes the panel and buffer
/// from [`LcdDma::parts`] (no transfer may run). About 12 ms (25,920
/// word stores through the FMC); not part of a real frame loop.
pub fn write_inverse(l: &mut Lcd, buf: &OutFrame) {
    l.set_window(0, Y0, lcd::WIDTH - 1, Y1);
    l.begin_pixels();
    for row in buf.iter() {
        for &w in row.iter() {
            // The buffer's memory byte order is the wire order, as the
            // MDMA sends it (module docs); a word store sends its bytes
            // in memory order (`Lcd::write_data_u32`).
            l.write_data_u32(!w);
        }
    }
    // Let the stores finish here, so their drain is not charged to the
    // caller's next timed step (`present`, the 5% budget figure): with a
    // DSB alone `present` measured 1,586 cycles instead of about 335
    // (the FMC's write FIFO still emptying at the panel's write speed);
    // 20 µs is several times that.
    cortex_m::asm::dsb();
    lcd::delay_us(20);
}

/// Every pixel's inverse reads back different in all three bytes.
const fn inverse_differs_everywhere() -> bool {
    let mut p: u32 = 0;
    while p <= 0xFFFF {
        let a = lcd::expected_readback(p as u16);
        let b = lcd::expected_readback(!(p as u16));
        let mut j = 0;
        while j < 3 {
            if a[j] & lcd::READBACK_MASK == b[j] & lcd::READBACK_MASK {
                return false;
            }
            j += 1;
        }
        p += 1;
    }
    true
}
const _: () = assert!(inverse_differs_everywhere());

/// TE edge statistics since [`te_reset`], in CPU cycles.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TeStats {
    /// Rising edges seen.
    pub edges: u32,
    /// Mean period: (last - first) / (edges - 1); 0 below two edges.
    pub mean: u32,
    pub min: u32,
    pub max: u32,
    /// Cycle counter at the last edge.
    pub last: u32,
}

/// Restarts the TE statistics.
pub fn te_reset() {
    cortex_m::interrupt::free(|_| {
        TE_COUNT.store(0, Ordering::Relaxed);
        TE_MIN.store(u32::MAX, Ordering::Relaxed);
        TE_MAX.store(0, Ordering::Relaxed);
    });
}

/// The TE statistics (read with interrupts off, so one consistent set).
/// The cycle counter wraps after 9.1 s: read within that of [`te_reset`].
pub fn te_stats() -> TeStats {
    cortex_m::interrupt::free(|_| {
        let edges = TE_COUNT.load(Ordering::Relaxed);
        let last = TE_AT.load(Ordering::Relaxed);
        let mean = if edges >= 2 {
            last.wrapping_sub(TE_FIRST.load(Ordering::Relaxed)) / (edges - 1)
        } else {
            0
        };
        TeStats {
            edges,
            mean,
            min: TE_MIN.load(Ordering::Relaxed),
            max: TE_MAX.load(Ordering::Relaxed),
            last,
        }
    })
}
