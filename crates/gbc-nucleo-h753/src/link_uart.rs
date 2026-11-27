//! USART3 for the serial link while a game runs (plan Stage 2 M5): both
//! directions by DMA, so a frame of emulation (up to ~7 ms of core time,
//! 16.7 ms per frame paced) between polls loses no received byte and a
//! save dump never stalls the loop.
//!
//! - **Receive:** DMA1 stream 1 (DMAMUX1 channel 1, request 45
//!   `usart3_rx_dma`, RM0433 Table 121), circular, bytes from USART_RDR
//!   into a [`RX_BYTES`] ring. The DMA serves every byte as it arrives,
//!   so the 16-byte RX FIFO never overruns; [`LinkUart::read`] takes what
//!   the DMA has written since the last call (the write position is
//!   `RX_BYTES - NDTR`). The ring can lap the reader only if more than
//!   [`RX_BYTES`] arrive between two polls: 20 ms of back-to-back bytes at
//!   2 Mbit/s, which stop-and-wait (one request of at most ~1.05 KB, then
//!   the PC waits for the answer) and `Input` frames (6-7 bytes on a
//!   change, every 500 ms otherwise) never send. [`LinkUart::rx_high`]
//!   records the most bytes taken in one poll, to show the margin.
//! - **Lap detection** (M7 review C5): NDTR alone cannot tell `k` new
//!   bytes from `k + RX_BYTES`, so each read also takes the stream's
//!   sticky HTIF and TCIF (set at the half and the end of every lap
//!   whether or not their interrupts are on, RM0433 15.3.19 and 15.4
//!   Table 116; cleared by writing LIFCR, 15.5.3). A read that finds both
//!   set counts in [`LinkUart::rx_overflow`]: that includes every lap
//!   (bytes lost) and never a read of at most half the ring
//!   (`gbc_firmware::rx_ring`, which also says why not an interrupt). The
//!   bytes are still delivered as before (the newest ones are intact; the
//!   frame that lost its middle fails CRC). USART ORE does not help here:
//!   the DMA keeps RDR drained while it overwrites the ring.
//! - **Transmit:** DMA1 stream 2 (DMAMUX1 channel 2, request 46
//!   `usart3_tx_dma`), normal mode, from a [`TX_BYTES`] buffer into
//!   USART_TDR. [`LinkUart::tx_buf`] lends the buffer while the stream is
//!   idle, [`LinkUart::start_tx`] sends it and returns at once. 4 KiB is
//!   ~20.5 ms on the wire at 2 Mbit/s, so a dump moves at most one buffer
//!   per poll, about 120-200 KB/s when polled every frame.
//! - Both buffers are one 8 KiB block of AXI SRAM (DMA1 cannot reach the
//!   TCMs, RM0433 Table 3), aligned to its size and made Normal
//!   non-cacheable by MPU region [`crate::mpu::REGION_LINK_BUF`], so no
//!   cache maintenance is needed.
//! - Both streams set TRBUFF, which RM0433 15.5.5 requires for
//!   U(S)ART transfers. CR3.DDRE = 0: a byte with a framing or noise
//!   error is not transferred (its frame then fails CRC and the PC
//!   resends); [`LinkUart::errors`] counts and clears FE, NE and ORE.
//! - No interrupt: both streams are polled. While a `LinkUart` exists,
//!   nothing else may read USART3 (`Uart::try_read` would steal bytes) or
//!   write it (bytes would interleave with a DMA transfer); the fault
//!   handler's report is the exception (the program has stopped then).

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};

use gbc_firmware::rx_ring::RxRing;

pub use crate::uart::RxErrors;
use crate::uart::{BaudError, BaudSetting, Uart};
use crate::{mpu, pac};

pub const RX_BYTES: usize = 4096;
pub const TX_BYTES: usize = 4096;
const BLOCK_BYTES: usize = RX_BYTES + TX_BYTES;

#[repr(C, align(8192))]
struct Block {
    rx: [u8; RX_BYTES],
    tx: [u8; TX_BYTES],
}
const _: () = assert!(core::mem::size_of::<Block>() == BLOCK_BYTES);
const _: () = assert!(core::mem::align_of::<Block>() == BLOCK_BYTES);

struct Slot(UnsafeCell<Block>);
// SAFETY: reached only through the one `LinkUart` (guarded by `TAKEN`);
// the DMA writes `rx` and reads `tx` as the module docs describe.
unsafe impl Sync for Slot {}
// SAFETY: `.axisram` is the NOLOAD AXI SRAM section memory.x defines for
// DMA buffers; not zeroed at boot, which is sound for bytes, and
// `mpu::dma_buffer_window` zeroes it before the DMA starts.
#[unsafe(link_section = ".axisram.link")]
static BUF: Slot = Slot(UnsafeCell::new(Block {
    rx: [0; RX_BYTES],
    tx: [0; TX_BYTES],
}));
static TAKEN: AtomicBool = AtomicBool::new(false);

const RX_STREAM: usize = 1;
const TX_STREAM: usize = 2;
/// DMAMUX1 request lines (RM0433 Table 121).
pub const DMAMUX_REQ_USART3_RX: u32 = 45;
pub const DMAMUX_REQ_USART3_TX: u32 = 46;

const RCC_AHB1_DMA1EN: u32 = 1 << 0;
// DMA_SxCR (RM0433 15.5.5).
const SCR_EN: u32 = 1 << 0;
const SCR_DIR_M2P: u32 = 0b01 << 6;
const SCR_CIRC: u32 = 1 << 8;
const SCR_MINC: u32 = 1 << 10;
const SCR_PL_HIGH: u32 = 0b10 << 16;
const SCR_PL_MEDIUM: u32 = 0b01 << 16;
const SCR_TRBUFF: u32 = 1 << 20;
const RX_SCR: u32 = SCR_TRBUFF | SCR_PL_HIGH | SCR_MINC | SCR_CIRC;
const TX_SCR: u32 = SCR_TRBUFF | SCR_PL_MEDIUM | SCR_MINC | SCR_DIR_M2P;
// DMA_LISR/LIFCR (15.5.1, 15.5.3): FEIF, DMEIF, TEIF, HTIF, TCIF of a
// stream at bits 0, 2, 3, 4, 5 of its group; stream 1's group at bit 6,
// stream 2's at 16.
const FLAGS: u32 = 0b11_1101;
const TEIF: u32 = 1 << 3;
const HTIF: u32 = 1 << 4;
const TCIF: u32 = 1 << 5;
const RX_SHIFT: u32 = 6;
const TX_SHIFT: u32 = 16;
// USART_CR3 (48.7.3), USART_ISR (48.7.10), USART_ICR (48.7.11).
const CR3_DMAR: u32 = 1 << 6;
const CR3_DMAT: u32 = 1 << 7;
const ISR_FE: u32 = 1 << 1;
const ISR_NE: u32 = 1 << 2;
const ISR_ORE: u32 = 1 << 3;

fn dma() -> &'static pac::dma1::RegisterBlock {
    // SAFETY: streams 1 and 2 and their flags belong to this module;
    // `audio_dac` uses stream 0 and its own flag bits (LIFCR is
    // write-1-to-clear, so neither clears the other's).
    unsafe { &*pac::DMA1::ptr() }
}

fn usart() -> &'static pac::usart1::RegisterBlock {
    // SAFETY: USART3 is this module's while a `LinkUart` exists (module
    // docs).
    unsafe { &*pac::USART3::ptr() }
}

fn stop_stream(n: usize) {
    let st = &dma().st[n];
    // SAFETY: EN cleared, other bits kept.
    st.cr.modify(|r, w| unsafe { w.bits(r.bits() & !SCR_EN) });
    let mut i = 0u32;
    while st.cr.read().bits() & SCR_EN != 0 && i < 1_000_000 {
        i += 1;
    }
}

/// The DMA-driven link UART (module docs). One per program.
pub struct LinkUart {
    rx: RxRing,
    tx_errors: u32,
    errors: RxErrors,
    setting: BaudSetting,
}

impl LinkUart {
    /// Switches USART3 to `bps` (FIFOs on, `Uart::set_baud`), sets up the
    /// buffers' MPU region and both DMA streams, and starts receiving.
    ///
    /// # Panics
    /// On a second call.
    pub fn new(uart: &mut Uart, bps: u32) -> Result<LinkUart, BaudError> {
        assert!(!TAKEN.swap(true, Ordering::AcqRel), "LinkUart taken twice");
        let setting = uart.set_baud(bps)?;
        let base = BUF.0.get() as usize;
        mpu::dma_buffer_window(mpu::REGION_LINK_BUF, base, BLOCK_BYTES);
        // SAFETY: RCC_AHB1ENR read-modify-write of DMA1EN only (DMA1 and
        // DMAMUX1, RM0433 8.7.41); the read back orders it before the
        // first DMA access.
        unsafe {
            let rcc = &*pac::RCC::ptr();
            rcc.ahb1enr
                .modify(|r, w| w.bits(r.bits() | RCC_AHB1_DMA1EN));
            let _ = rcc.ahb1enr.read();
        }
        // Streams an earlier image may have left running (a debugger reset
        // may not reset DMA1).
        stop_stream(RX_STREAM);
        stop_stream(TX_STREAM);
        let u = usart();
        // SAFETY: documented DMAMUX request IDs, no sync or event; the
        // stream registers are written with EN = 0 (15.3.19), flags
        // cleared before EN; PAR is USART3's RDR, M0AR the ring.
        unsafe {
            let mux = &*pac::DMAMUX1::ptr();
            mux.ccr[RX_STREAM].write(|w| w.bits(DMAMUX_REQ_USART3_RX));
            mux.ccr[TX_STREAM].write(|w| w.bits(DMAMUX_REQ_USART3_TX));
            dma()
                .lifcr
                .write(|w| w.bits(FLAGS << RX_SHIFT | FLAGS << TX_SHIFT));
            let st = &dma().st[RX_STREAM];
            st.par.write(|w| w.bits(&u.rdr as *const _ as u32));
            st.m0ar.write(|w| w.bits(base as u32));
            st.ndtr.write(|w| w.bits(RX_BYTES as u32));
            st.fcr.write(|w| w.bits(0)); // direct mode
            st.cr.write(|w| w.bits(RX_SCR));
            cortex_m::asm::dsb();
            st.cr.write(|w| w.bits(RX_SCR | SCR_EN));
            // Stale errors from before, then the DMA requests on.
            u.icr.write(|w| w.bits(ISR_FE | ISR_NE | ISR_ORE));
            u.cr3.modify(|r, w| w.bits(r.bits() | CR3_DMAR | CR3_DMAT));
        }
        Ok(LinkUart {
            rx: RxRing::new(RX_BYTES),
            tx_errors: 0,
            errors: RxErrors::default(),
            setting,
        })
    }

    /// The baud setting in use.
    pub fn setting(&self) -> BaudSetting {
        self.setting
    }

    /// Calls `f` with each byte received since the last call, in order.
    pub fn read(&mut self, mut f: impl FnMut(u8)) {
        self.count_errors();
        let (ndtr, marks) = rx_snapshot();
        let p = self.rx.poll(ndtr, marks & HTIF != 0, marks & TCIF != 0);
        let rx = BUF.0.get() as *const u8;
        let mut i = p.start;
        for _ in 0..p.avail {
            // SAFETY: in bounds (`RxRing` keeps indices < RX_BYTES); a
            // volatile read of a byte the DMA wrote (non-cacheable memory,
            // module docs).
            f(unsafe { rx.add(i).read_volatile() });
            i = (i + 1) % RX_BYTES;
        }
    }

    /// The most bytes one [`LinkUart::read`] found waiting.
    pub fn rx_high(&self) -> usize {
        self.rx.high()
    }

    /// Reads that found the receive ring possibly lapped (module docs,
    /// "Lap detection"): 0 means no received byte was overwritten unread.
    pub fn rx_overflow(&self) -> u32 {
        self.rx.laps()
    }

    /// Whether the transmit stream is idle (its last transfer is done:
    /// EN clears itself at the end of a normal-mode transfer).
    pub fn tx_idle(&self) -> bool {
        dma().st[TX_STREAM].cr.read().bits() & SCR_EN == 0
    }

    /// The transmit buffer, while the stream is idle.
    pub fn tx_buf(&mut self) -> Option<&mut [u8; TX_BYTES]> {
        if !self.tx_idle() {
            return None;
        }
        // SAFETY: the stream is idle, so the DMA is not reading `tx`; the
        // `&mut self` borrow keeps `start_tx` from starting one while the
        // buffer is lent.
        Some(unsafe { &mut (*BUF.0.get()).tx })
    }

    /// Sends the first `n` bytes of the transmit buffer (at most
    /// [`TX_BYTES`]); returns at once. False if the stream is busy or `n`
    /// is 0 (nothing sent).
    pub fn start_tx(&mut self, n: usize) -> bool {
        if n == 0 || !self.tx_idle() {
            return false;
        }
        let d = dma();
        if (d.lisr.read().bits() >> TX_SHIFT) & TEIF != 0 {
            self.tx_errors += 1;
        }
        let st = &d.st[TX_STREAM];
        let tx = BUF.0.get() as usize + RX_BYTES;
        // The buffer writes (Normal memory) complete before the DMA reads.
        cortex_m::asm::dsb();
        // SAFETY: EN = 0 (checked), flags cleared before EN; PAR is
        // USART3's TDR, M0AR the transmit buffer, NDTR <= TX_BYTES.
        unsafe {
            d.lifcr.write(|w| w.bits(FLAGS << TX_SHIFT));
            st.par.write(|w| w.bits(&usart().tdr as *const _ as u32));
            st.m0ar.write(|w| w.bits(tx as u32));
            st.ndtr.write(|w| w.bits(n.min(TX_BYTES) as u32));
            st.fcr.write(|w| w.bits(0));
            st.cr.write(|w| w.bits(TX_SCR));
            st.cr.write(|w| w.bits(TX_SCR | SCR_EN));
        }
        true
    }

    /// Copies `data` into the transmit buffer and sends it, if the stream
    /// is idle and it fits. False otherwise (nothing sent).
    pub fn send(&mut self, data: &[u8]) -> bool {
        match self.tx_buf().and_then(|b| b.get_mut(..data.len())) {
            Some(dst) if !data.is_empty() => dst.copy_from_slice(data),
            _ => return false,
        }
        self.start_tx(data.len())
    }

    /// Waits (bounded, ~1 s) for the transmit stream to finish.
    pub fn flush(&mut self) {
        let _ = crate::clock::wait_for(crate::CPU_HZ, || self.tx_idle());
    }

    /// Receive errors since [`LinkUart::new`] (counted at every read).
    pub fn errors(&mut self) -> RxErrors {
        self.count_errors();
        self.errors
    }

    /// DMA transfer errors seen on the transmit stream.
    pub fn tx_errors(&self) -> u32 {
        self.tx_errors
    }

    fn count_errors(&mut self) {
        let u = usart();
        let isr = u.isr.read().bits() & (ISR_FE | ISR_NE | ISR_ORE);
        if isr == 0 {
            return;
        }
        self.errors.overruns += u32::from(isr & ISR_ORE != 0);
        self.errors.framing += u32::from(isr & ISR_FE != 0);
        self.errors.noise += u32::from(isr & ISR_NE != 0);
        // SAFETY: ORECF, FECF, NECF are write-1-to-clear (same bit
        // positions as the ISR flags); other bits written 0.
        u.icr.write(|w| unsafe { w.bits(isr) });
    }
}

/// The receive stream's NDTR and its HTIF/TCIF (at bit 4/5), the flags
/// read then cleared, taken at one instant: NDTR is read before and after
/// LISR and the pair retried until they agree, so no byte landed between
/// them and the flags cover exactly the bytes up to that NDTR. Only the
/// flags read are cleared (LIFCR is write-1-to-clear, RM0433 15.5.3): a
/// mark crossed after the read stays for the next call. Clearing a flag
/// that was set again between the read and the clear would take a whole
/// lap. A byte takes 5 us at 2 Mbit/s and the three reads well under
/// 1 us, so the first try nearly always agrees; after 4 the last pair is
/// used (at worst a byte at a mark is counted one read late).
fn rx_snapshot() -> (usize, u32) {
    let d = dma();
    let st = &d.st[RX_STREAM];
    let mut tries = 0;
    let (ndtr, marks) = loop {
        let n0 = st.ndtr.read().bits();
        let marks = (d.lisr.read().bits() >> RX_SHIFT) & (HTIF | TCIF);
        let n1 = st.ndtr.read().bits();
        tries += 1;
        if n0 == n1 || tries == 4 {
            break (n1, marks);
        }
    };
    if marks != 0 {
        // SAFETY: write-1-to-clear of this stream's HTIF/TCIF only.
        d.lifcr.write(|w| unsafe { w.bits(marks << RX_SHIFT) });
    }
    (ndtr as usize, marks)
}

/// A baud rate from a build-time variable (`GBC_LINK_BAUD`): `default`
/// when unset; a compile error when empty or not a decimal number.
pub const fn parse_baud(s: Option<&str>, default: u32) -> u32 {
    let Some(s) = s else { return default };
    let b = s.as_bytes();
    assert!(!b.is_empty(), "the baud variable is empty");
    let mut v: u32 = 0;
    let mut i = 0;
    while i < b.len() {
        assert!(b[i].is_ascii_digit(), "the baud variable is not a number");
        v = v * 10 + (b[i] - b'0') as u32;
        i += 1;
    }
    v
}
