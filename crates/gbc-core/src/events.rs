//! Events reported by the run methods.

use crate::bitset::bitset;

bitset! {
    /// Things that happened during a run call ([`RunResult::events`](crate::RunResult::events)).
    ///
    /// Every event that occurred during the call is reported. Some events
    /// always end the call ([`Events::ALWAYS_STOP`]); the others end it only
    /// if enabled with [`Emulator::set_stop_events`](crate::Emulator::set_stop_events).
    /// A call always ends on an instruction boundary, right after the
    /// instruction (or interrupt dispatch, or idle M-cycle) that raised the
    /// event.
    pub struct Events(u16) {
        /// A frame was completed (the PPU entered VBlank, LY = 144). The
        /// frame buffer passed to this call now holds the whole frame.
        /// Always stops. Never fires while the LCD is off.
        const FRAME_READY = 1 << 0;
        /// The audio buffer passed to this call is full. Always stops.
        /// Never fires for an empty audio buffer.
        const AUDIO_BUFFER_FULL = 1 << 1;
        /// The CPU executed opcode `0x40` (`LD B,B`), the test-ROM
        /// breakpoint convention (Mooneye). Stops only if enabled.
        const LD_B_B = 1 << 2;
        /// The CPU executed an invalid opcode and locked up. Fires once, at
        /// the lockup. Always stops. Emulation can continue afterwards: time
        /// advances and the PPU keeps running, but the CPU stays stuck
        /// (see [`Emulator::cpu_locked`](crate::Emulator::cpu_locked)).
        const CPU_LOCKED = 1 << 3;
        /// A byte was sent out on the serial port. The byte is available
        /// from [`Emulator::serial_byte`](crate::Emulator::serial_byte)
        /// until the next one is sent. Stops only if enabled; enable it to
        /// collect every byte.
        const SERIAL_TX = 1 << 4;
    }
}

impl Events {
    /// Events that end a run call regardless of
    /// [`Emulator::set_stop_events`](crate::Emulator::set_stop_events): the
    /// caller must consume the frame or audio buffer before the core
    /// overwrites it, and a locked CPU needs attention.
    pub const ALWAYS_STOP: Self =
        Self::FRAME_READY.union(Self::AUDIO_BUFFER_FULL).union(Self::CPU_LOCKED);
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::Events;
    use std::format;

    #[test]
    fn always_stop_set() {
        assert!(Events::ALWAYS_STOP.contains(Events::FRAME_READY));
        assert!(Events::ALWAYS_STOP.contains(Events::AUDIO_BUFFER_FULL));
        assert!(Events::ALWAYS_STOP.contains(Events::CPU_LOCKED));
        assert!(!Events::ALWAYS_STOP.intersects(Events::LD_B_B | Events::SERIAL_TX));
    }

    #[test]
    fn debug_and_unknown_bits() {
        assert_eq!(
            format!("{:?}", Events::FRAME_READY | Events::LD_B_B),
            "Events(FRAME_READY | LD_B_B)"
        );
        assert_eq!(Events::from_bits_truncate(0xFFFF), Events::ALL);
        assert_eq!(Events::ALL.bits(), 0x1F);
    }
}
