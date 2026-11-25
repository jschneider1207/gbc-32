# Unpinned behaviours

Emulated behaviour that no test ROM in the manifest would catch if it were
wrong, or that rests on one ROM or one side of a disagreement between
references. Each has a `UNPINNED:` comment at its site in
`crates/gbc-core/src` (`rg -n 'UNPINNED:' crates/gbc-core/src`) and one row
here. `crates/gbc-core/tests/unpinned.rs` fails when the tags and the rows
differ (id, class, file), so add or remove both together. Origin: M10
review E7 (`docs/reviews/stage1-m10/E-holistic.md`).

This is a list of risks, not of bugs: nothing here is known to be wrong.
When a real game misbehaves in one of these areas, look here first. A row
goes away when a ROM in the manifest (or a hardware capture cited at the
site) would fail with the behaviour changed.

Classes:
- **a**: follows one reference (or an assumption), and no ROM in the
  manifest fails if it is wrong;
- **b**: pinned by exactly one ROM (or one ROM pair);
- **c**: the references disagree and one was chosen, with no ROM that
  decides.

Tag form, in a comment: `UNPINNED: <id> (<class>): <what it follows, who
disagrees, what would settle it>`.

Last column: always `tagged` (the test accepts nothing else; rows for
sites that could not be tagged yet were once marked pending there).

Not on the list: `docs/reference/audio-aliasing.md` (a quality limit, not
a behaviour in doubt), constants that several hardware captures bracket
(e.g. `hdma::HBLANK_DELAY`), and known deviations that are plan tasks.

## CPU, timer, speed switch, STOP

| Id | Class | Where | Behaviour | Follows | Disagrees | Would settle it | ADR, sign-off | Tag |
|---|---|---|---|---|---|---|---|---|
| `spsw-div-reset-point` | c | `bus/stop.rs` `speed_switch` (STOP mode: `stop_freeze`) | STOP resets DIV after its last byte: after the second byte when nothing is pending | AGE `gb_bus::execute_stop` (verified on CGB B/C) | SameBoy `enter_stop_mode` (CGB E): at the end of the fetch, held 4 T-cycles with IME off (the same counter) | nothing by itself; with the row below, an IME-on timer ROM | ADR 0020 amendment 2026-11-24 (human approved); `speed-switch.md` | tagged |
| `spsw-ime-on-timer` | c | `bus/stop.rs` `speed_switch` | IME on, nothing pending: the timer after the switch is the same as with IME off | AGE's model (verified on CGB B/C with IME off, or relative to an interrupt) | SameBoy (CGB E; its source distrusts the condition): 4 T-cycles ahead of this | `ei`, two-byte switch with IE = 0, then a DIV or 262 kHz TIMA read at a fixed M-cycle | ADR 0020 amendment 2026-11-24 (human approved) | tagged |
| `spsw-reset-edges` | a | `bus/timer_io.rs` `div_reset_as` | Serial clock edge at STOP's DIV reset (after the second byte: a bit that rose in it falls); 4 kHz TIMA and frame-sequencer edges of a one-byte STOP's reset | the plain DIV-reset rule (AGE for the serial edge) | none known | a ROM with a serial transfer or a 4 kHz timer across the reset | `speed-switch.md` "Measured" | tagged |
| `stop-mode` | a | `bus/stop.rs` `stop_freeze` (also `stop_enter`, `stop_wake`) | STOP mode: counter frozen, PPU and APU channels run on, OAM DMA and HDMA paused, wake on a selected P1 line, two idle M-cycles | SameBoy's CGB path, Pan Docs | none known (no ROM enters plain STOP) | a hardware ROM reading DIV and TIMA around a STOP ended by a button | ADR 0020 STOP mode amendment; human 2026-11-24 "Thats fine for stop" | tagged |
| `stop-mode-screen` | a | `bus/stop.rs` `stop_frame` | Black whole frames in STOP mode unless it began in mode 3 or with palettes blocked | SameBoy (per pixel), Pan Docs | none known | a photo or capture of the screen in STOP mode | ADR 0020 STOP mode amendment | tagged |
| `post-boot-bcps-ocps-cgb` | a | `bus/reset.rs` `Sys::reset` | BCPS and OCPS are 0 after the boot ROM in CGB mode | nothing (DMG-mode values are Mooneye's) | - | a `boot_hwio`-style ROM with a CGB-mode header on hardware | - | tagged |

## OAM DMA, HDMA

| Id | Class | Where | Behaviour | Follows | Disagrees | Would settle it | ADR, sign-off | Tag |
|---|---|---|---|---|---|---|---|---|
| `dma-speed-switch-retime` | a | `bus/oam_dma.rs` `dma_retime` | A speed switch mid-transfer: the bytes left keep their M-cycle count at the new speed | SameBoy (`dma_cycles`) | none known; only two gambatte ROMs run it, to double speed | a cgb04c capture of a switch to single speed (and one with an interrupt pending) mid-transfer | `oam-dma.md` "Unverified"; review A4-11 | tagged |
| `dma-restart-ppu-window` | c | `bus/oam_dma.rs` `oam_dma` (FF46) | A restart: the PPU sees the old transfer stop at the write and the new one start 2 dots before its byte 0 | own (the `late_sp*` lead applied to a restart) | SameBoy: active throughout | a `late_sp*`-style ROM with a restart around the OAM scan | `oam-dma.md` "Unverified" | tagged |
| `hdma-cancel-nonzero` | c | `hdma.rs` `write_hdma5` | Cancel with a block already triggered: that one block runs, whatever the written length | gambatte (captures cancel with 00 only) | SameBoy: value + 1 blocks | a capture cancelling with 01 | review A4-2; STATUS "Needs human" | tagged |
| `hdma-rearm-in-hblank` | c | `hdma.rs` `write_hdma5` | FF55 bit 7 written while an HBlank transfer is active, inside the HBlank: a second block on that line | SameBoy | gambatte: only the length is stored | a ROM writing FF55 = 8n twice in one HBlank and reading the destination | review A4-6 | tagged |

## APU

| Id | Class | Where | Behaviour | Follows | Disagrees | Would settle it | ADR, sign-off | Tag |
|---|---|---|---|---|---|---|---|---|
| `apu-trigger-ss-odd-grid` | a | `apu/mod.rs` `trigger_delay` | Single-speed square trigger on the odd CPU grid (after a switch back): first step on the absolute 1 mod 4 grid, not the power-on phase | own fit, gambatte's shape | the double-speed branch of the same function (review A3-4) | a cgb04c capture of a trigger after two switches | ADR 0021; `apu.md` | tagged |
| `apu-trigger-ds-delay-phase` | b | `apu/mod.rs` `trigger_delay` | Double-speed trigger while the frame sequencer is delayed (`Fs::ds_delay`): `lf_div` flipped | gambatte `speedchange_ch1_nr4init_duty0_pos6_to_pos7_timing_1`/`_2` | AGE delays only its frame sequencer | another double-speed trigger capture with the flag set | `apu.md` | tagged |
| `sq-nrx4-window-edges` | a | `apu/square.rs` `write_nrx4` | The NRx4 high-bits window at a 4-clock period (steps 1 clock late) and right after a trigger (reload counted one period before the first step) | own extension of the SameSuite window | none known | `channel_1_freq_change_timing`-style ROMs at f = 0x7FF and after a trigger | `apu.md` | tagged |

## Cartridge

| Id | Class | Where | Behaviour | Follows | Disagrees | Would settle it | ADR, sign-off | Tag |
|---|---|---|---|---|---|---|---|---|
| `mbc3-latch-edge` | c | `cart/mod.rs` `Cart::write_rom` (MBC3, 6000-7FFF) | The RTC latches when bit 0 goes 0 -> 1 | Pan Docs | SameBoy: every write | a ROM writing 01 twice and comparing the latched seconds | review A4-9; plan task | tagged |
| `filler-header-huc` | c | `cart/mod.rs` `MapperKind::from_cart_type` | A ROM-only image with 0147 = FE/FF (padding) loads as HuC3/HuC1 | Pan Docs' type table | SameBoy and no-MBC boards ignore 0147 | a human decision, not a capture | ADR 0031; STATUS "Needs human" | tagged |
| `huc1-registers` | a | `cart/mod.rs` `Cart::write_rom` (HuC1) | HuC1: register widths (6, 3 bits), no RAM enable, IR register reads C0 | SameBoy, Pan Docs | none known (no HuC1 ROM) | a HuC1 cartridge on hardware | ADR 0032; `hudson-mappers.md` | tagged |
| `huc1-ir-select-nibble` | a | `cart/mod.rs` `Cart::write_rom` (HuC1) | IR select compares the low nibble (1E-FE select IR too) | SameBoy | Pan Docs names only 0E | a write of 1E, then a read of A000 | `hudson-mappers.md` | tagged |
| `huc1-rom-bank-0` | a | `cart/mod.rs` `Cart::write_rom` (HuC1) | ROM bank 0 maps bank 0 at 4000-7FFF | SameBoy | Pan Docs silent | a read of 4000 after writing 00 | `hudson-mappers.md` | tagged |
| `huc3-ir-read-bits` | c | `cart/mod.rs` `Cart::read_huc` (mode E) | HuC3 IR register reads C0 | Pan Docs (as HuC1, D7 open) | SameBoy: 00 | a mode E read on a cartridge | `hudson-mappers.md` | tagged |
| `huc3-unmapped-read` | c | `cart/mod.rs` `Cart::read_huc` (other modes) | HuC3 modes 1-9, B, F read FF | Pan Docs ("often $FF") | SameBoy: 01 | reads in those modes on a cartridge | `hudson-mappers.md` | tagged |
| `huc3-clock-protocol` | a | `cart/huc3.rs` module | The whole clock chip: mailbox, commands, location map | Pan Docs, SameBoy for the gaps | see the rows below | a HuC3 cartridge on hardware | ADR 0032; `hudson-mappers.md` | tagged |
| `huc3-day-width` | c | `cart/huc3.rs` `Huc3RtcState::days` | Day counter is 16 bits | SameBoy, the save footer | Pan Docs: 12 bits (13-15) | reading 16 after the days pass 4095 | ADR 0032 | tagged |
| `huc3-minute-overflow` | a | `cart/huc3.rs` `add_minutes` | A minute value of 1440-4095 counts to 4095 and wraps without a day carry | the MBC3 rule, by analogy | SameBoy would count on | setting it on a cartridge and reading over time | `hudson-mappers.md` | tagged |
| `huc3-command-on-semaphore` | c | `cart/huc3.rs` `write_command` | The command runs at the semaphore write | Pan Docs | SameBoy: at the mode B write | a mode B write and a mode C read with no semaphore write | `hudson-mappers.md` | tagged |
| `huc3-response-high-bits` | c | `cart/huc3.rs` `read_response` | Mode C reads 80 + the command in bits 4-6 | Pan Docs | SameBoy: bits 4-7 clear | a mode C read on a cartridge | `hudson-mappers.md` | tagged |
| `huc3-semaphore-read` | c | `cart/huc3.rs` `read_semaphore` | Mode D reads 81, never busy | Pan Docs (bit 0), own (bits 1-7) | SameBoy: 01 | mode D reads right after a command | `hudson-mappers.md` | tagged |
| `huc3-command-2` | a | `cart/huc3.rs` `execute` | Command 2 writes without incrementing the address | SameBoy | Pan Docs: purpose unknown | commands 2 then 1 on a cartridge | `hudson-mappers.md` | tagged |
| `huc3-time-buffer` | c | `cart/huc3.rs` `extended` | Locations 00-06 are a buffer; extended commands 0 and 1 copy to and from the counters | Pan Docs | SameBoy: 00-06 are the live counters | reading 00-06 twice a minute apart with no extended command 0 | `hudson-mappers.md` | tagged |
| `huc3-set-time-restarts-minute` | a | `cart/huc3.rs` `extended` | Setting the time restarts the minute | assumption (MBC3 seconds write) | - | timing the next minute after a set | `hudson-mappers.md` | tagged |
| `huc3-counter-direct-write` | a | `cart/huc3.rs` `write_location` | Writes to locations 10-16 are ignored | assumption | - | a write to 10, then extended command 0 and a read | `hudson-mappers.md` | tagged |

## PPU

| Id | Class | Where | Behaviour | Follows | Disagrees | Would settle it | ADR, sign-off | Tag |
|---|---|---|---|---|---|---|---|---|
| `obj-palette-power-on` | c | `bus/reset.rs` `Sys::reset` | OBJ palette RAM is zero after the boot ROM | own (determinism) | the cgb04c capture shows that console's 0x21926C (gambatte `scx_during_m3_spx2*`) | nothing: random on hardware | human sign-off (STATUS: stretch group) | tagged |
| `wx0-window-grid` | c | `ppu/render.rs` `trigger_skip` | WX = 0 with SCX & 7 = f: the window loses 7 + f pixels | Pan Docs | gambatte's model: 7; SameBoy: 8 + f | a CGB capture, WX = 0, f = 1..=7, distinguishable columns: column 7, 7 + f or 8 + f at x = 0 | ADR 0034 (human: "go with your judgement"); STATUS "Needs human" | tagged |
| `wx0-window-grid-stalls` | c | `ppu/mod.rs` `sorted_object_stalls` | The object stalls use the same 7 + f grid | Pan Docs, via the renderer | SameBoy: one dot less, or 11 against 6 (oracle exclusion, 892 of 1,348 lines) | the same capture, or mode-0 dots with an object | ADR 0034; `mode3-oracle.md` disagreement 1 | tagged |
| `wx0-discard-wrap-pixels` | a | `ppu/render.rs` `trigger_skip` | After an SCX write that wraps the discard (n >= 8) at WX = 0, the cut stays 7 + n % 8 | not modelled | SameBoy's loop: 8 + n | a capture of such a line | `mode3-oracle.md`; plan, open edge (e) | tagged |
| `wx0-discard-after-window` | a | `ppu/mod.rs` `hold_fetch` | At WX = 0 the fine-scroll discard's compares wait for the window's first tile (7 dots) | SameBoy, the DMG netlist, Pan Docs | gambatte's model (window after the discard) | CGB: WX = 0, SCX 1, SCX = 0 written at 89..=97; mode 0 at 263, 271 (at 96), 264 | `mode3-oracle.md`; STATUS oracle note (c) | tagged |
| `x0-object-before-discard` | a | `ppu/mod.rs` `hold_fetch` | Objects at X = 0 are fetched before the fine-scroll discard | SameBoy, the DMG netlist, Pan Docs | none known (no ROM reaches it) | object at X = 0, SCX = 1 written at 89..=99: mode 0 at 268 or 267 | `mode3-oracle.md`; STATUS oracle note (b) | tagged |
| `discard-wrap-window-write` | a | `ppu/mod.rs` `set_stalls` | A later window write on a line whose discard wrapped rebuilds the stalls without the junk-tile correction or the kept WX = 0 dot | not modelled | SameBoy's loop | a capture, or the oracle sweep extended to it | plan, open edge (d) | tagged |
| `window-zero-pixel` | b | `ppu/mod.rs` `window_zero` | Window reactivation zero pixel on CGB, and which pixel after `drawn` it counts from (0-5 give the same manifest) | Mealybug `m3_wx_4_change_sprites` (CGB C capture) | SameBoy: DMG only | a second ROM with the WX write at another pixel phase | review A2-08 (rejected as a finding; kept as single-ROM) | tagged |
| `window-retrigger-once` | a | `ppu/mod.rs` `retrigger` | One window re-trigger per line, none with 10 objects on it | approximation of SameBoy | SameBoy: any number | a ROM toggling LCDC.5 three times in a line | review E7 | tagged |
| `wy-lead-phase` | a | `ppu/mod.rs` `WY_LEAD`, `WY_LEAD_DS` | A mode-3 WY write takes effect 5 dots (double speed: 1) before the window's stall, at every phase | constant fitted to gambatte `late_wy_*` at their phase | SameBoy: an 8-dot grid from the LCD-on clock (1-8 dots) | `late_wy`-style captures at the other 3 CPU phases (lcd offsets) | review E7 | tagged |
| `stat-during-dma` | c | `ppu/mod.rs` `stat_during_dma` | STAT mode 2 reads as 0 while an OAM DMA runs | SameBoy `GB_STAT_update` | gambatte: no such rule | a 20-line ROM polling STAT from the DMA wait loop outside VBlank, on hardware | reviews E7, A2-11; `oam-dma.md` "STAT" | tagged |
| `dma-fetch-read-dot` | a | `ppu/mod.rs` `fix_attrs` | The mode-3 object fetch reads OAM `d - 5` dots into its `d`-dot stall (it matters during an OAM DMA) | SameBoy `oam_read`, Pan Docs (what is read); own (when) | none known | a ROM with an OAM DMA running through mode 3 | review A2-11; `oam-dma.md` | tagged |
| `oam-scan-dma-bus` | c | `ppu/mod.rs` `scan_to` | OAM scan during a DMA compares the last Y and X read (a covering Y makes every entry an object) | SameBoy `mode2_y_bus` | Pan Docs: "most PPU revisions read each object as being off-screen" | a ROM whose last-read Y covers the line while a DMA runs through mode 2 | `oam-dma.md` | tagged |
| `post-boot-pos-dmg-mode` | a | `ppu/mod.rs` `POST_BOOT_POS` | The PPU's post-boot position (144:171) for DMG-mode cartridges | gambatte `display_startstate/*` (CGB-mode cartridges) | none known; Mooneye passes at 155-171 | a `display_startstate`-style ROM with a DMG-mode header on a CGB | ADR 0014; ADR 0022 (DIV does differ by mode); review A2-11 | tagged |
| `ly0-ds-dot4-mode1-clear` | c | `ppu/mod.rs` `mode_source` | Line 0, double speed, dot 4: a STAT write clearing the mode-1 select unblocks the mode-2 pulse | SameBoy's level model | gambatte: its latch keeps the block | a `late_m1disable_ly0_ds` capture with the write at dot 4 (`lcdoffset1` grid) | review E3; plan task; `stat-irq.md` | tagged |
| `ly0-ds-dot4-stat-10-30` | c | `ppu/mod.rs` `write_stat` | Line 0, double speed, dot 4: STAT 10 -> 30 requests an interrupt | own (kept: "no ROM tells") | gambatte: none | a `late_enable_ly0_ds` capture with mode 1 selected | review E3; plan task; `stat-irq.md` | tagged |
| `ly153-zero-if-read` | a | `ppu/edges.rs` `L153_ZERO_WALK` | An IF read, interrupt check or acknowledge on 153:12 doesn't see line 153's LYC = 0 request (the point is at 13) | gambatte's model; write side: CGB04C `lycint152_lyc0irq_ifw_1`/`_2`, `_late_retrigger_ds_1`/`_2` | SameBoy: at 12 | a `lycint152_lyc0irq`-style capture at lcd offset 3 | ADR 0033; `stat-irq.md` "Open" | tagged |
| `l153-ds-write-before-point` | c | `ppu/mod.rs` `point_before_write` | Double speed: a STAT or LYC write at 153:12 that ends the match loses line 153's LYC = 0 interrupt (single speed keeps it) | own (no rule) | gambatte's latch keeps the interrupt | a `lyc0_ff4{1,5}_disable_ds` capture landing on 153:12 | `stat-irq.md` "Open" | tagged |
| `m0-latch-first-line` | a | `ppu/mod.rs` `m0_latched` | The first line after LCD enable has the mode-0 latch of lines 0-143 (STAT writes 1-2 dots, LYC writes 1-6 dots before its mode-0 interrupt; double speed 1 and 2) | gambatte's model (the latch is against the mode-0 event's time, on every line) | none known | an `m0enable/disable`-style capture on the line after an LCD enable | M10 review (m0 latch); `stat-irq.md` | tagged |
