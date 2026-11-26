//! The board's audio as the PC receives it (ADR 0041): the `Audio`
//! messages of `gbc-pc play --audio-wav`, checked for gaps and collected
//! into a 16-bit stereo WAV. Host-tested (`tests/audio.rs`) against the
//! simulated board and the real board session.
//!
//! - **Order:** each message's `first` must be the previous one's plus its
//!   frame count. A jump forward is a **gap** (the board dropped messages
//!   because its link fell behind, or a frame was corrupt on the way):
//!   counted, and the WAV gets the last frame repeated for the missing
//!   frames (up to [`MAX_FILL`] per gap), so a WAV sample index stays
//!   `frame index - first_frame`. A jump backwards (a board reset) is
//!   counted as a gap too and the stream restarts there with no fill.
//! - **What is written:** the real frames only. Underrun filler (`held`)
//!   is counted, not written: the WAV is the game's sound as the DAC was
//!   fed, frame for frame, and compares with the host's feed.
//! - **Scale:** a 12-bit DAC code c becomes the `i16` `(c - 2048) << 4`
//!   (exact: `c = (s >> 4) + 2048` gives it back), the inverse of
//!   `gbc_firmware::audio::dac_code`'s 16 units per step.
//! - **Rate:** the WAV header says [`WAV_RATE_HZ`] = 32,768 Hz; the board
//!   plays at 32,768.675 Hz (`docs/reference/audio-dac.md`, +20.6 ppm).

use gbc_link::audio::{FRAME_BYTES, codes};

/// The WAV header's sample rate (the board's nominal rate).
pub const WAV_RATE_HZ: u32 = 32_768;

/// The most frames one gap is filled with (1 s); a longer one is counted
/// but not filled.
pub const MAX_FILL: u32 = 32_768;

/// A DAC code as a WAV sample (module docs).
pub const fn to_i16(code: u16) -> i16 {
    ((code as i32 - 2048) << 4) as i16
}

/// The DAC code back from a WAV sample.
pub const fn to_code(s: i16) -> u16 {
    ((s >> 4) + 2048) as u16
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CaptureStats {
    pub messages: u64,
    /// Real frames received.
    pub frames: u64,
    /// The `first` of the first message, if any.
    pub first_frame: Option<u32>,
    /// Jumps in `first` (forward or backward).
    pub gaps: u32,
    /// Frames missing in forward jumps.
    pub missing: u64,
    /// Filler frames written for them (at most [`MAX_FILL`] per gap).
    pub filled: u64,
    /// Underrun filler frames the board reported (`held`), not written.
    pub held: u64,
}

/// Collects `Audio` messages (module docs).
#[derive(Clone, Debug, Default)]
pub struct Capture {
    next: Option<u32>,
    last: [i16; 2],
    /// Interleaved left, right.
    samples: Vec<i16>,
    pub stats: CaptureStats,
}

impl Capture {
    pub fn new() -> Self {
        Self::default()
    }

    /// One `Audio { first, held, data }`.
    pub fn on_audio(&mut self, first: u32, held: u16, data: &[u8]) {
        let s = &mut self.stats;
        s.messages += 1;
        s.held += u64::from(held);
        s.first_frame.get_or_insert(first);
        if let Some(next) = self.next
            && first != next
        {
            s.gaps += 1;
            let ahead = first.wrapping_sub(next);
            if ahead < 1 << 31 {
                s.missing += u64::from(ahead);
                let fill = ahead.min(MAX_FILL);
                s.filled += u64::from(fill);
                for _ in 0..fill {
                    self.samples.extend_from_slice(&self.last);
                }
            }
        }
        let n = (data.len() / FRAME_BYTES) as u32;
        for f in data.as_chunks::<FRAME_BYTES>().0 {
            let (l, r) = codes(*f);
            self.last = [to_i16(l), to_i16(r)];
            self.samples.extend_from_slice(&self.last);
        }
        s.frames += u64::from(n);
        self.next = Some(first.wrapping_add(n));
    }

    /// The samples so far, interleaved left, right.
    pub fn samples(&self) -> &[i16] {
        &self.samples
    }

    /// The capture as a WAV file (PCM, 16-bit, stereo, [`WAV_RATE_HZ`]).
    pub fn wav(&self) -> Vec<u8> {
        wav(&self.samples, WAV_RATE_HZ)
    }
}

/// A canonical 44-byte-header PCM WAV of interleaved stereo `i16`s.
pub fn wav(samples: &[i16], rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut w = Vec::with_capacity(44 + samples.len() * 2);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_len).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes()); // PCM
    w.extend_from_slice(&2u16.to_le_bytes()); // channels
    w.extend_from_slice(&rate.to_le_bytes());
    w.extend_from_slice(&(rate * 4).to_le_bytes()); // bytes per second
    w.extend_from_slice(&4u16.to_le_bytes()); // block align
    w.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        w.extend_from_slice(&s.to_le_bytes());
    }
    w
}
