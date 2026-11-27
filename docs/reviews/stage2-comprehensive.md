# Stage 2 comprehensive review (`stage1-m10..d9c546d`)

Requested by the human, 2026-11-27 (session 30). Four fresh read-only
reviewers: **firmware correctness**, **protocol/host correctness**,
**performance and optimizations**, **Rust quality and maintainability**.
Prior items in `stage2-m6.md` / `stage2-m7.md` were excluded. Findings
marked *verified* were re-checked by the orchestrator against the code.
Outcome column: PENDING = not yet triaged into a fix or a plan task.

## Correctness

| # | Sev | Where | Finding | Outcome |
|---|---|---|---|---|
| K1 | med | nucleo loader.rs:216-224, :232-238 | Decode-error Nak is encoded into `self.reply` (the stored reply for `last_req`) without clearing `last_req`; "reply lost, then resend corrupted" makes every later resend get the stale `Nak`, load ends `GaveUp`. `link_session.rs` and `sim.rs` keep them separate. Untested (fault pairs never adjacent) | *verified*; **fixed** a77fca6: the bad-frame Nak has its own buffer; test `a_corrupt_resend_after_a_lost_reply_keeps_the_stored_reply` |
| K2 | med | gbc-pc play.rs:145 + link_session.rs:322-331 | `play` sends no `Hello`, seqs restart at 0; after a run that ended without `AudioStop`, the next `AudioStart` seq 0 is byte-equal to `last_req` → treated as duplicate, audio never starts, PC stuck in Prefill | *verified* (dedup path); **fixed** 724dfe0: `play --audio` opens with flush + `Hello` (no-audio `play` unchanged); test `play_audio_after_a_run_that_ended_without_audio_stop`. board (2026-11-27): `play --audio` killed (`taskkill /F`, no `AudioStop`), 5 s gap (board `audio_timeouts` 0 → 1), then `play --audio --seconds 10`: `hellos` 5 → 6, `audio_on=1`, `audio_sent` 13,565 → 14,984, PC `live: Playing` (0 underruns); `gbc-pc hello`: `HelloReply: protocol 2` |
| K3 | low | gbc-firmware save.rs:90, player.rs:847 | Board-started game without a loaded save dumps footer stamp 0; a later sync runs `Clock::sync(0, now)` (~20,000 days, MBC3 day carry) | **fixed** 895f84d, baf2f61: stamp 0 means unknown, no advance (Clock::sync) |
| K4 | low | gbc-pc play.rs:219 | Chord dump whose `SaveBegin` is corrupted: all `SaveChunk`s dropped, no retry, no save | **fixed** bd3189a: a `SaveChunk` with no job sends one `SaveDumpRequest`; tests `board_dump_with_corrupt_begin_is_requested_again`, `leftover_chunks_of_a_failed_dump_do_not_restart_it` |
| K5 | low | gbc-pc audio.rs:116-119 | WAV size truncates/overflows past ~9.1 h capture | **fixed** 296e1a7: WAV cut at the u32 size limit |
| K6 | low | gbc-pc keyboard.rs:48 | `GetAsyncKeyState` is global: keys typed in other windows reach the game; F5 dumps, Esc quits | **fixed** 9d80f68: keys read only while the console window (or its root owner) is foreground |
| K7 | low | gbc-pc main.rs:101-104 | `--timeout-ms 0` accepted; every request `GaveUp` with no reason | **fixed** d1ac087: `--timeout-ms 0` rejected |
| K8 | low | gbc-pc client.rs:407-420 | Resend on every Nak incl. refusals and `Busy`, no backoff | **fixed** 6738027: refusal Naks fail at once, `Busy` backs off 50 ms x attempts, BadCrc/BadFrame resent; tests `refusal_nak_fails_at_once_without_resend`, `busy_nak_is_resent_after_a_backoff` |
| K9 | low | player.rs:555-571 | Stats > 1016 B dropped uncounted (`Text<1024>`) | already plan task (stage-2.md:295, :601) |
| K10 | low | player.rs:123-127, paced.rs:330 | Live ramp (24 pops, 187 ms) sized for a 2 MiB ROM hash; 8 MiB ~240 ms → startup underruns; the hash is discarded in live mode | **fixed** 1aace5a (board check pending): `Service::HASHES` off also skips the ROM hash (`rom_fnv` = hash of no bytes; scripted/paced/headless headers unchanged); LIVE_RAMP_LEFT kept at 24 as margin until an 8 MiB live start shows 0 underruns with less |
| K11 | low | link_uart.rs:206-220 | RX DMA TEIF/FEIF/DMEIF never checked; a bus error freezes the link silently | **fixed** bea81df (landed under another agent's docs commit by mistake): each read takes EN + TEIF/FEIF/DMEIF; a stopped stream is counted (`rx_dma_errors=` in player Stats and link-probe status) and re-armed (RM0433 15.3.19/15.3.20); B5 and run-save-roundtrip require `rx_dma_errors=0` |
| K12 | low | link_session.rs:477-486 | `Hello` doesn't cancel a dump in progress (harmless today) | **fixed** d8f4698: `Hello` drops the dump; test `hello_drops_a_dump_being_sent` |
| K13 | low | bin/paced.rs:252-254 | Unbounded `wfi` wait for TE edges (probe only) | **fixed** b48b149: SysTick (CPU/8, masked wake-up, never taken) bounds the 30 periods at 4 wraps (~1 s); the field reads `timeout` |
| K14 | low | audio_dac.rs:589 | SAFETY comment: "GPIOA belongs to nobody else" (joystick uses PA0; main thread only, no race) | **fixed** 1db0235: SAFETY comment corrected |

No defects found in: COBS/CRC framing and sizes, baud/BRR math, debouncer
and chord edge, rx_ring/AudioRing/AudioTee ordering, DC blocker, scaler
output, replay lateness, `write_atomic`, serial DCB/COMMTIMEOUTS, link UART
DMA, DAC ping-pong, MDMA/TE handoff, MPU/cache maintenance, QSPI mmap
enter/exit, loader erase/program walk, save check/reload/take.

Untested: loader adjacent faults (K1); two `play --audio` runs (K2);
stamp-0 / future-stamp RTC round trips; frames at exactly MAX_ENCODED
1039/1040 through the dedupe path; Stats + dump + audio queued at once
under the 3,072 B TX cap; worst-case `RomBegin` (8 MiB validate + erase)
vs the PC's 2 s timeout; cold-boot ECC state of NOLOAD AXI buffers; ITCM
`memcpy`/`memset` called before `tcm::place()`.

## Performance and optimizations

| # | Impact | Where | Finding / proposal | Est. gain | Outcome |
|---|---|---|---|---|---|
| P1 | high (fe) | gbc-firmware video.rs:73-158 | Scaler is ALU-bound (74 instr / 8 px; per band `memclr` of `tmp` + 2 copies, *verified*). Convert two pixels per u32 (`(w&0x001F001F)\|((w<<1)&0xFFC0FFC0)\|((w>>4)&0x00200020)`), REV16 to wire order, store the doubled row twice without `tmp` | 196k → ~85-100k cycles/presented frame (~38% of live fe mean) | **fixed** f4b11d1; board (2026-11-27, perf.md "Session 30 review fixes"): `scale_frame` 195,927 / 196,676 / 197,247 → 117,510 / 118,674 / 120,169 (min/mean/max, -39.7%; above the estimate), cgb-acid2 display EQUAL, 0 mismatches; live fe mean Pokemon Red 80 s 244,589 → 167,126, Wario Land 3 120 s 231k → 166,785; fe max -74..85k; 0 underruns |
| P2 | med-low | display.rs:531, paced.rs:417 | Stash `copy_from_slice` (42-45k cycles, generic memcpy, ~46% of frames); swap buffers instead (core allows it after FRAME_READY) | ~20k mean, 45k max | PENDING (board) |
| P3 | low-med | player.rs:432-461, gbc-link crc/cobs | Audio-to-PC framing ~59k cycles/frame while streaming; HW CRC unit or slicing-by-4, block COBS | ~35-40k (opt-in `--audio`) | PENDING |
| P4 | low | audio.rs:83-104, paced.rs:421-434 | Conditioner spills (10 KB `run_with`); out-of-line + SSAT + fuse with push | ~10-15k | **fixed** ba6b626 (board check pending): the frame is neutral `PcmFrame` (ADR 0044), the producer only saturates to i16 (the 12-bit round/clamp/pack moved to the DAC ISR); out-of-line/fuse part not measured; the DAC side's fuse is P5 (4ac67b1) |
| P5 | low | player.rs:150-160, audio_dac.rs:505 | DAC ISR from flash (max 8-10k cycles, counted in `core`); split pop loop, ISRs to ITCM on H735 | few k + less jitter | **partly fixed** ea6e9dc, 4ac67b1 (board checked 2026-11-27): pop loop split (copy n, then fill), and the DAC conversion fused into it (`AudioRing::pop_to` + `HalfOut<U12Lanes>`): no scratch half or memclr, one store per frame, hash/tee read the ring. player DMA1_STR0 0x724 -> 0x4fe + 314 B `map_run` (37 instr per 4 frames), ISR frame 1,088 -> 80 B (pre-Q1 48); paced 0x5f0 -> 0x3fe + 314. board (2026-11-27, perf.md "Session 30 review fixes"): `audio_isr_max` Pokemon Red 8,356 → 2,802 (off), 10,496 → 5,290 (tee on, `--audio`); Wario Land 3 2,936 / 5,410; 0 underruns, 0 dac_restarts, 0 audio dropped; tee pack 107 → 116 cyc/frame (now converts from `PcmFrame`). ISRs to ITCM left for H735 |
| P6 | low-med | build flags | `-align-loops=1` never board-measured: ~4.4-6.3 KB ITCM back | ITCM headroom | PENDING |
| P7 | med (plan) | H735 RAM | DTCM full, AXI 394 KB vs 256 KiB on H735: move FRAME out of DTCM first (measured cheap), keep cart RAM internal, spend save staging buffer, reuse panel MPU region slack (27 KB) for DMA buffers; boot-check `TCM_AXI_SHARED` | planning | PENDING |
| P8 | low | mpu.rs:75-77 | H753 D2 SRAM1-3 (288 KiB) unused; would allow 128 KiB saves | none (speed) | PENDING |

Methodology:
- M1 (med): the +3% ratchet sits on ±1.7% BTAC layout noise (ADR 0042: 0.11% with BTAC off). Add a BTAC-off ratchet pass at ~+0.5-1% (stricter only).
- M2 (med): LTO / opt-level / ITCM-tier decisions are one build each; use 4-8 layout-perturbed builds per variant.
- M3 (low-med): `tools/h7-bench/README.md` claims the bench's cache state matches a real front end; the shipped player doesn't evict like the bench, so QSPI +1.24% is an upper bound. **fixed**: `tools/h7-bench/README.md` Measurement now says the player reads the frame from DTCM and writes non-cacheable memory, so the bench is an upper bound.
- M4 (low): `fe_max`/`busy_max` include the Stats-formatting frame; bench `max` has no frame index or warm-up discard. **bench part fixed**: workload lines end with `max_at=<call>` (no warm-up discarded, ratchet unchanged); `fe_max`/`busy_max` part is not part of this fix.
- M5 (low): 550 MHz projection assumes constant cycles; fixed-ns miss latency (QSPI ~500 cycles at 550 MHz) and the H735's 32 KiB D-cache push opposite ways, unmeasured.

Agreed with recorded outcomes: LTO/opt-level, ITCM tier swaps, stash cache
attribute (M7 P2), QSPI clock/SIOO/TCEN, display cost to the core.

## Rust quality and maintainability

| # | Sev | Where | Finding | Fix (effort) | Outcome |
|---|---|---|---|---|---|
| Q1 | high | gbc-firmware audio.rs:1-25, :117-130; gbc-link lib.rs:150-180 | Shared audio frame and the link's `Audio` wire format are the `DAC_DHR12RD` layout (2×12-bit); `DMA_HALF_FRAMES` in the shared crate. SAI/16-bit codec on Stage 3 touches ring, tee, hash baselines, protocol and PC at once | neutral stereo frame, pack in the ISR, version the audio payload (M-L); decide before Stage 3 baselines | **fixed** ba6b626 (board check pending): `PcmFrame` (i16 pair) in ring/tap/tee/hash, DAC conversion only in `audio_dac::on_dma`, `DMA_HALF_FRAMES` out of gbc-firmware, `Audio` format byte + protocol 2 (ADR 0044, human 2026-11-27); feed baselines regenerated |
| Q2 | high | nucleo loader.rs, link_session.rs; gbc-pc tests `#[path]`, sim.rs | Board-independent protocol servers live in the board crate, host-tested via `#[path]`; resend/dedup/Nak code copied 2-3× (K1 is a symptom) | `gbc-link-board` no_std crate with one shared dedup/responder (M) | **fixed** e1a1972: crate `gbc-link-board` (ADR 0043, human 2026-11-27), one `Responder` for loader, session and sim; 54db549: the sim answers with the real `Loader` and `Session` (one image at a time, faults on the wire only) |
| Q3 | med-high | gate-stage2.sh:362-363 | Board crate linted only with no features and `headless`: `player`, `player-script`, `paced`, `link` bins, `display`/`rom-qspi` cfgs and gbc-pc's `cfg(windows)` modules (20 unsafe blocks) never see `-D warnings` / `undocumented_unsafe_blocks` | one clippy per `build.sh` feature set + windows-gnu target (S, stricter only) | **fixed** ba19ac7: gate lints display, paced, link, rom-qspi, player, player-script + gbc-pc windows-gnu (all clean) |
| Q4 | med | 11 copies (`Slot<T>` + `unsafe impl Sync` + `TAKEN`) | Hand-repeated take-once static pattern | one audited `TakeOnce<T>` or macro (S-M) | **partly fixed** aaa147c, 762e865 (`take_once!` macro, src/take_once.rs; guard kept out of NOLOAD in `.bss`; host tests in tools/memory-x/tests/take_once.rs); audio_dac.rs done 4ac67b1 (buffer from `take_once!` in `AudioDac::new`, kept as an AtomicPtr for start/halt/ISR); bin/player.rs, bin/paced.rs done e14529c (nm on player.elf: the six guards in `.bss`, the buffers keep their `.axisram` sections); left: bin/dac-probe.rs `LOG` (owned elsewhere), tools/headless-qemu (no board-lib dependency) |
| Q5 | med | audio_dac.rs, lcd_dma.rs, qspi.rs, link_uart.rs (138 `w.bits(`) | Raw bit constants where the PAC has typed field writers | use PAC fields while porting (M-L) | PENDING |
| Q6 | med | qspi.rs, lcd_dma.rs, audio_dac.rs, lcd.rs | "No ISR touches RCC/GPIO/QUADSPI" restated in ~10 SAFETY comments; RCC RMWs in 6 modules, no critical section | `rcc::enable()` under `interrupt::free` + one ISR table (S-M) | **partly fixed** aaa147c (`rcc::modify` under `interrupt::free` for every RCC enable/reset RMW; ISR table in lib.rs docs, SAFETY comments point to it); audio_dac.rs done 4ac67b1 (`rcc::modify`, GPIOA SAFETY points to the table, table's DMA1_STR0 row corrected: pop_to/offer_popped, DAC SR); left: paced_board.rs, bin/player.rs, bin/paced.rs, bin/dac-probe.rs (owned elsewhere); fault.rs on purpose (PRIMASK set for good) |
| Q7 | med | qspi.rs:655-700 `pll2_on` | QSPI driver owns PLL2 and stops it if running (*verified*); kills any other PLL2 consumer (SAI on H735) | clock plan in `clock.rs` (S-M) | **fixed** 2ffa51c (board check pending) |
| Q8 | med | qspi.rs (1285 lines) | QUADSPI controller and MX25L6433F command set mixed; H735 has OCTOSPI | split controller / NOR (M) | **fixed** ae528a0 (board check pending) |
| Q9 | med | bin/headless.rs vs paced_board.rs, player.rs, h7-bench | `Dwt`, `Capture`, QSPI map+validate (3×), bounded `fmt::Write` (~8×), two display sinks (~390 lines) duplicated | shared `FmtBuf<N>`, `qspi::map_rom_image()`, one display sink (M) | **partly fixed** 3794f7f, 9ddc6a9, 6e8fc01, 8583268, e14529c, c2aac36: `gbc_firmware::fmt_buf` (`FmtBuf<N>` truncating, `reserving_nul` + `push_nul` for 0x00-terminated link text, `append_truncated`, `append_whole`; host tests, mutation-checked) replaces the buffers in player (`Text`, `Why`), link-probe, loader, qspi-write, lcd-dma-probe and both `Capture`s, each with its old truncation point; headless takes `Dwt`/`Capture` from paced_board and `paced_board::send_lines` echoes the capture for both reports; `qspi::map_rom_image()` (`RomImageError::stage()`/`error()`) is the map+validate in headless-qspi, player and h7-bench, lines unchanged. Display sinks (c2aac36): the M3 measurement sink moved to `display::measure` (src/display/measure.rs, cfg `display`), bin/headless.rs only selects it (`measure::init()`, its own EXTI15_10; 818 -> 416 lines); it stays its own type (blocking per-frame sentinel + full readback, four start modes, vs the never-waiting TE-paced sink with stash and sampled rows) and shares with `Display` the panel set-up (`panel`), the 2-pixel control (`control_readback`), one `Range` (n/min/max/sum) and the `first_bad`/`first_error` fields. Board: headless-display-cgb-acid2 report identical with digits masked (bytes=38029 both; setup_mean 319 -> 294, other cycles within noise), 602 lines EQUAL, self-test passed; player-script replay cgb-acid2 PASS (readbacks=2, mismatches=0, control 2). .itcm 64,088 B unchanged, ITCM checks OK; .text headless-display -496, player +136, player-script +256, paced +40 B. Left: dac-probe `Out` (owned elsewhere). .text (flash; .itcm unchanged, ITCM checks OK) player +256, headless +920, headless-qspi +1104, headless-display +824, link-probe +128, loader +1368 B, from inlining shifts (e.g. `send_lines` 912 B out of line, loader `main` absorbing `Qspi::write_op`), not new logic; board check pending |
| Q10 | med-low | bin/headless.rs:83-88 | `rom-qspi,display` defines `IMAGE` twice (E0428, *verified*) | `compile_error!` or separate bins (S) | **fixed** ba19ac7: compile_error! in headless.rs |
| Q11 | low-med | 6 places | Baud 2,000,000, `parse_baud`, NOR sector sizes, `IMAGE_HEADER_LEN`, memory map in build.sh duplicated | single source (S) | (client.rs `IMAGE_HEADER_LEN` now `gbc_firmware::rom_image::HEADER_LEN`, 6738027; NOR `SECTOR_BYTES`/`BLOCK_BYTES` once in `gbc_link_board::loader`, qspi.rs re-exports, e1a1972; **fixed** 8974629: `gbc_link::DEFAULT_BAUD` and `gbc_link::parse_baud` (one copy), scripts' literals checked by gbc-link tests/default_baud.rs, both build.sh memory maps checked against memory-x by tools/memory-x/tests/build_sh_map.rs) |
| Q12 | low | gbc-pc fnv.rs | Own FNV; comment says gbc-firmware isn't a dependency, but it is (*verified*) | use `gbc_firmware::fnv` (S) | **fixed** 7fd6c86: `gbc_pc::fnv` re-exports `gbc_firmware::fnv` |
| Q13 | low | 3× tests/hard_rules.rs | Same ~440-line checker copied (a real tokenizer, not grep) | dev-dep crate (M; gbc-core part needs human) | **fixed** 0dfdcc0 (firmware, link, link-board); gbc-core converted too (human-approved dev-dependency, 2026-11-27; this commit) |
| Q14 | low | link_session.rs:446-450, loader.rs:220,252 | `encode(..).unwrap_or(0)` silently sends nothing | const-assert reply sizes, count failures (S) | **fixed** 764f80e: `REPLY_FRAME_MAX <= REPLY_MAX` const assert + `every_reply_fits_the_reply_buffer` tests; failures counted in `replies_dropped` |
| Q15 | low | gbc-pc lib.rs | `pub mod sim` ships in the exe | cfg-gate (S) | **fixed** c7739b4: `sim` feature, enabled by a self dev-dependency; df3d99c: docs name `crate::sim` and the cfg(windows) modules as plain text, `cargo doc -p gbc-pc` warning-free on host and windows-gnu |
| Q16 | low | Cargo.toml, ADR 0036, lib.rs | Comment/doc drift ("h7-bench will depend", "bins hold only `#[entry]`") | S | **fixed** 1db0235 (Cargo.toml), e1a1972 (board lib.rs docs), ADR 0043 amends ADR 0036's "bins hold only `#[entry]`" |
| Q17 | low | 7 probe bins (~3.2k lines) | Maintenance cost; decide which stay for Stage 3 | S | **fixed** (docs only): all seven stay. Each is built by `build.sh` and is a documented bring-up/diagnostic procedure; `link-probe` is also used by the gate and run scripts, `qspi-mmap` by `run-loader-roundtrip.sh`. None is dead, none removed. `crates/gbc-nucleo-h753/README.md` "Probe bins" table says what each is for and whether Stage 3 reuses it |
| Q18 | low | pedantic clippy | 144 / 29 / 192 warnings (firmware/link/pc), mostly docs/must_use/casts | optional | — |

Overall (quality reviewer): good health for a 40k-line stage; gbc-firmware
and gbc-link are clean no_std and forbid unsafe, board boundary is
trait-based, no `static mut` besides linker symbols, linted unsafe all has
specific SAFETY comments, MPU centralised, references cited. Debt is
structural: board-independent logic in/copied into board code, peripheral
and clock ownership resting on prose, DAC-specific formats in shared crates
and the wire protocol. Settle Q1, Q2, Q3, Q6, Q7 before Stage 3.
