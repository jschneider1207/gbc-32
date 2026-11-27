# 0044: A neutral audio frame; the DAC layout stays on the board; the link's audio is versioned

- Status: Accepted (human, 2026-11-27: "go ahead with all of them", on the
  Stage 2 comprehensive review's Q1)
- Date: 2026-11-27
- Amends: ADR 0041 (audio to the PC: its `Audio` wire format)
- Builds on: ADR 0021 (APU), ADR 0035 (decimation filter; DC blocker left
  to the platform), ADR 0036 (firmware crate layout), ADR 0043
  (`gbc-link-board`); `docs/reference/audio-dac.md`,
  `docs/reference/link-protocol.md`; review
  `docs/reviews/stage2-comprehensive.md` rows Q1, P4
  (`docs/reviews/stage2-m7.md` P4)

## Context
The "board-independent" audio path carried the H753's `DAC_DHR12RD`
word (two 12-bit codes at bits 11:0 and 27:16) from the conditioner on:
the sample ring, the feed hash, the PC tee, the link's `Audio` payload
(the two codes in 3 bytes) and the PC's 12-bit maths. The DMA half
(256 frames) was a constant of `gbc-firmware`. Stage 3's H735 plays
through SAI into a 16-bit codec: every one of those would change at
once.

## Decision
1. **`gbc_firmware::audio::PcmFrame { left: i16, right: i16 }`** is the
   frame: signed 16-bit PCM at the core's scale (0 = silence). The
   conditioner saturates the DC blocker's output (+-65,535 at worst) to
   `i16`. Rings, the tap and the tee store it as one `u32`
   (`to_bits`: left in 15:0, right in 31:16), one atomic per slot.
   `i16` over `u16` offset binary: it is the core's own sample type, the
   format an I2S codec and the PC's waveOut/WAV take unchanged, and 0 is
   silence in every consumer; only a 12-bit DAC needs an offset, and it
   pays it in the same instruction that rounds.
2. **Only the board knows the DAC.** `audio_dac::on_dma` gives its
   source a scratch half (1 KiB on the ISR stack, DTCM) of `PcmFrame`
   words and converts it into the DMA half with
   `gbc_firmware::audio::u12_lanes` (per channel `clamp(2048 + n/16)`,
   ties up, both lanes at once, no branch). DHR12RD's channels sit in
   the same 16-bit lanes as `to_bits`, so there is no shuffle; a board
   with another layout converts differently. `on_dma_codes` keeps raw
   DAC words (dac-probe). `u12_lanes` lives in `gbc-firmware` only
   because the board crate has no host tests and the bit-identity proof
   (below) must run in `cargo test`; it is documented as the 12-bit
   quantiser, not the register.
   *Implementation note (2026-11-27, review P5; the decision is
   unchanged):* the scratch half is gone. `on_dma` hands its source the
   DMA half as a write-only `HalfOut<U12Lanes>`, and the ring's
   `pop_to` converts each frame with `u12_lanes` as it reads it (one
   store per frame, the held last frame mapped too); the feed hash and
   the tee read the `PcmFrame` words from the ring in the pop's
   `inspect` callback, never the non-cacheable half. The player's ISR
   stack frame went from 1,088 B back to 80 B.
3. **Bit-identical DAC codes.** `u12_lanes(sample(y))` equals the old
   `pack(dac_code(y), ..)` for every blocker output: saturating to
   `i16` first cannot change a code, because the old code already
   clamped at the rails (y <= -32,761 gives 0, y >= 32,744 gives 4095,
   and -32,768 / 32,767 give the same), and inside the `i16` range
   `(n + 32,768 + 8) >> 4 = ((n + 8) >> 4) + 2048` exactly (32,768 =
   2048 x 16), so there is one rounding, not two. Test
   `u12_lanes_equal_the_old_dac_code` (y in +-70,000 and the `i32`
   extremes on both channels, every `i16` on each lane against a spread
   on the other; 6 of 6 non-equivalent mutants caught, one equivalent). End to end: the
   host's new feed of cgb_sound (`headless --paced --dac-feed`, 1,316,677
   frames), converted with the old formula and hashed as DAC words, gives
   the old committed hash `a47140640767c765`.
4. **The feed hash hashes `PcmFrame`s** (FNV-1a over `to_bits` LE, i.e.
   the s16le stereo stream), so it means the same on any board. Every
   `feed=` value changes once: the agent-generated host lists
   `tools/headless/expected/replay/*.txt` were regenerated with gate H5's
   loop (only `feed=` differs). No human-owned file holds a feed hash
   (`acceptance/` has none); `tools/h7-bench/expected-hashes.txt` hashes
   core samples, unchanged. Old `feed=` values in `docs/reference/m*-evidence*`
   are records of their runs and stay. The board side is re-verified by
   the gate (board equals host).
5. **DMA geometry is the board's.** `DMA_HALF_FRAMES` left
   `gbc-firmware`: `AudioTee<F, H>` takes the half size (the player:
   `audio_dac::HALF`), and the host stand-ins for the ISR (`headless`,
   `replay-input`) name their own 256 (the gate's board equality catches
   a mismatch).
6. **Versioned link audio, protocol 2.** `Audio { first, held, format,
   data }`: a format byte after `held` in every message (self-describing:
   no state to lose with an `Ack`). `1 = U12`: the two 12-bit codes in 3
   bytes, v1's byte layout; `2 = S16`: s16le pairs. `MAX_AUDIO_BYTES` =
   1,020 (whole frames in both: 340 U12, 255 S16). An unknown format
   decodes (any length) and `gbc-pc play` stops with an error naming it.
   `PROTOCOL_VERSION` 2 (the body changed), so a v1 PC and a v2 board
   refuse each other at `Hello`. The PC decodes once in `play` and its WAV
   capture and live playback take 16-bit frames.

## Bandwidth (why the H753 keeps U12)
The UART is 2 Mbit/s, 8N1: 200,000 B/s board to PC (input goes the other
way). A message is type + seq + body + CRC (4), COBS (+1 per 254 bytes,
here 4) and the 0 delimiter; 32,768.675 / 256 = 128 halves a second.
- U12, one message per half: body 7 + 768, frame 786 B: **100,610 B/s,
  50% of the line** (v1 was 785 B; measured 2026-11-26: 100,868 B/s with
  `Stats`).
- S16: 256 frames do not fit one message (255 max), so two of 128:
  2 x 529 B: **135,424 B/s, 68%**, and 2,267 B of the 3,072 B per-frame
  TX cap the player uses while audio streams (U12: 1,684 B). The rest
  (Stats, a save dump of up to 128 KiB in 1 KiB chunks, replies) would
  have a third of the line and the tee only 31 ms of slack.
So the H753 sends U12; S16 is for a board whose output is 16-bit and
whose link has the room (Stage 3 decides, and may need a larger
`MAX_BODY` or a split half).

## Cost
DMA ISR (Arm objdump of `DMA1_STR0`, release images): the conversion loop
is 8 instructions a frame (ldr, and/lsr, eor, and/lsr, add, and/lsr, sub,
str; unrolled x16), ~2,110 per refill, plus a 1 KiB `memclr4` of the
scratch (~260 word stores). In exchange the pop, the hash and the tee
write and read DTCM instead of the non-cacheable DMA buffer (before, the
hash and the tee read the buffer back), and the buffer still gets one
store per frame. Static size: `DMA1_STR0` 0x4ee -> 0x724 bytes (player),
0x3d0 -> 0x5f0 (paced). The cycles are the board gate's to measure
(`audio_isr_max`, ADR 0041: 8,356 off / 10,496 tee on before). The
producer's per-frame conversion got cheaper (two saturations instead of
two round-offset-clamps and a pack). If the ISR max rises measurably, the
next step is to fuse the conversion into the ring's pop (the hash and tee
reading the scratch words in the same loop).

## Consequences
- Stage 3's SAI board writes `to_bits` words (or their halves) to its
  codec with no conversion, and the ring, hash, tee, link and PC need no
  change; it may send S16.
- The bit-identity test pins the H753's DAC codes; changing the blocker
  or the quantiser fails it.
- One more protocol bump to keep in step: gbc-pc and the firmware must
  come from the same commit (as for v1).
