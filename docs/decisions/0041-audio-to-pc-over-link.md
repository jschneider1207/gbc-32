# 0041: The board's audio goes to the PC over the serial link (opt-in)

- Status: Accepted (agent decision)
- Date: 2026-11-26
- Builds on: ADR 0036 (link protocol, `gbc-link`), ADR 0035 (DC blocker,
  12-bit DAC feed), plan `docs/plan/stage-2.md` M4/M5; the human asked for
  it ("Do the audio over pc link": no speaker on the DAC pins)

## Context
The player feeds DAC1 at 32,768.675 Hz, 12-bit stereo. The human has no
speaker for PA4/PA5, so the sound should also reach the PC. The link is
USART3 at 2 Mbit/s through the ST-LINK VCP (200 KB/s raw), already used
for replies, `Stats` and save dumps from one 4 KiB TX DMA buffer, polled
once per video frame. The M5 runs and the compared `paced`/`player` lines
must not change.

## Decision
- **Opt-in.** New messages: `AudioStart` (0x40) and `AudioStop` (0x41),
  PC → board, answered `Ack` (`Nak UnexpectedType` on images without
  audio: link-probe, the player's no-ROM loop). `Audio` (0xB0), board → PC,
  never answered: `first u32, held u16, data` (3 bytes per frame, up to
  340 frames). Nothing is sent until the PC asks; the stream also stops on
  `Hello` and after 90 frames (1.5 s) with no frame from the PC.
- **What is sent: the real frames the DAC ISR hands to the DMA** (what
  `FeedTap` hashes), not underrun filler; `held` counts the filler the DAC
  played after them, so a live player can reproduce the DAC exactly.
  `first` counts real frames since boot (wrapping), so the PC sees gaps.
  One message per DMA half (256 frames, 7.8 ms): ~784 B on the wire,
  100.9 KB/s measured, half the link.
- **Packing:** two 12-bit codes in 3 bytes, little-endian bit order, left
  (DAC channel 1) in bits 0..=11 (`gbc_link::audio::pack`). The PC's WAV
  sample is `(code - 2048) << 4` (exact inverse of the DAC conversion's 16
  units per step), header rate 32,768 Hz.
- **Board path:** the DAC ISR, after its ring pop, offers the half to an
  `AudioTee` (`gbc_firmware::audio`, SPSC, 4 halves = 31 ms, 4 KiB of AXI
  SRAM; DTCM would have cost stack). The service hook packs queued halves
  into the idle TX buffer after the replies and the pending `Stats`, before
  dump frames, whole frames only. While audio streams one TX transfer is
  capped at 3,072 B (15.4 ms) so it ends inside a video frame and every
  service call finds the transmitter idle.
- **Drop policy:** a full tee drops the newest half and counts it
  (`audio_dropped` frames, `audio_dropped_halves` in `Stats`); the DAC is
  never stalled and no DAC sample is ever touched by the PC's speed.
- **Full rate, no decimation:** measured first-try on the board, 60 s of
  Pokemon Red's intro: 0 gaps, 0 drops, 100.9 KB/s through the VCP, and
  the captured codes equal the host's DAC feed for all 1,971,968 frames.

## Consequences
- `gbc-pc play --audio-wav <file>` (and `scripts/play-on-board.sh
  --audio-wav`) captures the sound; `headless --paced --dac-feed` gives the
  host's frames to compare. Live playback (step 2) can build on
  `PlayEvent::Audio`.
- Cost: ISR +2,140 cycles per half (8.4 per frame; `isr_max` 8,356 →
  10,496), main loop 107 cycles per frame to pack and encode: ~0.8% of
  the 472 MHz CPU, only while streaming. Off, the ISR adds two atomic
  accesses.
- A save dump during streaming gets the TX room audio leaves (~1.3 KB per
  frame instead of 4 KiB per transfer): slower, still complete.
- Only 31 ms of slack on the board: a main loop that skips the service for
  more than ~2 video frames drops audio (counted, visible as gaps).
