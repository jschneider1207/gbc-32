# Stage 2 M4 acceptance: board evidence

What the M4 acceptance (a 10-minute audio-paced run with the display and
the DAC on: 0 underruns, speed within +-0.1%, the DAC-feed hash equal to
the host's) rests on (M4 review T-2: the captures were not committed and
the docs cited pre-rebase commits). NUCLEO-H753ZI + X-NUCLEO-GFX02Z1,
ST-LINK VCP, CPU 472 MHz, 2026-11-26.

**Commit.** The two images were built from code identical to main's
`6485873` (`paced-<rom>` image) and `fab1e5a` (`run-paced.sh`); the
original build commits (`eea7a18`, `9d50db7`) were rebased into those.
Image cksums (POSIX `cksum`, bytes), as printed by the images:

| Image | bytes | cksum |
|---|---|---|
| `paced-cgb_sound.bin` | 302,248 | 877509939 |
| `paced-worst-case.bin` | 269,480 | 3145285527 |

The `.bin` files and the synthetic ROM are not committed: rebuild with
`crates/gbc-nucleo-h753/build.sh` (`paced-<rom>`; for the synthetic ROM,
`bench --write-synth worst-case out/worst-case.gb`, 32,768 B) and compare
the cksum.

## Run commands

```
cargo build --release -p headless
cd crates/gbc-nucleo-h753
RUN_PACED_SAVE=<serial capture> ./run-paced.sh out/paced-cgb_sound.bin ../../test-roms/blargg/cgb_sound/cgb_sound.gb
RUN_PACED_SAVE=<serial capture> ./run-paced.sh out/paced-worst-case.bin out/worst-case.gb
```

Each ran the host `headless <rom> --frames 35837 --paced`, flashed, read
the port 660 s and took the last complete report; both exited 0 (PASS).

## Files (`docs/reference/m4-evidence/`)

- `run-paced-<rom>.log`: run-paced.sh's output (host path made `<repo>`).
- `report-<rom>.txt`: the last complete report of the serial capture
  (each capture is about 165 KB, over the 100 KB limit; the whole run
  re-sent the same report 70 times), cut exactly at the script's framing:
  from the `paced report n=<k>` line to its `paced report-end n=<k>` line,
  whose `bytes=` and `cksum=` match the lines between them (checked by
  `scripts/test-run-paced.sh`).
- `scripts/test-run-paced.sh` replays these reports through run-paced.sh
  (the image fields swapped for a synthetic image, since the `.bin` is not
  committed) and expects exit 0, plus every failure path.

## cgb_sound (frames 35,837)

```
paced v1 rom_bytes=65536 rom_fnv=228d75ac28477026 input_fnv=cbf29ce484222325 frames=35837
paced end frames=35837 clock=2516614644 ready=35834 fbs=ffe7f333d13ab726 feed_frames=19661051 feed=a42b9bdd2e459ea5
paced drain feed_frames=19661051 feed=a42b9bdd2e459ea5
stats paced ring min_fill=1039 max_fill=2047 underrun_events=0 underrun_frames=0 overrun_events=0 overrun_frames=0
stats paced drain underrun_events=1 underrun_frames=5
stats paced speed elapsed=283180281665 ppm=+82 steady_ppm=+9
stats display paced presented=25788 skipped=10046 transfers=25788 transfer_errors=0 stuck=0 aborts=0 te_edges=35920 scale_mean=193289 scale_max=194739 present_mean=339 present_max=412
stats dac refills=76872 late=0 spurious=0 te=0 dme=0 fe=0 udr1=0 udr2=0 isr_min=552 isr_max=8110 stop=ok
```

## synth:worst-case (frames 35,837)

```
paced v1 rom_bytes=32768 rom_fnv=8813cb5d7ea113b3 input_fnv=cbf29ce484222325 frames=35837
paced end frames=35837 clock=2516611398 ready=35836 fbs=329470c6bc19a93f feed_frames=19661026 feed=4b3d013ebbd78714
paced drain feed_frames=19661026 feed=4b3d013ebbd78714
stats paced ring min_fill=1243 max_fill=2047 underrun_events=0 underrun_frames=0 overrun_events=0 overrun_frames=0
stats paced drain underrun_events=1 underrun_frames=30
stats paced speed elapsed=283183015155 ppm=+72 steady_ppm=+18
stats display paced presented=26231 skipped=9605 transfers=26231 transfer_errors=0 stuck=0 aborts=0 te_edges=35915 scale_mean=202312 scale_max=203990 present_mean=362 present_max=378
stats dac refills=76872 late=0 spurious=0 te=0 dme=0 fe=0 udr1=0 udr2=0 isr_min=556 isr_max=8290 stop=ok
```

The `drain` hash is the stream as the DAC ISR wrote it into the DMA half
buffer (consumer side), not a read of the DAC output.
