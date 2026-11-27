//! `play --audio-wav` (ADR 0041): the capture's gap and WAV logic, and the
//! `play` loop asking for the stream from the simulated board.

use std::cell::Cell;
use std::io;
use std::time::{Duration, Instant};

use gbc_link::audio::{decode, encode_u12, sample};
use gbc_pc::audio::{Capture, MAX_FILL, WAV_RATE_HZ, to_code, to_i16, wav};
use gbc_pc::input::{Key, KeySet};
use gbc_pc::play::{Keyboard, PlayEvent, PlayOptions, play};
use gbc_pc::sim::SimBoard;
use gbc_pc::{Client, Config, Error};

/// A code word (the sim's audio script): left 12-bit code in bits 11:0,
/// right in 27:16.
fn word(l: u16, r: u16) -> u32 {
    u32::from(l) | u32::from(r) << 16
}

/// Code words as the frames the PC decodes from a U12 `Audio` (through
/// the wire encoding, so the test covers it).
fn data(words: &[u32]) -> Vec<[i16; 2]> {
    let bytes: Vec<u8> = words
        .iter()
        .flat_map(|&w| encode_u12(sample(w as u16), sample((w >> 16) as u16)))
        .collect();
    let mut f = Vec::new();
    decode(gbc_link::audio::U12, &bytes, |l, r| f.push([l, r])).unwrap();
    f
}

/// The link's U12 code is the H753 DAC's code: `gbc_link::audio::code`
/// equals each lane of `gbc_firmware::audio::u12_lanes` for every sample
/// (the board sends the PC what its DAC plays; ADR 0044).
#[test]
fn link_u12_code_equals_the_dac_code() {
    use gbc_firmware::audio::{PcmFrame, u12_lanes};
    for n in i16::MIN..=i16::MAX {
        let w = u12_lanes(PcmFrame::new(n, n.wrapping_mul(7)).to_bits());
        let c = gbc_link::audio::code(n);
        assert_eq!((w & 0xFFF) as u16, c, "{n}");
        assert_eq!(
            (w >> 16) as u16,
            gbc_link::audio::code(n.wrapping_mul(7)),
            "{n}"
        );
    }
}

#[test]
fn scale_round_trips_every_code() {
    for c in 0..4096u16 {
        assert_eq!(to_code(to_i16(c)), c);
    }
    assert_eq!(to_i16(2048), 0);
    assert_eq!(to_i16(0), -32768);
    assert_eq!(to_i16(4095), 32752);
}

#[test]
fn capture_orders_fills_gaps_and_counts_held() {
    let mut c = Capture::new();
    c.on_audio(1000, 0, &data(&[word(1, 2), word(3, 4)]));
    c.on_audio(1002, 5, &data(&[word(5, 6)]));
    // Two frames missing (1003, 1004): filled with the last frame.
    c.on_audio(1005, 0, &data(&[word(7, 8)]));
    let s = c.stats;
    assert_eq!(s.first_frame, Some(1000));
    assert_eq!(
        (s.messages, s.frames, s.gaps, s.missing, s.filled, s.held),
        (3, 4, 1, 2, 2, 5)
    );
    let want: Vec<i16> = [1, 2, 3, 4, 5, 6, 5, 6, 5, 6, 7, 8]
        .iter()
        .map(|&c| to_i16(c))
        .collect();
    assert_eq!(c.samples(), &want[..]);
    // A jump backwards (a board reset): a gap, no fill.
    c.on_audio(10, 0, &data(&[word(9, 9)]));
    assert_eq!((c.stats.gaps, c.stats.missing, c.stats.filled), (2, 2, 2));
    assert_eq!(c.samples().len(), want.len() + 2);
    // A huge forward gap is filled with at most MAX_FILL frames.
    c.on_audio(11 + 3 * MAX_FILL, 0, &[]);
    assert_eq!(c.stats.missing, 2 + 3 * u64::from(MAX_FILL));
    assert_eq!(c.samples().len(), want.len() + 2 + 2 * MAX_FILL as usize);
    // Wrapping counters are no gap.
    let mut w = Capture::new();
    w.on_audio(u32::MAX, 0, &data(&[word(1, 1), word(2, 2)]));
    w.on_audio(1, 0, &data(&[word(3, 3)]));
    assert_eq!(w.stats.gaps, 0);
}

#[test]
fn wav_header_is_16_bit_stereo_pcm() {
    let w = wav(&[1, -2, 3, -4], WAV_RATE_HZ);
    assert_eq!(w.len(), 44 + 8);
    assert_eq!(&w[0..4], b"RIFF");
    assert_eq!(u32::from_le_bytes(w[4..8].try_into().unwrap()), 36 + 8);
    assert_eq!(&w[8..16], b"WAVEfmt ");
    assert_eq!(u16::from_le_bytes([w[22], w[23]]), 2, "channels");
    assert_eq!(u32::from_le_bytes(w[24..28].try_into().unwrap()), 32_768);
    assert_eq!(
        u32::from_le_bytes(w[28..32].try_into().unwrap()),
        32_768 * 4
    );
    assert_eq!(u16::from_le_bytes([w[34], w[35]]), 16, "bits");
    assert_eq!(&w[36..40], b"data");
    assert_eq!(u32::from_le_bytes(w[40..44].try_into().unwrap()), 8);
    assert_eq!(&w[44..46], &1i16.to_le_bytes());
    assert_eq!(&w[50..52], &(-4i16).to_le_bytes());
}

/// No keys for `n` polls, then Esc.
struct Idle(u32);

impl Keyboard for Idle {
    fn held(&mut self) -> io::Result<KeySet> {
        self.0 = self.0.saturating_sub(1);
        Ok(if self.0 == 0 {
            KeySet::of(&[Key::Esc])
        } else {
            KeySet::default()
        })
    }
}

fn run(board: SimBoard, audio: bool) -> (Client<SimBoard>, Result<u32, Error>, Vec<PlayEvent>) {
    let mut c = Client::new(
        board,
        Config {
            retries: 1,
            timeout: Duration::from_millis(50),
        },
    );
    let t0 = Instant::now();
    let n = Cell::new(0u32);
    let mut now = || {
        n.set(n.get() + 1);
        t0 + Duration::from_millis(10) * n.get()
    };
    let opts = PlayOptions {
        audio,
        ..PlayOptions::default()
    };
    let mut events = Vec::new();
    let r = play(&mut c, &mut Idle(20), &opts, &mut now, &mut |e| {
        events.push(e)
    })
    .map(|r| r.audio);
    (c, r, events)
}

#[test]
fn play_asks_for_audio_and_hands_on_every_message() {
    let mut b = SimBoard::new(1 << 20);
    let halves: Vec<(u32, u16, Vec<u32>)> = (0..3u32)
        .map(|h| {
            (
                h * 256,
                0,
                (0..256).map(|i| word(i as u16, 4095 - h as u16)).collect(),
            )
        })
        .collect();
    b.audio_script = halves.clone();
    let (c, r, events) = run(b, true);
    assert_eq!(r.unwrap(), 3);
    let got: Vec<(u32, u16, Vec<[i16; 2]>)> = events
        .into_iter()
        .filter_map(|e| match e {
            PlayEvent::Audio {
                first,
                held,
                frames,
            } => Some((first, held, frames)),
            _ => None,
        })
        .collect();
    let want: Vec<(u32, u16, Vec<[i16; 2]>)> =
        halves.iter().map(|(f, h, w)| (*f, *h, data(w))).collect();
    assert_eq!(got, want);
    assert!(!c.transport().audio_on, "AudioStop sent at the end");
}

#[test]
fn play_without_audio_never_asks() {
    let mut b = SimBoard::new(1 << 20);
    b.audio_script = vec![(0, 0, vec![word(1, 1)])];
    let (c, r, events) = run(b, false);
    assert_eq!(r.unwrap(), 0);
    assert!(!events.iter().any(|e| matches!(e, PlayEvent::Audio { .. })));
    assert_eq!(c.transport().audio_script.len(), 1, "never turned on");
}

/// ADR 0044: a sample format this gbc-pc does not know ends the play with
/// an error that names it; nothing of it is handed on.
#[test]
fn an_unknown_audio_format_fails_the_play_clearly() {
    let mut b = SimBoard::new(1 << 20);
    b.audio_script = vec![(0, 0, vec![word(1, 1)])];
    b.audio_format = 9;
    let (_, r, events) = run(b, true);
    let e = r.unwrap_err().to_string();
    assert!(e.contains("sample format 9"), "{e}");
    assert!(!events.iter().any(|e| matches!(e, PlayEvent::Audio { .. })));
    // The same script in the H753's format plays.
    let mut b = SimBoard::new(1 << 20);
    b.audio_script = vec![(0, 0, vec![word(1, 1)])];
    b.audio_format = gbc_link::audio::U12;
    assert_eq!(run(b, true).1.unwrap(), 1);
}

#[test]
fn a_board_without_audio_fails_the_play() {
    let mut b = SimBoard::new(1 << 20);
    b.audio_capable = false;
    let (_, r, _) = run(b, true);
    assert!(r.is_err());
}
