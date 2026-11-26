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
- `Stats` frames (any seq) are printed as they arrive.

`play` does not send `Hello`; it can follow a `load` in a separate run.
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
  back on dumps, so load then dump is byte-identical.
- One save buffer (128 KiB + 64, AXI SRAM) serves load and dump: a
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
