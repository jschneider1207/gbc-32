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
