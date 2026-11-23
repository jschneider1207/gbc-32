//! The length counter and the volume envelope, shared by the channels that
//! have them (ADR 0021 section 5). Pan Docs: "Audio Registers", "Audio
//! details" (Obscure behavior, Zombie mode).

/// The length counter (NRx1 load, NRx4 bit 6 enable). `max` is 64, or 256
/// for CH3; the channel passes it.
#[repr(C)]
#[derive(Clone, Copy)]
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub(crate) struct Length {
    /// Clocks left before the channel turns off (0: expired).
    pub(crate) counter: u16,
    /// NRx4 bit 6.
    pub(crate) enabled: bool,
}

impl Length {
    pub(crate) const ZERO: Self = Self {
        counter: 0,
        enabled: false,
    };

    /// An NRx1 write: the counter loads `max - value` (`value < max`).
    #[inline]
    pub(crate) fn load(&mut self, max: u16, value: u16) {
        self.counter = max - value;
    }

    /// A frame-sequencer length step (256 Hz). Returns `true` when the
    /// counter reaches 0 now: the channel turns off.
    #[inline]
    pub(crate) fn clock(&mut self) -> bool {
        if self.enabled && self.counter != 0 {
            self.counter -= 1;
            return self.counter == 0;
        }
        false
    }

    /// An NRx4 write (`value`) with the frame sequencer at `fs` (steps
    /// since power on, mod 8). Returns `true` if the channel must turn off.
    ///
    /// Pan Docs "Obscure behavior": if the next step doesn't clock length
    /// (`fs` odd), enabling length (bit 6 from 0 to 1) with a non-zero
    /// counter clocks it once more, and reaching 0 turns the channel off
    /// unless the write also triggers. A trigger with the counter at 0
    /// loads `max`, or `max - 1` if length is now enabled and `fs` is odd.
    /// (SameBoy `GB_apu_write` NRx4 does the trigger reload first and then
    /// the extra clock, with the same results.)
    #[inline]
    pub(crate) fn write_nrx4(&mut self, max: u16, value: u8, fs: u8) -> bool {
        let enable = value & 0x40 != 0;
        let trigger = value & 0x80 != 0;
        let extra = enable && fs & 1 == 1;
        let mut off = false;
        if extra && !self.enabled && self.counter != 0 {
            self.counter -= 1;
            off = self.counter == 0 && !trigger;
        }
        self.enabled = enable;
        if trigger && self.counter == 0 {
            self.counter = max - u16::from(extra);
        }
        off
    }
}

/// The volume envelope (NRx2), classic model (ADR 0021 section 5).
#[repr(C)]
#[derive(Clone, Copy)]
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub(crate) struct Envelope {
    /// The current volume, 0-15: the channel's digital output when its
    /// waveform is high.
    pub(crate) volume: u8,
    /// Envelope steps left before the next volume change.
    pub(crate) timer: u8,
    /// The last NRx2 written: initial volume (bits 4-7), direction (bit 3,
    /// 1 = up), period (bits 0-2, 0 = no change).
    pub(crate) nrx2: u8,
    /// Still changing the volume (cleared at 0 or 15).
    pub(crate) running: bool,
}

impl Envelope {
    pub(crate) const ZERO: Self = Self {
        volume: 0,
        timer: 0,
        nrx2: 0,
        running: false,
    };

    /// The DAC is on: NRx2 bits 3-7 not all 0 (Pan Docs: "Audio details",
    /// DACs).
    #[inline]
    pub(crate) fn dac(&self) -> bool {
        self.nrx2 & 0xF8 != 0
    }

    /// A trigger: the volume and timer reload from NRx2.
    #[inline]
    pub(crate) fn trigger(&mut self) {
        self.volume = self.nrx2 >> 4;
        // A period of 0 loads 8 (it only counts once NRx2 gives a period).
        self.timer = match self.nrx2 & 7 {
            0 => 8,
            p => p,
        };
        self.running = true;
    }

    /// A frame-sequencer envelope step (64 Hz). With a non-zero period,
    /// the timer counts down and at 0 reloads and moves the volume one
    /// step in the NRx2 direction; reaching 0 or 15 stops the envelope.
    #[inline]
    pub(crate) fn clock(&mut self) {
        let period = self.nrx2 & 7;
        if period == 0 || !self.running {
            return;
        }
        self.timer = self.timer.wrapping_sub(1);
        if self.timer != 0 {
            return;
        }
        self.timer = period;
        let up = self.nrx2 & 8 != 0;
        match (up, self.volume) {
            (true, v) if v < 15 => self.volume = v + 1,
            (false, v) if v > 0 => self.volume = v - 1,
            _ => self.running = false,
        }
    }

    /// An NRx2 write. While the channel is `on`, the volume changes by the
    /// CGB-02/04 "zombie mode" rule (Pan Docs "Audio details", Zombie
    /// mode): if the old period was 0 and the envelope still runs, +1;
    /// else if the old direction was down, +2; then if the direction
    /// changed, 16 - volume; the low 4 bits are kept. (SameBoy
    /// `_nrx2_glitch` is the exact CGB-C model, ADR 0021 section 12.)
    #[inline]
    pub(crate) fn write(&mut self, value: u8, on: bool) {
        let old = self.nrx2;
        self.nrx2 = value;
        if !on {
            return;
        }
        let mut v = self.volume;
        if old & 7 == 0 && self.running {
            v += 1;
        } else if old & 8 == 0 {
            v += 2;
        }
        if (old ^ value) & 8 != 0 {
            v = 16u8.wrapping_sub(v);
        }
        self.volume = v & 0x0F;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn length_expires_after_its_count() {
        let mut len = Length::ZERO;
        len.load(64, 61);
        assert!(!len.clock(), "disabled: not clocked");
        assert_eq!(len.counter, 3);
        len.enabled = true;
        assert!(!len.clock());
        assert!(!len.clock());
        assert!(len.clock(), "3 -> 0: off");
        assert!(!len.clock(), "stays at 0");
        assert_eq!(len.counter, 0);
    }

    #[test]
    fn enabling_length_when_the_next_step_skips_it_clocks_once_more() {
        // fs odd: the next step doesn't clock length.
        let mut len = Length::ZERO;
        len.load(64, 60);
        assert!(!len.write_nrx4(64, 0x40, 1));
        assert_eq!(len.counter, 3, "extra clock");
        // Already enabled: no extra clock.
        assert!(!len.write_nrx4(64, 0x40, 1));
        assert_eq!(len.counter, 3);
        // fs even: no extra clock.
        let mut len = Length::ZERO;
        len.load(64, 60);
        assert!(!len.write_nrx4(64, 0x40, 2));
        assert_eq!(len.counter, 4);
        // Down to 0 without a trigger: the channel turns off.
        let mut len = Length::ZERO;
        len.load(64, 63);
        assert!(len.write_nrx4(64, 0x40, 3));
        assert_eq!(len.counter, 0);
        // Down to 0 with a trigger: stays on, reloads 63.
        let mut len = Length::ZERO;
        len.load(64, 63);
        assert!(!len.write_nrx4(64, 0xC0, 3));
        assert_eq!(len.counter, 63);
    }

    #[test]
    fn a_trigger_reloads_an_expired_length() {
        let mut len = Length::ZERO;
        assert!(!len.write_nrx4(64, 0x80, 1));
        assert_eq!(len.counter, 64, "length disabled: max");
        let mut len = Length::ZERO;
        assert!(!len.write_nrx4(64, 0xC0, 0));
        assert_eq!(len.counter, 64, "fs even: max");
        let mut len = Length::ZERO;
        assert!(!len.write_nrx4(256, 0xC0, 1));
        assert_eq!(len.counter, 255, "enabled, fs odd: max - 1");
        // A non-zero counter is kept.
        let mut len = Length::ZERO;
        len.load(64, 50);
        len.write_nrx4(64, 0x80, 0);
        assert_eq!(len.counter, 14);
    }

    fn env(nrx2: u8) -> Envelope {
        let mut e = Envelope::ZERO;
        e.write(nrx2, false);
        e.trigger();
        e
    }

    #[test]
    fn envelope_moves_down_every_period_steps_and_stops_at_0() {
        let mut e = env(0x22); // volume 2, down, period 2
        e.clock();
        assert_eq!(e.volume, 2);
        e.clock();
        assert_eq!(e.volume, 1);
        e.clock();
        e.clock();
        assert_eq!(e.volume, 0);
        assert!(e.running);
        e.clock();
        e.clock();
        assert_eq!((e.volume, e.running), (0, false));
    }

    #[test]
    fn envelope_moves_up_and_stops_at_15() {
        let mut e = env(0xE9); // volume 14, up, period 1
        e.clock();
        assert_eq!(e.volume, 15);
        e.clock();
        assert_eq!((e.volume, e.running), (15, false));
        e.clock();
        assert_eq!(e.volume, 15);
    }

    #[test]
    fn envelope_period_0_never_changes_the_volume() {
        for nrx2 in [0x70, 0x78] {
            let mut e = env(nrx2);
            for _ in 0..20 {
                e.clock();
            }
            assert_eq!(e.volume, 7, "{nrx2:#x}");
        }
    }

    #[test]
    fn zombie_mode_follows_the_cgb_rule() {
        // Old period 0, still running: +1.
        let mut e = env(0x50);
        e.write(0x50, true);
        assert_eq!(e.volume, 6);
        // Old period non-zero, old direction down: +2.
        let mut e = env(0x51);
        e.write(0x51, true);
        assert_eq!(e.volume, 7);
        // Old direction up, period non-zero: unchanged.
        let mut e = env(0x59);
        e.write(0x59, true);
        assert_eq!(e.volume, 5);
        // Direction changed (up -> down): 16 - volume.
        let mut e = env(0x59);
        e.write(0x51, true);
        assert_eq!(e.volume, 11);
        // Low 4 bits kept: 15 + 2 = 17 -> 1.
        let mut e = env(0xF1);
        e.write(0xF1, true);
        assert_eq!(e.volume, 1);
        // 17 and a direction change: 16 - 17 wraps to 15.
        let mut e = env(0xF1);
        e.write(0xF9, true);
        assert_eq!(e.volume, 15);
        // Channel off: the volume is untouched.
        let mut e = env(0x51);
        e.write(0x58, false);
        assert_eq!(e.volume, 5);
    }
}
