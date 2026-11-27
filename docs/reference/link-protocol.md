# Link protocol: saves and input (Stage 2 M5)

What the board must do for the M5 messages, as `tools/gbc-pc` sends and
expects them. The framing, the message table and the stop-and-wait rule
are in the `gbc-link` crate docs (`crates/gbc-link/src/lib.rs`); the ROM
load is ADR 0038 and `loader.md`. The PC side is host-tested against the
simulated board `tools/gbc-pc/src/sim.rs`, which is the executable form
of this page (`tests/save.rs`, `tests/play.rs`).

Hash: FNV-1a 64 over the whole `.sav`, as for the ROM (ADR 0037;
`gbc_firmware::fnv::hash`). The `.sav` is the desktop format (ADR 0023):
cartridge RAM plus an optional clock footer; the board checks it with
`gbc_firmware::save::check` against the loaded cartridge.

## Save load (PC → board), `gbc-pc load <rom> --sav <file>`

After the ROM load (`RomDone` or `AlreadyLoaded`), in the same session:

| PC sends | Board answers (same seq) |
|---|---|
| `SaveBegin { len, hash }` | `Ack`; `Nak OutOfRange` if `len` is not an accepted `.sav` length for this cartridge (`save::check` sizes) or 0; `Nak UnexpectedType` if no game can take a save now |
| `SaveChunk { offset, data }`, offset 0, then each the previous end, `data.len() <= max_chunk` | `Ack`; `Nak OutOfRange` for an offset that does not continue the save, an empty chunk, or one past `len`; `Nak UnexpectedType` without a `SaveBegin` |
| the chunk that completes `len` | the **verdict**, after the hash and `save::check`: `Ack` = the save is applied; `Nak HashMismatch` (hash) or `Nak OutOfRange` (format, e.g. `BadFooter`) = refused, nothing applied, the old save kept |

There is no end message. Buffer the whole save and apply it only after
the last chunk passed (never half-applied). As for ROM frames, a repeat
of the frame last answered is a duplicate: re-send the stored answer,
including the verdict (the PC resends on any `Nak` up to `--retries`, so
a refused save is reported as `gave up after N attempts (last: Nak
HashMismatch)`). A new `SaveBegin` discards a save load in progress.

## Save dump (board → PC), `gbc-pc dump <out.sav>` and F5 in `play`

| PC sends | Board sends, all with the request's seq |
|---|---|
| `SaveDumpRequest` | `SaveBegin { len, hash }`, then `SaveChunk`s covering `0..len` in order (offset 0, then each the previous end; any size 1..=1024), back to back, no `Ack`s; or `Nak UnexpectedType` if there is no save (no game, or a cartridge without one) |

- Snapshot the save (`save::take`) when the request arrives and stream
  from the snapshot, so the hash and the bytes agree while the game runs.
- Every `SaveDumpRequest` starts a fresh dump, aborting one still being
  sent. The PC retries a broken dump (gap, corrupt frame, hash mismatch,
  or no dump frame for `--timeout-ms`) with a new request and a new seq,
  and ignores frames that still carry an older seq.
- `Stats` frames may be interleaved; the PC skips them.
- A board-started dump (joystick combo, plan M5) is the same `SaveBegin`
  plus chunks, unrequested, with any seq; `play` receives it like an F5
  dump (it writes it to `--sav-out`).

The PC writes the file only once the whole dump has arrived and hashes
to `SaveBegin.hash`, via `<out>.part` and a rename.

## Who syncs the RTC (ADR 0040)

The board has no wall clock: it never advances a loaded clock by the
time since the footer's stamp, and it stamps dumps with the stamp of the
save it loaded. The PC does both ends, with the desktop's rule
(`gbc_firmware::save::Clock::sync`, ADR 0023), in `tools/gbc-pc/src/rtc.rs`:

- `load --sav`: the PC checks the save as the board will
  (`save::check`, refusing locally what the board would refuse), advances
  the live clock (MBC3 RTC or HuC3) by now − stamp (halted clocks stay
  put; day overflow sets the carry) and stamps it now. The board starts
  from real time. A 44-byte VBA footer goes as the 48-byte one.
- `dump` and `play`'s dumps: a clock footer is restamped with the PC's
  time when the dump arrived (the board's registers are the time then);
  RAM and clock registers are written as received. The footer is found
  from the length (RAM sizes are 0 or powers of two ≥ 512: mod 512 = 48
  MBC3, 17 HuC3); anything else is written as received.
- `--raw-save` (load, dump, play): neither; the bytes go and come
  unchanged. The round-trip scripts (`run-save-roundtrip.sh`,
  `run-player-save.sh`) use it: they check the transport.

Losses: MBC3 drops the part second, HuC3 the seconds into the current
minute, on each board load and each dump; the board clock is only as true
as its emulation keeps real time.

## Input (PC → board), `gbc-pc play`

- `Input { buttons }`: the held buttons (`gbc_link::buttons`, decoded by
  `gbc_firmware::buttons::from_pc`), a new seq per frame. **Never
  answered**, not even `Ack` (the stop-and-wait rule does not apply);
  apply the latest bitmap at the next frame start.
- The PC sends one on every change and again 500 ms after the last one
  if nothing changed, so a lost frame heals within 500 ms. At exit it
  sends `Input { buttons: 0 }`. A board may treat "no `Input` for well
  over 500 ms" as the PC gone and release the PC buttons.
- Keys: arrows, Z = B, X = A, Enter = Start, Right Shift or Backspace =
  Select; Esc ends `play`, F5 dumps the save to `--sav-out`.
- A frame the board cannot decode is still answered `Nak` (any seq);
  in `play` the PC ignores a `Nak` that does not carry the seq of its
  outstanding dump request.
- `Stats` frames (any seq) are printed as they arrive. The `player` image
  puts its statistics in `extra` as ASCII `key=value` fields separated by
  spaces, starting with `image=player` (`docs/reference/player.md`);
  `gbc-pc play` prints them as `stats: <extra>`.

`play` without audio sends no `Hello` (it can follow a `load` in a
separate run, and `--port null` needs no board). With `--audio` or
`--audio-wav` it opens with the 0x00 flush and `Hello` before
`AudioStart` (see "Audio to the PC").
`gbc-pc play --port null --seconds 2` runs the loop with no board.

## Board side (as implemented, 2026-11-26)

Code: `crates/gbc-nucleo-h753/src/link_session.rs` (the protocol logic,
board-independent; host-tested by `tools/gbc-pc/tests/link_session.rs`,
which runs the real gbc-pc client against it), `src/link_uart.rs` (the
UART), `src/joystick.rs` (the pins), and the `link-probe` image
(`src/bin/link-probe.rs`, `./build.sh link-probe`). The `player` image
will reuse all three.

**UART.** USART3 at 2,000,000 bit/s, the loader's rate and gbc-pc's
`--baud` default, so one rate serves every image (a build-time constant,
`GBC_LINK_BAUD`; gbc-link v1 has no baud switch). Both directions by DMA,
no interrupts: DMA1 stream 1 (DMAMUX request 45) writes a 4 KiB circular
ring, stream 2 (request 46) sends from a 4 KiB buffer; both in one 8 KiB
AXI SRAM block that MPU region 10 makes non-cacheable (DMA1 cannot reach
the TCMs). The DMA takes every byte as it arrives, so a frame of
emulation between polls loses nothing; the ring could only lap with
4 KiB unread, which stop-and-wait (one ~1.05 KB request, then the PC
waits) never sends. FE/NE/ORE are counted.

**Polling.** The probe runs one frame, then serves the link (RX bytes to
the session, its output to the TX DMA when idle) until the frame's 16.74
ms are up, so requests are answered between frames, never inside one. A
dump goes out as whole frames, up to 4 KiB per TX transfer.

**Per message**, as the tables above, plus:
- `Hello`: `HelloReply` (protocol 1, max_chunk 1024, flash_len = the
  NOR's 8 MiB); drops a save load in progress.
- `RomBegin`: `AlreadyLoaded` if it names the running ROM (length and
  FNV-1a 64), else `Nak UnexpectedType`; `RomChunk`/`RomEnd`: `Nak
  UnexpectedType`. So `gbc-pc load <rom> --sav <f>` with the probe's own
  ROM file sends only the save. ROM loading stays in the `loader` image.
- `SaveBegin`: the length must be one `save::check` accepts (RAM, RAM +
  48 or 44 for MBC3, RAM + 17 for HuC3), checked at once; the content
  (hash, footer) at the verdict. Applying is `save::reload` (a power cycle
  of the running game); the footer time of the applied save is stamped
  back on dumps, so load then dump is byte-identical (with
  `--raw-save`; without it the PC restamps, see "Who syncs the RTC").
- One save buffer (AXI SRAM; 128 KiB + 64 in link-probe, 64 KiB + 64 in the player) serves load and dump: a
  `SaveDumpRequest` drops a load in progress, a `SaveBegin` stops a dump
  being sent.
- `Input`: the bitmap goes to `buttons::Shared::set_pc`; 90 frames (1.5 s)
  without one release the PC buttons.
- The chord (joystick CENTER + B1, debounced, on its rising edge) starts
  an unrequested dump with seq 0.

**Input pins** (`joystick.rs`): LEFT PE3, CENTER PA0, DOWN PF8, UP PF9,
RIGHT PF7 active low with the internal pull-ups (UM2905 names none), B1
PC13 active high with the internal pull-down; sampled once per frame,
`Debouncer<FRAME_SAMPLES>` (2), mapped with `to_buttons`, OR-ed with the
PC's and applied with `set_buttons_at` at the frame's start clock.

**Text on the link.** The probe's text lines (a button echo
`link-probe buttons merged=0x.. joy=0x.. pc=0x..` on every change, for the
human's button check with `read-serial.sh`, and a `status` line every
10 s once the line has been quiet for 3 s) each end with 0x00, so the PC
decoder drops one as a single bad frame, as for the loader's reports. They
are only sent while the session has nothing queued and the TX DMA is
idle, so a line never lands inside a reply or a dump. While `gbc-pc play`
holds the port the human cannot see them: the PC-input part of the
button check needs the `player` image's screen (or a later Stats field).

**Round trip on the board** (`crates/gbc-nucleo-h753/run-save-roundtrip.sh`,
gate B5). The probe's ROM is generated by build.sh (32 KiB, `nop; jr -2`,
MBC3+TIMER+RAM+BATTERY, 32 KiB RAM): no test ROM fits (rtc3test has no
RAM; mealybug's `mbc3_rtc` rewrites the clock). The fixture
`tools/gbc-pc/tests/fixtures/link-probe.sav` (32,816 B: a RAM pattern, the
RTC halted with latched = live, unix time 1,700,000,000) is generated and
checked across 600 frames by `crates/gbc-firmware/tests/save.rs`. 2026-11-26,
NUCLEO-H753ZI, COM5: PASS. The power-on dump differs from the fixture;
`load --sav` "already loaded" then the save in 0.18 s; dumps right after
and 10 s later (~600 frames) `cmp`-equal to the fixture (0.17-0.18 s
each); a fixture + 1 byte got `Nak OutOfRange` at `SaveBegin` and a bad
footer (DH bit 5) `Nak OutOfRange` at the verdict, each followed by a dump
equal to the fixture; gbc-pc counters all 0 on every successful step;
board `overruns=0 framing=0 noise=0 bad_frames=0 replies_dropped=0
rx_high=257`. With `RUN_SAVE_EXPECT` a copy with one byte changed, the
script exits 1 at step 4 (`d1.sav ... differ: byte 1001`). The probe's
frame of `jr -2` takes ~606,000 cycles (1.28 ms) of core time. `gbc-pc
play --seconds 3`: 7 `Input` frames sent, the probe counted `inputs=7`.

## Audio to the PC (ADR 0041), `gbc-pc play --audio-wav`

Opt-in: a board sends no audio until asked, so nothing else changes.

| PC sends | Board answers |
|---|---|
| `AudioStart` | `Ack`, then `Audio` messages (any seq, never answered) until stopped; `Nak UnexpectedType` from an image without audio (link-probe, the player's no-ROM loop) |
| `AudioStop` | `Ack`; no more `Audio` (ones already queued may follow) |

`play` sends the 0x00 flush and `Hello` first: its seqs restart at 0
every run and the board takes a frame byte-identical to the last answered
request as a resend, so after a run that ended without `AudioStop`
(Ctrl-C, a lost frame) the next run's `AudioStart` would otherwise get
the stored `Ack` and the stream would stay off. `Hello` makes the board
forget the last request.

`Audio { first: u32, held: u16, data }`: `data` is the **real** frames
the DAC ISR handed to the DMA, in order (never underrun filler), 3 bytes
each (`gbc_link::audio::pack`: left code = DAC channel 1 in bits 0..=11,
right in 12..=23, little-endian), at most 340 frames; the player sends
one message per DMA half, 256 frames (7.8 ms). `first` is the index of
the first of them among all real frames fed since the board started
(wrapping u32), so consecutive messages chain (`first + frames`) and a
jump is a gap. `held` is the filler frames (the last frame repeated) the
DAC played right after them: 0 unless the board underran.

- The stream stops on `AudioStop`, on `Hello`, and after 90 frames
  (1.5 s) without any decoded frame from the PC (`play` repeats `Input`
  every 500 ms), so it never outlives the PC.
- Transmit order: replies, then the pending `Stats`, then audio, then
  dump frames; whole frames only. While audio streams one TX transfer is
  at most 3,072 B (15.4 ms), so every service call finds the transmitter
  idle.
- The board never waits for the PC: its queue (`AudioTee`, 4 halves =
  31 ms) drops whole halves when full and counts them (`audio_dropped`,
  `audio_dropped_halves` in `Stats`); the PC sees the jump in `first`.

The PC (`tools/gbc-pc/src/audio.rs`): checks the chain (gaps forward are
filled with the last frame, at most 1 s each, so WAV sample i is frame
`first_frame + i`; a jump back is a board reset, no fill), writes the
real frames as a 16-bit stereo WAV at 32,768 Hz, sample = `(code - 2048)
<< 4`, and prints `audio:` lines: the link's received bytes/s,
`first_frame`, frames, messages, gaps, missing, filled, held, and the
board's `audio_*` fields from its last `Stats`.

Measured 2026-11-26 (NUCLEO-H753ZI, COM5, 2 Mbit/s, Pokemon Red intro,
no keys): 60 s, 7,703 messages, 0 gaps, 0 drops, 100,868 B/s received;
a second 30 s run the same. Both captures equal the host's feed
(`headless --paced --dac-feed`) for every frame (1,971,968 and 986,112
frames from `first_frame`).
