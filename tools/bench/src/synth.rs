//! Synthetic benchmark ROMs, hand-assembled SM83 built in memory (no ROM
//! files in git). Each one stresses a path the test-ROM benchmarks miss
//! (docs/perf.md). All are 32 KiB, no MBC, CGB-only (except
//! `dmg-raster`, which runs in DMG compatibility mode), with a valid header
//! and checksums; execution starts at 0x0100 as the CGB boot ROM leaves it.

/// The names `bench synth:<name>` accepts.
pub const NAMES: &[&str] = &[
    "event-storm",
    "dma-storm",
    "halt-irq",
    "worst-case",
    "cgb-raster",
    "apu-writes",
    "dmg-raster",
    "oam-lines",
    // The storms ([`storm`]): measured, not benched (docs/perf.md,
    // "Outside the bound").
    "storm-control",
    "storm-apu",
    "storm-spsw",
    "storm-div",
    "storm-gdma",
    "storm-ppu",
];

/// Nintendo logo (header 0x104-0x133), checked by the real boot ROM.
const LOGO: [u8; 48] = [
    0xCE, 0xED, 0x66, 0x66, 0xCC, 0x0D, 0x00, 0x0B, 0x03, 0x73, 0x00, 0x83, 0x00, 0x0C, 0x00, 0x0D,
    0x00, 0x08, 0x11, 0x1F, 0x88, 0x89, 0x00, 0x0E, 0xDC, 0xCC, 0x6E, 0xE6, 0xDD, 0xDD, 0xD9, 0x99,
    0xBB, 0xBB, 0x67, 0x63, 0x6E, 0x0E, 0xEC, 0xCC, 0xDD, 0xDC, 0x99, 0x9F, 0xBB, 0xB9, 0x33, 0x3E,
];

/// Timer and serial event storm in double speed: switch speed (KEY1 +
/// STOP), then TIMA at 262 kHz with TMA=FF (an overflow and reload every
/// 8 base clocks), and the serial port re-armed on the CGB fast internal
/// clock (SC=83) as soon as each transfer ends. IE=0, so only the event
/// scheduler and the polling loop run.
const EVENT_STORM: &[u8] = &[
    0xF3, //             DI
    0x3E, 0x01, //       LD A,01
    0xE0, 0x4D, //       LDH (KEY1),A    arm the speed switch
    0x10, 0x00, //       STOP
    0x3E, 0xFF, //       LD A,FF
    0xE0, 0x06, //       LDH (TMA),A
    0x3E, 0x05, //       LD A,05
    0xE0, 0x07, //       LDH (TAC),A     on, 262 kHz
    // wait:
    0xF0, 0x02, //       LDH A,(SC)
    0x87, //             ADD A,A         carry = transfer running
    0x38, 0xFB, //       JR C,wait
    0x3E, 0x83, //       LD A,83         start, fast clock, internal
    0xE0, 0x02, //       LDH (SC),A
    0x18, 0xF5, //       JR wait
];

/// Back-to-back OAM DMA from WRAM with the CPU running from ROM: start a
/// DMA (FF46=C0), then copy ROM bytes to WRAM for 20 iterations of an
/// 8 M-cycle loop (just over the 160 M-cycle transfer), and restart.
const DMA_STORM: &[u8] = &[
    0x21, 0x00, 0x00, // LD HL,0000
    0x11, 0x00, 0xC1, // LD DE,C100
    // dma:
    0x3E, 0xC0, //       LD A,C0
    0xE0, 0x46, //       LDH (DMA),A
    0x06, 0x14, //       LD B,20
    // copy:
    0x2A, //             LD A,(HL+)      ROM read during DMA
    0x12, //             LD (DE),A       WRAM write during DMA
    0x05, //             DEC B
    0x20, 0xFB, //       JR NZ,copy
    0x21, 0x00, 0x00, // LD HL,0000
    0x11, 0x00, 0xC1, // LD DE,C100
    0x18, 0xED, //       JR dma
];

/// HALT woken by the timer IRQ: TIMA at 262 kHz from TMA=F0 overflows
/// every 256 base clocks; the handler at 0x50 is RETI.
const HALT_IRQ: &[u8] = &[
    0x3E, 0xF0, //       LD A,F0
    0xE0, 0x06, //       LDH (TMA),A
    0x3E, 0x05, //       LD A,05
    0xE0, 0x07, //       LDH (TAC),A
    0x3E, 0x04, //       LD A,04
    0xE0, 0xFF, //       LDH (IE),A      timer only
    0xAF, //             XOR A
    0xE0, 0x0F, //       LDH (IF),A
    0xFB, //             EI
    // idle:
    0x76, //             HALT
    0x00, //             NOP
    0x18, 0xFC, //       JR idle
];

/// Combined worst case: the event storm plus the busiest PPU frame. After
/// the speed switch, with the LCD off: tiles 0 and 1 solid, 40 objects in
/// four groups of 10 (8x16, Y=16/52/88/124, X=8+6i so they overlap) giving
/// 10 objects on 64 of the 144 lines (40 objects cannot cover more), then
/// LCD on with BG, window (WY=0, WX=87), objects, every STAT source
/// (modes 0/1/2 and LYC=72) and IE = STAT and timer (below). The STAT
/// handler (`JP` at 0x48) bumps a WRAM counter. Then the event-storm serial
/// loop runs, which
/// also keeps a CGB HBlank HDMA going: 128 blocks from ROM 0000 to VRAM
/// 8800 (tiles 128-255, which nothing displays), restarted as soon as one
/// ends, so every visible line copies a 16-byte block. All four sound
/// channels play before this, and the STAT handler (one IRQ per line,
/// about 146 a frame) toggles NR50's right volume, like a game playing PCM
/// through the master volume: every APU write syncs the APU, so its fixed
/// cost per sync counts ([`worst_case`], [`STAT_HANDLER_NR50`]). The main
/// loop also polls STAT and writes SCX (+1 each time) once per visible
/// line, the first time a poll sees mode 3 and then an LY it has not
/// written on (comparing the mode with the last poll's would miss lines:
/// the handlers take most of a heavy line's modes 0 and 2). The write is
/// 18 M-cycles after the poll's STAT read and the poll loop is 24, so
/// with no handler in between it lands 18-42 M-cycles (36-84 dots) into
/// mode 3; a handler in between moves it later. Measured over 20 frames
/// (2,880 writes): 86% land in mode 3 after the fetcher has started (24
/// to 196 dots in), so the PPU's held-fetch slow path (`Ppu::hold_fetch`,
/// `render_held`) runs about 124 times a frame, like a game's per-line
/// parallax scroll (human, session 10); 0.7% land in the first 24 dots of
/// mode 3, where the fine-scroll discard is; 14% land in the next line's
/// mode 2 (the line changed between the STAT read and the write), where
/// the write is a plain one.
/// The timer interrupt is serviced too (IE = STAT and timer): TIMA counts
/// at 262 kHz nominal, 524 kHz in double speed, from TMA=E0, so it
/// overflows every 256 base clocks: 16,384 Hz, 274.3 a frame, about two
/// per line. The handler at 0x50 ([`timer_handler`]) is a PCM sample
/// player: it advances a 16-bit sample counter in WRAM (C002, low byte =
/// position in a 256-byte table at 4000, [`PCM_TABLE`]), loads the next
/// sample and writes it to NR50, the master volume: the usual way to play
/// 4-bit PCM on the Game Boy with no DAC of its own (every channel held
/// at a constant level, the samples scaling their sum), as
/// `synth:apu-writes` does. 16,384 Hz is twice that bench's 8 kHz, the
/// high end for such a driver, and a CGB game in double speed can afford
/// it (the handler is 42 M-cycles with the dispatch, 33% of the CPU);
/// a TMA of FF (an overflow every 8 clocks, as in `synth:event-storm`)
/// would keep the CPU in the handler. The STAT handler moved behind a `JP`
/// at 0x48 to make room for it ([`STAT_JP`]).
const WORST_CASE: &[u8] = &[
    0xF3, //                   DI
    0x3E, 0x01, //             LD A,01
    0xE0, 0x4D, //             LDH (KEY1),A
    0x10, 0x00, //             STOP
    0x3E, 0xE0, //             LD A,E0
    0xE0, 0x06, //             LDH (TMA),A     an overflow every 256 base clocks
    0x3E, 0x05, //             LD A,05
    0xE0, 0x07, //             LDH (TAC),A     on, 262 kHz (524 kHz in double speed)
    0xAF, //                   XOR A
    0xE0, 0x40, //             LDH (LCDC),A    LCD off: free VRAM/OAM access
    0x21, 0x00, 0x80, //       LD HL,8000
    0x3E, 0xFF, //             LD A,FF
    0x06, 0x20, //             LD B,32         tiles 0 and 1 solid
    0x22, //                   LD (HL+),A
    0x05, //                   DEC B
    0x20, 0xFC, //             JR NZ,-4
    0x21, 0x00, 0xFE, //       LD HL,FE00
    0x16, 0x10, //             LD D,16         first group's Y
    0x1E, 0x04, //             LD E,4          groups
    // group:
    0x06, 0x0A, //             LD B,10
    0x0E, 0x08, //             LD C,8          first X
    // obj:
    0x7A, //                   LD A,D
    0x22, //                   LD (HL+),A      Y
    0x79, //                   LD A,C
    0x22, //                   LD (HL+),A      X
    0xAF, //                   XOR A
    0x22, //                   LD (HL+),A      tile 0
    0x22, //                   LD (HL+),A      attributes 0
    0x79, //                   LD A,C
    0xC6, 0x06, //             ADD A,6
    0x4F, //                   LD C,A
    0x05, //                   DEC B
    0x20, 0xF2, //             JR NZ,obj
    0x7A, //                   LD A,D
    0xC6, 0x24, //             ADD A,36
    0x57, //                   LD D,A
    0x1D, //                   DEC E
    0x20, 0xE7, //             JR NZ,group
    0xAF, //                   XOR A
    0xE0, 0x4A, //             LDH (WY),A
    0x3E, 0x57, //             LD A,87
    0xE0, 0x4B, //             LDH (WX),A
    0x3E, 0x48, //             LD A,72
    0xE0, 0x45, //             LDH (LYC),A
    0x3E, 0x78, //             LD A,78         STAT: LYC, mode 2, 1, 0
    0xE0, 0x41, //             LDH (STAT),A
    0x3E, 0x06, //             LD A,06
    0xE0, 0xFF, //             LDH (IE),A      STAT and timer
    0xAF, //                   XOR A
    0xE0, 0x0F, //             LDH (IF),A
    0x3E, 0xB7, //             LD A,B7         LCD, window, 8x16 obj, 8000 tiles, obj, BG
    0xE0, 0x40, //             LDH (LCDC),A
    0xFB, //                   EI
    // wait:
    0xF0, 0x41, //             LDH A,(STAT)
    0xE6, 0x03, //             AND 3
    0xFE, 0x03, //             CP 3
    0x20, 0x0A, //             JR NZ,serial
    0xF0, 0x44, //             LDH A,(LY)
    0xBA, //                   CP D            mode 3 of the line last written?
    0x28, 0x05, //             JR Z,serial
    0x57, //                   LD D,A
    0x1C, //                   INC E           a new line's mode 3: new SCX
    0x7B, //                   LD A,E
    0xE0, 0x43, //             LDH (SCX),A     mid-line: the held-fetch path
    // serial:
    0xF0, 0x02, //             LDH A,(SC)
    0x87, //                   ADD A,A         carry = transfer running
    0x38, 0x04, //             JR C,hdma
    0x3E, 0x83, //             LD A,83
    0xE0, 0x02, //             LDH (SC),A
    // hdma:
    0xF0, 0x55, //             LDH A,(HDMA5)
    0x87, //                   ADD A,A         no carry = HDMA active
    0x30, 0xE0, //             JR NC,wait
    0xAF, //                   XOR A           restart: 0000 -> 8800 (tiles 128-255, unused)
    0xE0, 0x51, //             LDH (HDMA1),A
    0xE0, 0x52, //             LDH (HDMA2),A
    0xE0, 0x54, //             LDH (HDMA4),A
    0x3E, 0x88, //             LD A,88
    0xE0, 0x53, //             LDH (HDMA3),A
    0x3E, 0xFF, //             LD A,FF         HBlank, 128 blocks
    0xE0, 0x55, //             LDH (HDMA5),A
    0x18, 0xCF, //             JR wait
];

/// `synth:worst-case` audio: APU off and on (every channel off, including
/// the post-boot CH1), NR50 = 77 (full volume, both sides),
/// NR51 = FF (every channel on both sides). Then the channels in
/// `APU_CHANNELS` the build mask selects, each at its worst period for
/// `Apu::sync` (ADR 0021 section 9): squares and wave at f = 2047, noise at
/// r = 0, s = 0, 7-bit. Envelopes run (volume 15, down, pace 7: 1.64 s of
/// steps); CH1's sweep runs (pace 1, negate, shift 0: a calculation every
/// 1/128 s that leaves f at 2047). Lengths are off.
const APU_ON: &[u8] = &[
    0xAF, //                   XOR A
    0xE0, 0x26, //             LDH (NR52),A    APU off: all channels off
    0x3E, 0x80, //             LD A,80
    0xE0, 0x26, //             LDH (NR52),A    APU on
    0x3E, 0x77, //             LD A,77
    0xE0, 0x24, //             LDH (NR50),A    full volume both sides
    0x3E, 0xFF, //             LD A,FF
    0xE0, 0x25, //             LDH (NR51),A    every channel both sides
];

/// CH1-CH4 setup, in mask bit order.
const APU_CHANNELS: [&[u8]; 4] = [
    &[
        0x3E, 0x18, //         LD A,18
        0xE0, 0x10, //         LDH (NR10),A    sweep pace 1, negate, shift 0
        0x3E, 0x80, //         LD A,80
        0xE0, 0x11, //         LDH (NR11),A    duty 50%
        0x3E, 0xF7, //         LD A,F7
        0xE0, 0x12, //         LDH (NR12),A    volume 15, down, pace 7
        0x3E, 0xFF, //         LD A,FF
        0xE0, 0x13, //         LDH (NR13),A
        0x3E, 0x87, //         LD A,87
        0xE0, 0x14, //         LDH (NR14),A    trigger, f = 2047
    ],
    &[
        0x3E, 0x80, //         LD A,80
        0xE0, 0x16, //         LDH (NR21),A    duty 50%
        0x3E, 0xF7, //         LD A,F7
        0xE0, 0x17, //         LDH (NR22),A    volume 15, down, pace 7
        0x3E, 0xFF, //         LD A,FF
        0xE0, 0x18, //         LDH (NR23),A
        0x3E, 0x87, //         LD A,87
        0xE0, 0x19, //         LDH (NR24),A    trigger, f = 2047
    ],
    &[
        0xAF, //               XOR A
        0xE0, 0x1A, //         LDH (NR30),A    DAC off: free wave RAM access
        0x21, 0x30, 0xFF, //   LD HL,FF30
        // ram:
        0x7D, //               LD A,L
        0x22, //               LD (HL+),A      30 31 .. 3F: 32 varied levels
        0x7D, //               LD A,L
        0xFE, 0x40, //         CP 40
        0x20, 0xF9, //         JR NZ,ram
        0x3E, 0x80, //         LD A,80
        0xE0, 0x1A, //         LDH (NR30),A    DAC on
        0x3E, 0x20, //         LD A,20
        0xE0, 0x1C, //         LDH (NR32),A    100% volume
        0x3E, 0xFF, //         LD A,FF
        0xE0, 0x1D, //         LDH (NR33),A
        0x3E, 0x87, //         LD A,87
        0xE0, 0x1E, //         LDH (NR34),A    trigger, f = 2047
    ],
    &[
        0x3E, 0xF7, //         LD A,F7
        0xE0, 0x21, //         LDH (NR42),A    volume 15, down, pace 7
        0x3E, 0x08, //         LD A,08
        0xE0, 0x22, //         LDH (NR43),A    s = 0, 7-bit, r = 0
        0x3E, 0x80, //         LD A,80
        0xE0, 0x23, //         LDH (NR44),A    trigger
    ],
];

/// The `synth:worst-case` code with the channels in `mask` (bit n = CHn+1)
/// playing: [`APU_ON`] and their [`APU_CHANNELS`] setup (none at all for
/// mask 0), then [`WORST_CASE`] (its jumps are relative).
/// `synth:worst-case` is mask F; `synth:worst-case@<hex digit>` builds the
/// others, to measure the APU's cost per channel (docs/perf.md). Mask 0
/// also keeps the plain STAT handler (no APU writes: no APU at all).
fn worst_case(mask: u8) -> Vec<u8> {
    let mut code = Vec::new();
    if mask != 0 {
        code.extend_from_slice(APU_ON);
    }
    for (ch, setup) in APU_CHANNELS.iter().enumerate() {
        if mask >> ch & 1 != 0 {
            code.extend_from_slice(setup);
        }
    }
    code.extend_from_slice(WORST_CASE);
    code
}

/// CGB palette raster effect ("hi-color") in double speed: after the speed
/// switch, with the LCD off, tile 0 gets a pattern using all four colors
/// and the BG map attributes (VRAM bank 1) select palette (column & 7), so
/// every BG palette is on screen. Then BCPS=80 (auto-increment from 0) and
/// LCD on with BG only (LCDC=91). Per line: wait for mode 0, write 32 BCPD
/// bytes (LD (C),A; INC A at 3 M-cycles each, so they run on through mode
/// 2 into mode 3 of the next line), wait for mode 3, then write BCPD, SCX,
/// BCPD mid-line. E carries the value from line to line, so the colors
/// keep changing.
const CGB_RASTER_SETUP: &[u8] = &[
    0xF3, //             DI
    0x3E, 0x01, //       LD A,01
    0xE0, 0x4D, //       LDH (KEY1),A
    0x10, 0x00, //       STOP
    0xAF, //             XOR A
    0xE0, 0x40, //       LDH (LCDC),A    LCD off: free VRAM access
    0x21, 0x00, 0x80, // LD HL,8000
    0x06, 0x10, //       LD B,16
    // tile:
    0x3E, 0x55, //       LD A,55
    0x22, //             LD (HL+),A
    0x3E, 0x33, //       LD A,33
    0x22, //             LD (HL+),A      colors 0-3 across each row
    0x05, //             DEC B
    0x20, 0xF7, //       JR NZ,tile
    0x3E, 0x01, //       LD A,01
    0xE0, 0x4F, //       LDH (VBK),A     attributes
    0x21, 0x00, 0x98, // LD HL,9800
    // attr:
    0x7D, //             LD A,L
    0xE6, 0x07, //       AND 7           palette = column & 7
    0x22, //             LD (HL+),A
    0x7C, //             LD A,H
    0xFE, 0x9C, //       CP 9C
    0x20, 0xF7, //       JR NZ,attr
    0xAF, //             XOR A
    0xE0, 0x4F, //       LDH (VBK),A
    0x3E, 0x80, //       LD A,80
    0xE0, 0x68, //       LDH (BCPS),A    auto-increment from 0
    0x0E, 0x69, //       LD C,69         BCPD
    0x3E, 0x91, //       LD A,91         LCD on, 8000 tiles, BG
    0xE0, 0x40, //       LDH (LCDC),A
];

/// The `synth:cgb-raster` code: [`CGB_RASTER_SETUP`], then the per-line
/// loop (32 unrolled BCPD writes).
fn cgb_raster() -> Vec<u8> {
    let mut code = CGB_RASTER_SETUP.to_vec();
    code.extend_from_slice(&[
        // line:
        0xF0, 0x41, //   LDH A,(STAT)
        0xE6, 0x03, //   AND 3
        0x20, 0xFA, //   JR NZ,line      until mode 0
        0x7B, //         LD A,E
    ]);
    for _ in 0..32 {
        code.extend_from_slice(&[0xE2, 0x3C]); // LD (C),A; INC A
    }
    code.extend_from_slice(&[
        0x5F, //         LD E,A
        // mode3:
        0xF0, 0x41, //   LDH A,(STAT)
        0xE6, 0x03, //   AND 3
        0xFE, 0x03, //   CP 3
        0x20, 0xF8, //   JR NZ,mode3
        0x7B, //         LD A,E
        0xE2, //         LD (C),A        BCPD in mode 3
        0xE0, 0x43, //   LDH (SCX),A     SCX in mode 3
        0x3C, //         INC A
        0xE2, //         LD (C),A        BCPD in mode 3
        0x5F, //         LD E,A
        0x18, 0xA6, //   JR line
    ]);
    code
}

/// DMG render path (DMG compatibility mode, header 0143 = 00, so single
/// speed): BG, window and 10 objects per line, with per-line raster
/// writes, the DMG counterpart of `synth:cgb-raster`. With the LCD off:
/// tiles 0-3 have all four colors across each row (55/33), tile 4 is solid
/// color 3; the BG map (9800) is all tile 0 and the window map (9C00) all
/// tile 4. 40 8x16 objects (tile 2) in four groups of 10 as in
/// `synth:worst-case` (Y=16/52/88/124, X=8+6i, so they overlap and DMG's
/// X-coordinate priority decides): 10 objects on 64 of the 144 lines.
/// Object i has attributes (10-i)<<4: OBP0/OBP1, X and Y flip in every
/// combination, and BG-over-OBJ priority on objects 0-2. BGP=OBP0=E4,
/// OBP1=1B, window at WY=0, WX=87 (the right half), LCDC=F7 (LCD, window
/// map 9C00, window, 8000 tiles, BG map 9800, 8x16 objects, objects, BG).
/// Per line: wait for mode 0, rotate the shades by one (RLCA twice, still
/// a permutation) into BGP, OBP0 and its complement into OBP1, SCY + 1;
/// wait for mode 3, then write SCX and the next rotation to BGP mid-line.
const DMG_RASTER: &[u8] = &[
    0xF3, //             DI
    0xAF, //             XOR A
    0xE0, 0x40, //       LDH (LCDC),A    LCD off: free VRAM/OAM access
    0x21, 0x00, 0x80, // LD HL,8000
    0x06, 0x20, //       LD B,32         tiles 0-3
    // tile:
    0x3E, 0x55, //       LD A,55
    0x22, //             LD (HL+),A
    0x3E, 0x33, //       LD A,33
    0x22, //             LD (HL+),A      colors 0-3 across each row
    0x05, //             DEC B
    0x20, 0xF7, //       JR NZ,tile
    0x3E, 0xFF, //       LD A,FF
    0x06, 0x10, //       LD B,16         tile 4 solid color 3
    // solid:
    0x22, //             LD (HL+),A
    0x05, //             DEC B
    0x20, 0xFC, //       JR NZ,solid
    0x21, 0x00, 0x98, // LD HL,9800
    // map:
    0x7C, //             LD A,H
    0xD6, 0x9C, //       SUB 9C
    0x9F, //             SBC A,A         FF below 9C00
    0x2F, //             CPL
    0xE6, 0x04, //       AND 4           tile 0 (BG map), tile 4 (window map)
    0x22, //             LD (HL+),A
    0x7C, //             LD A,H
    0xFE, 0xA0, //       CP A0
    0x20, 0xF3, //       JR NZ,map
    0x21, 0x00, 0xFE, // LD HL,FE00
    0x16, 0x10, //       LD D,16         first group's Y
    0x1E, 0x04, //       LD E,4          groups
    // group:
    0x06, 0x0A, //       LD B,10
    0x0E, 0x08, //       LD C,8          first X
    // obj:
    0x7A, //             LD A,D
    0x22, //             LD (HL+),A      Y
    0x79, //             LD A,C
    0x22, //             LD (HL+),A      X
    0x3E, 0x02, //       LD A,2
    0x22, //             LD (HL+),A      tile 2 (and 3)
    0x78, //             LD A,B
    0xE6, 0x0F, //       AND 0F
    0xCB, 0x37, //       SWAP A
    0x22, //             LD (HL+),A      attributes B<<4
    0x79, //             LD A,C
    0xC6, 0x06, //       ADD A,6
    0x4F, //             LD C,A
    0x05, //             DEC B
    0x20, 0xEC, //       JR NZ,obj
    0x7A, //             LD A,D
    0xC6, 0x24, //       ADD A,36
    0x57, //             LD D,A
    0x1D, //             DEC E
    0x20, 0xE1, //       JR NZ,group
    0x3E, 0xE4, //       LD A,E4
    0xE0, 0x47, //       LDH (BGP),A
    0xE0, 0x48, //       LDH (OBP0),A
    0x2F, //             CPL
    0xE0, 0x49, //       LDH (OBP1),A
    0xAF, //             XOR A
    0xE0, 0x4A, //       LDH (WY),A
    0x3E, 0x57, //       LD A,87
    0xE0, 0x4B, //       LDH (WX),A
    0x3E, 0xF7, //       LD A,F7
    0xE0, 0x40, //       LDH (LCDC),A
    0x1E, 0xE4, //       LD E,E4         the shades, line to line
    // line:
    0xF0, 0x41, //       LDH A,(STAT)
    0xE6, 0x03, //       AND 3
    0x20, 0xFA, //       JR NZ,line      until mode 0
    0x7B, //             LD A,E
    0x07, //             RLCA
    0x07, //             RLCA            rotate the shades
    0x5F, //             LD E,A
    0xE0, 0x47, //       LDH (BGP),A
    0xE0, 0x48, //       LDH (OBP0),A
    0x2F, //             CPL
    0xE0, 0x49, //       LDH (OBP1),A
    0x14, //             INC D
    0x7A, //             LD A,D
    0xE0, 0x42, //       LDH (SCY),A
    // mode3:
    0xF0, 0x41, //       LDH A,(STAT)
    0xE6, 0x03, //       AND 3
    0xFE, 0x03, //       CP 3
    0x20, 0xF8, //       JR NZ,mode3
    0x7A, //             LD A,D
    0x2F, //             CPL
    0xE0, 0x43, //       LDH (SCX),A     SCX in mode 3
    0x7B, //             LD A,E
    0x07, //             RLCA
    0x07, //             RLCA
    0x5F, //             LD E,A
    0xE0, 0x47, //       LDH (BGP),A     BGP in mode 3
    0x18, 0xD7, //       JR line
];

/// HBlank sprite multiplexing on an ordinary CGB frame (single speed): the
/// CPU rewrites OAM on every visible line, so the PPU's summary of the
/// lines objects cover (`ObjectLines` in gbc-core's ppu) is stale at every
/// line's scan (M10 review). With the LCD off: tiles 0-3 have all four
/// colors across each row (55/33), the BG map (9800) is all tile 0, and 40
/// 8x16 objects (tile 2, OBJ palette 10-i & 7) sit in four groups of 10 as
/// in `synth:worst-case` (Y=16/52/88/124, X=8+6i): 10 objects on 64 of the
/// 144 lines, none on the other 80. OBJ palette RAM gets 64 non-zero bytes.
/// LCDC=97 (LCD, 8000 tiles, 8x16 objects, objects, BG). Per line: wait
/// for mode 0, then write E (+1 per line) to the X of objects 0, 10, 20
/// and 30 (the first of each group), 14 M-cycles of writes that end at
/// most 104 dots into an HBlank of 121 dots or more (mode 3 is 255 dots
/// with the 10 objects), so OAM is open for all four; then wait for mode 3
/// (which also skips VBlank: 144 rounds of writes a frame).
const OAM_LINES: &[u8] = &[
    0xF3, //             DI
    0xAF, //             XOR A
    0xE0, 0x40, //       LDH (LCDC),A    LCD off: free VRAM/OAM access
    0x21, 0x00, 0x80, // LD HL,8000
    0x06, 0x20, //       LD B,32         tiles 0-3
    // tile:
    0x3E, 0x55, //       LD A,55
    0x22, //             LD (HL+),A
    0x3E, 0x33, //       LD A,33
    0x22, //             LD (HL+),A      colors 0-3 across each row
    0x05, //             DEC B
    0x20, 0xF7, //       JR NZ,tile
    0x21, 0x00, 0x98, // LD HL,9800
    // map:
    0xAF, //             XOR A
    0x22, //             LD (HL+),A      BG map: tile 0
    0x7C, //             LD A,H
    0xFE, 0x9C, //       CP 9C
    0x20, 0xF9, //       JR NZ,map
    0x21, 0x00, 0xFE, // LD HL,FE00
    0x16, 0x10, //       LD D,16         first group's Y
    0x1E, 0x04, //       LD E,4          groups
    // group:
    0x06, 0x0A, //       LD B,10
    0x0E, 0x08, //       LD C,8          first X
    // obj:
    0x7A, //             LD A,D
    0x22, //             LD (HL+),A      Y
    0x79, //             LD A,C
    0x22, //             LD (HL+),A      X
    0x3E, 0x02, //       LD A,2
    0x22, //             LD (HL+),A      tile 2 (and 3)
    0x78, //             LD A,B
    0xE6, 0x07, //       AND 07
    0x22, //             LD (HL+),A      attributes: OBJ palette B & 7
    0x79, //             LD A,C
    0xC6, 0x06, //       ADD A,6
    0x4F, //             LD C,A
    0x05, //             DEC B
    0x20, 0xEE, //       JR NZ,obj
    0x7A, //             LD A,D
    0xC6, 0x24, //       ADD A,36
    0x57, //             LD D,A
    0x1D, //             DEC E
    0x20, 0xE3, //       JR NZ,group
    0x3E, 0x80, //       LD A,80
    0xE0, 0x6A, //       LDH (OCPS),A    auto-increment from 0
    0x06, 0x40, //       LD B,64
    // pal:
    0x78, //             LD A,B
    0xE0, 0x6B, //       LDH (OCPD),A
    0x05, //             DEC B
    0x20, 0xFA, //       JR NZ,pal
    0x3E, 0x97, //       LD A,97
    0xE0, 0x40, //       LDH (LCDC),A
    // line:             (E = 0 after the groups)
    0x1C, //             INC E
    0x21, 0x01, 0xFE, // LD HL,FE01      object 0's X
    // wait:
    0xF0, 0x41, //       LDH A,(STAT)
    0xE6, 0x03, //       AND 3
    0x20, 0xFA, //       JR NZ,wait      until mode 0
    0x73, //             LD (HL),E
    0x2E, 0x29, //       LD L,29         object 10's X
    0x73, //             LD (HL),E
    0x2E, 0x51, //       LD L,51         object 20's X
    0x73, //             LD (HL),E
    0x2E, 0x79, //       LD L,79         object 30's X
    0x73, //             LD (HL),E
    // mode3:
    0xF0, 0x41, //       LDH A,(STAT)
    0xE6, 0x03, //       AND 3
    0xFE, 0x03, //       CP 3
    0x20, 0xF8, //       JR NZ,mode3
    0x18, 0xE2, //       JR line
];

/// Where the worst-case STAT handler lives: after the vectors, so the
/// timer vector at 0x50 is free for [`TIMER_HANDLER`].
const STAT_HANDLER_AT: usize = 0x68;

/// The STAT vector at 0x48: JP [`STAT_HANDLER_AT`].
const STAT_JP: &[u8] = &[0xC3, STAT_HANDLER_AT as u8, 0x00];

/// The worst-case STAT handler without APU writes (9 bytes): PUSH AF;
/// PUSH HL; LD HL,C000; INC (HL); POP HL; POP AF; RETI.
const STAT_HANDLER: &[u8] = &[0xF5, 0xE5, 0x21, 0x00, 0xC0, 0x34, 0xE1, 0xF1, 0xD9];

/// The worst-case timer handler at 0x50 (20 bytes, up to 0x63; the
/// joypad vector at 0x60 is unused), for HRAM address `reg`: bumps the
/// 16-bit counter at C002 (little-endian), then plays sample (low byte)
/// of [`PCM_TABLE`] to `reg`. PUSH AF; PUSH HL; LD HL,C002; INC (HL);
/// LD A,(HL); JR NZ,play; INC L; INC (HL); play: LD L,A; LD H,40;
/// LD A,(HL); LDH (reg),A; POP HL; POP AF; RETI. `synth:worst-case` plays
/// to NR50; `@0` (no APU at all) to HRAM FF80.
const fn timer_handler(reg: u8) -> [u8; 20] {
    [
        0xF5, 0xE5, 0x21, 0x02, 0xC0, 0x34, 0x7E, 0x20, 0x02, 0x2C, 0x34, 0x6F, 0x26, 0x40, 0x7E,
        0xE0, reg, 0xE1, 0xF1, 0xD9,
    ]
}
const TIMER_HANDLER_NR50: [u8; 20] = timer_handler(0x24);
const TIMER_HANDLER_HRAM: [u8; 20] = timer_handler(0x80);

/// Where [`PCM_TABLE`] lives (bank 1 of the 32 KiB ROM).
const PCM_TABLE_AT: usize = 0x4000;

/// The worst-case PCM samples as NR50 values: a 4-bit triangle wave
/// (period 16 samples, 1,024 Hz at 16 kHz) at levels 0-7, the same on
/// both sides (Pan Docs FF24: bits 4-6 left, 0-2 right, VIN off).
const PCM_TABLE: [u8; 256] = {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        let p = (i & 15) as u8;
        let s = if p < 8 { p } else { 15 - p };
        t[i] = s << 4 | s;
        i += 1;
    }
    t
};

/// The worst-case STAT handler with the channels on (15 bytes):
/// [`STAT_HANDLER`] plus LDH A,(NR50); XOR 07; LDH (NR50),A before the
/// pops.
const STAT_HANDLER_NR50: &[u8] = &[
    0xF5, 0xE5, 0x21, 0x00, 0xC0, 0x34, 0xF0, 0x24, 0xEE, 0x07, 0xE0, 0x24, 0xE1, 0xF1, 0xD9,
];

/// `synth:apu-writes`: 8 kHz PCM through NR50 (M6 review: the per-sync
/// fixed cost). All four channels as in `synth:worst-case` ([`APU_ON`],
/// [`APU_CHANNELS`]), single speed, then TIMA at 262 kHz from TMA=E0
/// overflows every 512 base clocks (8,192 Hz, 137.2 a frame) and the
/// timer handler ([`PCM_HANDLER`]) toggles NR50's right volume; the CPU
/// HALTs in between. `synth:apu-writes@hram` toggles HRAM FF80 instead
/// (the same program without APU writes): Ir per APU write is the
/// difference over the writes (docs/perf.md, APU cost).
const PCM_IDLE: &[u8] = &[
    0x3E, 0xE0, //       LD A,E0
    0xE0, 0x06, //       LDH (TMA),A
    0x3E, 0x05, //       LD A,05
    0xE0, 0x07, //       LDH (TAC),A     on, 262 kHz
    0x3E, 0x04, //       LD A,04
    0xE0, 0xFF, //       LDH (IE),A      timer only
    0xAF, //             XOR A
    0xE0, 0x0F, //       LDH (IF),A
    0xFB, //             EI
    // idle:
    0x76, //             HALT
    0x00, //             NOP
    0x18, 0xFC, //       JR idle
];

/// The `synth:apu-writes` timer handler at 0x50, for HRAM address `reg`:
/// LDH A,(reg); XOR 07; LDH (reg),A; RETI.
const fn pcm_handler(reg: u8) -> [u8; 7] {
    [0xF0, reg, 0xEE, 0x07, 0xE0, reg, 0xD9]
}
const PCM_HANDLER: [u8; 7] = pcm_handler(0x24);
const PCM_HANDLER_HRAM: [u8; 7] = pcm_handler(0x80);

/// The `synth:apu-writes` code: every channel on, then [`PCM_IDLE`].
fn apu_writes() -> Vec<u8> {
    let mut code = APU_ON.to_vec();
    APU_CHANNELS.iter().for_each(|c| code.extend_from_slice(c));
    code.extend_from_slice(PCM_IDLE);
    code
}

// --- Storms: adversarial programs outside the benched bound ---
//
// `synth:storm-*` (docs/perf.md, "Outside the bound"). Each one runs one
// kind of expensive access in the tightest loop the CPU allows, for the
// whole frame, over the same background: the picture of `synth:worst-case`
// and all four sound channels at their worst periods, no interrupts. They
// are measurements of what an adversarial (or careless) program costs,
// not workloads the 50% bar covers: none is in `scripts/check-bench.sh`.
// `storm-control` is the background with the loop writing HRAM, so a
// storm's own cost is its Ir minus the control's.

/// The storms' picture, as `synth:worst-case` draws it, from any LCD
/// state: LCD off, tiles 0 and 1 solid, 40 8x16 objects in four groups of
/// 10 (Y=16/52/88/124, X=8+6i: 10 objects on 64 of the 144 lines), window
/// at WY=0, WX=87, then LCDC=B7 (LCD, window, 8x16 objects, 8000 tiles,
/// objects, BG). The maps are all tile 0 (VRAM as powered on).
const STORM_SCENE: &[u8] = &[
    0xAF, //             XOR A
    0xE0, 0x40, //       LDH (LCDC),A    LCD off: free VRAM/OAM access
    0x21, 0x00, 0x80, // LD HL,8000
    0x3E, 0xFF, //       LD A,FF
    0x06, 0x20, //       LD B,32         tiles 0 and 1 solid
    0x22, //             LD (HL+),A
    0x05, //             DEC B
    0x20, 0xFC, //       JR NZ,-4
    0x21, 0x00, 0xFE, // LD HL,FE00
    0x16, 0x10, //       LD D,16         first group's Y
    0x1E, 0x04, //       LD E,4          groups
    // group:
    0x06, 0x0A, //       LD B,10
    0x0E, 0x08, //       LD C,8          first X
    // obj:
    0x7A, //             LD A,D
    0x22, //             LD (HL+),A      Y
    0x79, //             LD A,C
    0x22, //             LD (HL+),A      X
    0xAF, //             XOR A
    0x22, //             LD (HL+),A      tile 0
    0x22, //             LD (HL+),A      attributes 0
    0x79, //             LD A,C
    0xC6, 0x06, //       ADD A,6
    0x4F, //             LD C,A
    0x05, //             DEC B
    0x20, 0xF2, //       JR NZ,obj
    0x7A, //             LD A,D
    0xC6, 0x24, //       ADD A,36
    0x57, //             LD D,A
    0x1D, //             DEC E
    0x20, 0xE7, //       JR NZ,group
    0xAF, //             XOR A
    0xE0, 0x4A, //       LDH (WY),A
    0x3E, 0x57, //       LD A,87
    0xE0, 0x4B, //       LDH (WX),A
    0x3E, 0xB7, //       LD A,B7
    0xE0, 0x40, //       LDH (LCDC),A
];

/// The storms' background: DI, the switch to double speed (`double`), the
/// four channels of `synth:worst-case` ([`APU_ON`], [`APU_CHANNELS`]) and
/// [`STORM_SCENE`]. IE = 0.
fn storm_background(double: bool) -> Vec<u8> {
    let mut code = vec![0xF3]; //              DI
    if double {
        code.extend_from_slice(&[
            0x3E, 0x01, //                     LD A,01
            0xE0, 0x4D, //                     LDH (KEY1),A
            0x10, 0x00, //                     STOP
        ]);
    }
    code.extend_from_slice(APU_ON);
    APU_CHANNELS.iter().for_each(|c| code.extend_from_slice(c));
    code.extend_from_slice(STORM_SCENE);
    code
}

/// Pairs of 2-byte steps in a storm loop.
const STORM_PAIRS: usize = 32;

/// M-cycles of one pass of a [`storm_loop`] whose steps are 2 M-cycles
/// each (`LD (C),A`, `LD (HL),B`): the steps and the `JR`.
#[cfg(test)]
const STORM_LOOP_MCYCLES: u64 = 4 * STORM_PAIRS as u64 + 3;

/// A storm's loop: C = `c`, HL = FF00 + `hl`, A = `a`, B = `b`, then
/// `pair` [`STORM_PAIRS`] times and a `JR` back to the first. With
/// `pair` = `LD (C),A; LD (HL),B` (E2, 70) and `c` = `hl`, that is a write
/// to one register every 2 M-cycles (the densest the SM83 has), the value
/// alternating between `a` and `b`, and 3 M-cycles for the `JR` after
/// every 64: 64 writes per 131 M-cycles, 17,154 a frame in double speed
/// (35,112 M-cycles), 8,577 in single speed.
fn storm_loop(c: u8, hl: u8, a: u8, b: u8, pair: [u8; 2]) -> Vec<u8> {
    let mut code = vec![
        0x0E, c, //                            LD C,c
        0x21, hl, 0xFF, //                     LD HL,FF00+hl
        0x3E, a, //                            LD A,a
        0x06, b, //                            LD B,b
    ];
    for _ in 0..STORM_PAIRS {
        code.extend_from_slice(&pair);
    }
    // JR to the first pair: back over the pairs and the JR itself.
    code.extend_from_slice(&[0x18, 0u8.wrapping_sub(2 * STORM_PAIRS as u8 + 2)]);
    code
}

/// `LD (C),A; LD (HL),B`.
const WRITE_PAIR: [u8; 2] = [0xE2, 0x70];

/// `storm-apu@<reg>`'s two values for FF00 + `reg`: ones that keep the
/// channel playing (a DAC off, or a volume of 0, would end the work the
/// storm is there to cause).
fn storm_apu_values(reg: u8) -> (u8, u8) {
    match reg {
        // NR11, NR21: duty 50% / 25%.
        0x11 | 0x16 => (0x80, 0x40),
        // NR12, NR22, NR42: volume 15 / 14, down, pace 7.
        0x12 | 0x17 | 0x21 => (0xF7, 0xE7),
        // NR13, NR23, NR33: period low byte.
        0x13 | 0x18 | 0x1D => (0xFF, 0xFE),
        // NR32: 100% / 50% (4-bit PCM through CH3's volume does this).
        0x1C => (0x20, 0x40),
        // NR43: r = 0 / 1 (s = 0, 7-bit).
        0x22 => (0x08, 0x09),
        // NR50: both sides 7 / 6.
        0x24 => (0x77, 0x66),
        // NR51: every channel on both sides / CH1 off on the right.
        0x25 => (0xFF, 0xFE),
        // Unused FF27-FF2F, wave RAM, and the rest.
        _ => (0x00, 0xFF),
    }
}

/// Where [`storm_gdma_table`] lives: ROM bank 1, 4000-7FFF.
const STORM_GDMA_TABLE_AT: usize = 0x4000;

/// `storm-gdma`'s ROM bank 1: 16-byte block `k` of the address space
/// (address 16k, k = 400-7FF) holds k (little-endian) and fourteen 5A, so
/// a block found in VRAM tells how many blocks the transfers had copied.
fn storm_gdma_table() -> Vec<u8> {
    let mut t = vec![0x5A; 0x4000];
    for (j, block) in t.as_chunks_mut::<16>().0.iter_mut().enumerate() {
        block[..2].copy_from_slice(&((STORM_GDMA_TABLE_AT / 16 + j) as u16).to_le_bytes());
    }
    t
}

/// The code of storm `name` (`storm-<kind>[@<variant>][:1x]`; without a
/// variant, the kind's default below, which [`NAMES`] and docs/perf.md
/// list), and whether it needs [`storm_gdma_table`]. All but `storm-spsw`
/// run in double speed unless `:1x` asks for single speed.
///
/// - `storm-control[@hram]`: the loop writes HRAM FF80 (00 / FF).
/// - `storm-apu[@<hex reg>]` (default `1c`, NR32): the loop writes that
///   APU register ([`storm_apu_values`]). Every write but a powered
///   NR50/NR51 one brings the four channels up to the write
///   (`Apu::sync_write`). `@off`: NR52 = 0 first (APU off, where FF10-FF25
///   ignore writes), then writes to NR32.
/// - `storm-spsw`: IE = IF = 04 with IME off, so every STOP is the
///   one-byte kind that switches speed without the 0x20000-clock halt
///   (`Cpu::stop`); the loop is `LD (HL),B` (KEY1 = 01) and `STOP`: a
///   switch every 3 M-cycles, plus the idle M-cycle to double speed, 20
///   clocks for the two directions: 32 switches per 332 clocks, 6,769 a
///   frame.
/// - `storm-div[@div]`: TMA = FF, TAC = 05 (on, 262 kHz), a serial
///   transfer started on the fast internal clock (SC = 83), then the loop
///   writes DIV. `@tac`: no serial, the loop writes TAC = 05 / 04 (on /
///   off at the same rate), `@tac-rate` 05 / 07 (262 kHz / 16 kHz).
/// - `storm-gdma[@pinned]`: general-purpose VRAM DMA of one block, source
///   from 0000 on (it advances 16 a block through the whole address
///   space), the loop being `LD (HL),B` (HDMA3 = 88: the destination's
///   high byte, so the blocks land in 8800-88FF, tiles the picture does
///   not use) and `LD (C),A` (HDMA5 = 00): a block every 4 M-cycles plus
///   its 17 M-cycle stall (double speed), 32 per 675 M-cycles, 1,665 a
///   frame. `@free`: both writes are HDMA5 = 00 (19 M-cycles a block,
///   1,839 a frame) and the destination runs through all of VRAM bank 0,
///   tiles and maps included, so the picture becomes ROM bytes.
/// - `storm-ppu[@<reg>]` (default `lcdc`): the loop writes a PPU register,
///   so about a third of the writes land in mode 3 and move what the rest
///   of the line shows: `lcdc` (B7 / AB: bits 2, 3 and 4, object size, BG
///   map and tile data), `scx` (00 / 05), `scy` (00 / 04), `wx` (87 / 95),
///   `bgp` (FF47, E4 / 1B: no effect on the picture in CGB mode), `bcpd`
///   (BCPS = 80 first, auto-increment; 00 / FF to FF69), `lyc` (FF45, 00 /
///   48), `stat` (FF41, 00 / 78: every STAT source; IE = 0).
/// - `storm-io@<hex reg>` (no default): the loop writes 00 / FF to FF00 +
///   reg, whatever that does there: for sweeping every register for a
///   cost the kinds above miss (docs/perf.md).
fn storm(name: &str) -> Option<(Vec<u8>, bool)> {
    let (name, double) = match name.strip_suffix(":1x") {
        Some(n) if n.contains('@') => (n, false),
        Some(_) => return None,
        None => (name, true),
    };
    let (kind, variant) = match name.split_once('@') {
        Some((k, v)) => (k, Some(v)),
        None => (name, None),
    };
    let mut pre: &[u8] = &[];
    let mut table = false;
    let (c, hl, a, b, pair) = match (kind, variant) {
        ("storm-control", None | Some("hram")) => (0x80, 0x80, 0x00, 0xFF, WRITE_PAIR),
        // Any I/O register, FF00 + the variant (two hex digits): 00 / FF.
        ("storm-io", Some(v)) => {
            let reg = u8::from_str_radix(v, 16).ok().filter(|_| v.len() == 2)?;
            (reg, reg, 0x00, 0xFF, WRITE_PAIR)
        }
        ("storm-apu", Some("off")) => {
            pre = &[
                0xAF, //                       XOR A
                0xE0, 0x26, //                 LDH (NR52),A    APU off
            ];
            (0x1C, 0x1C, 0x20, 0x40, WRITE_PAIR)
        }
        ("storm-apu", v) => {
            let v = v.unwrap_or("1c");
            let reg = u8::from_str_radix(v, 16).ok().filter(|_| v.len() == 2)?;
            if !(0x10..=0x3F).contains(&reg) {
                return None;
            }
            let (a, b) = storm_apu_values(reg);
            (reg, reg, a, b, WRITE_PAIR)
        }
        ("storm-spsw", None) => {
            pre = &[
                0x3E, 0x04, //                 LD A,04
                0xE0, 0x0F, //                 LDH (IF),A      timer requested,
                0xE0, 0xFF, //                 LDH (IE),A      enabled, IME off
            ];
            // LD (HL),B (KEY1 = 01); STOP (one byte: IE & IF != 0).
            (0x4D, 0x4D, 0x04, 0x01, [0x70, 0x10])
        }
        ("storm-div", None | Some("div")) => {
            pre = &[
                0x3E, 0xFF, //                 LD A,FF
                0xE0, 0x06, //                 LDH (TMA),A
                0x3E, 0x05, //                 LD A,05
                0xE0, 0x07, //                 LDH (TAC),A     on, 262 kHz
                0x3E, 0x83, //                 LD A,83
                0xE0, 0x02, //                 LDH (SC),A      start, fast, internal
            ];
            (0x04, 0x04, 0x00, 0x00, WRITE_PAIR)
        }
        ("storm-div", Some(v @ ("tac" | "tac-rate"))) => {
            pre = &[
                0x3E, 0xFF, //                 LD A,FF
                0xE0, 0x06, //                 LDH (TMA),A
            ];
            let b = if v == "tac" { 0x04 } else { 0x07 };
            (0x07, 0x07, 0x05, b, WRITE_PAIR)
        }
        ("storm-gdma", v @ (None | Some("pinned" | "free"))) => {
            pre = &[
                0xAF, //                       XOR A
                0xE0, 0x51, //                 LDH (HDMA1),A   source 0000
                0xE0, 0x52, //                 LDH (HDMA2),A
                0xE0, 0x54, //                 LDH (HDMA4),A   destination 8800
                0x3E, 0x88, //                 LD A,88
                0xE0, 0x53, //                 LDH (HDMA3),A
            ];
            table = true;
            if v == Some("free") {
                (0x55, 0x55, 0x00, 0x00, WRITE_PAIR)
            } else {
                // LD (HL),B (HDMA3 = 88); LD (C),A (HDMA5 = 00).
                (0x55, 0x53, 0x00, 0x88, [0x70, 0xE2])
            }
        }
        ("storm-ppu", v) => {
            let (reg, a, b) = match v.unwrap_or("lcdc") {
                "lcdc" => (0x40, 0xB7, 0xAB),
                "scx" => (0x43, 0x00, 0x05),
                "scy" => (0x42, 0x00, 0x04),
                "wx" => (0x4B, 0x57, 0x5F),
                "bgp" => (0x47, 0xE4, 0x1B),
                "bcpd" => {
                    pre = &[
                        0x3E, 0x80, //         LD A,80
                        0xE0, 0x68, //         LDH (BCPS),A    auto-increment from 0
                    ];
                    (0x69, 0x00, 0xFF)
                }
                "lyc" => (0x45, 0x00, 0x48),
                "stat" => (0x41, 0x00, 0x78),
                _ => return None,
            };
            (reg, reg, a, b, WRITE_PAIR)
        }
        _ => return None,
    };
    // The speed-switch storm starts in single speed and has no `:1x`.
    let spsw = kind == "storm-spsw";
    if spsw && !double {
        return None;
    }
    let mut code = storm_background(double && !spsw);
    code.extend_from_slice(pre);
    code.extend_from_slice(&storm_loop(c, hl, a, b, pair));
    Some((code, table))
}

/// The ROM image for `name`, or `None` if there is no such ROM.
pub fn build(name: &str) -> Option<Vec<u8>> {
    if let Some((code, table)) = storm(name) {
        let table = if table {
            storm_gdma_table()
        } else {
            Vec::new()
        };
        return Some(image(name, 0xC0, &code, &[(STORM_GDMA_TABLE_AT, &table)]));
    }
    let raster = cgb_raster();
    // `worst-case@<hex digit>`: the channel mask (`worst_case`).
    let mask = match name.strip_prefix("worst-case@") {
        Some(m) if m.len() == 1 => Some(u8::from_str_radix(m, 16).ok()?),
        Some(_) => return None,
        None => (name == "worst-case").then_some(0xF),
    };
    let worst = mask.map(worst_case).unwrap_or_default();
    let (stat, timer): (&[u8], &[u8]) = if mask == Some(0) {
        (STAT_HANDLER, &TIMER_HANDLER_HRAM)
    } else {
        (STAT_HANDLER_NR50, &TIMER_HANDLER_NR50)
    };
    let pcm = apu_writes();
    // The code (at 0x150), and (address, bytes) pieces outside it: the
    // interrupt vectors and handlers below 0x100, data from 0x4000.
    let (code, low): (&[u8], Vec<(usize, &[u8])>) = match name {
        "event-storm" => (EVENT_STORM, vec![]),
        "dma-storm" => (DMA_STORM, vec![]),
        "halt-irq" => (HALT_IRQ, vec![(0x50, &[0xD9])]), // RETI
        _ if mask.is_some() => (
            &worst,
            vec![
                (0x48, STAT_JP),
                (0x50, timer),
                (STAT_HANDLER_AT, stat),
                (PCM_TABLE_AT, &PCM_TABLE),
            ],
        ),
        "apu-writes" => (&pcm, vec![(0x50, &PCM_HANDLER)]),
        "apu-writes@hram" => (&pcm, vec![(0x50, &PCM_HANDLER_HRAM)]),
        "cgb-raster" => (&raster, vec![]),
        "dmg-raster" => (DMG_RASTER, vec![]),
        "oam-lines" => (OAM_LINES, vec![]),
        _ => return None,
    };
    // Pan Docs, 0143 (CGB flag): bit 7 clear runs the cartridge in DMG
    // compatibility mode; C0 is CGB only.
    let cgb_flag = if name == "dmg-raster" { 0x00 } else { 0xC0 };
    Some(image(name, cgb_flag, code, &low))
}

fn image(title: &str, cgb_flag: u8, code: &[u8], low: &[(usize, &[u8])]) -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    for &(at, bytes) in low {
        let dst = &mut rom[at..at + bytes.len()];
        assert!(
            at + bytes.len() <= 0x100 || at >= 0x4000,
            "{title}: {at:#x} runs into the header or code"
        );
        assert!(dst.iter().all(|&b| b == 0), "{title}: {at:#x} overlaps");
        dst.copy_from_slice(bytes);
    }
    // Entry: NOP; JP 0150.
    rom[0x100..0x104].copy_from_slice(&[0x00, 0xC3, 0x50, 0x01]);
    rom[0x104..0x134].copy_from_slice(&LOGO);
    let t = title.to_ascii_uppercase();
    let t = &t.as_bytes()[..t.len().min(15)];
    rom[0x134..0x134 + t.len()].copy_from_slice(t);
    rom[0x143] = cgb_flag;
    // 0x147 = 00 (ROM only), 0x148 = 00 (32 KiB), 0x149 = 00 (no RAM).
    rom[0x150..0x150 + code.len()].copy_from_slice(code);
    // Pan Docs: The Cartridge Header, 014D (header checksum) and 014E-014F
    // (global checksum, big-endian, over every byte except itself).
    rom[0x14D] = rom[0x134..0x14D]
        .iter()
        .fold(0u8, |x, &b| x.wrapping_sub(b).wrapping_sub(1));
    let sum = rom
        .iter()
        .enumerate()
        .filter(|&(i, _)| i != 0x14E && i != 0x14F)
        .fold(0u16, |s, (_, &b)| s.wrapping_add(u16::from(b)));
    rom[0x14E..0x150].copy_from_slice(&sum.to_be_bytes());
    rom
}

#[cfg(test)]
mod tests {
    use super::*;
    use gbc_core::{
        CLOCK_HZ, Config, DmgPalettes, Emulator, Events, Frame, Memory, SCREEN_HEIGHT,
        SCREEN_WIDTH, WHITE, rgb555,
    };

    /// Runs `name` for `clocks` and returns `peek` of each address.
    fn run(name: &str, clocks: u64, addrs: &[u16]) -> Vec<u8> {
        run_frame(name, clocks, false, Config::default(), addrs).0
    }

    /// Runs `name` for `clocks`, then (if `whole`) on to the end of the
    /// next complete frame, and returns `peek` of each address and the
    /// frame buffer.
    fn run_frame(
        name: &str,
        clocks: u64,
        whole: bool,
        config: Config,
        addrs: &[u16],
    ) -> (Vec<u8>, Box<Frame>) {
        let rom = build(name).unwrap();
        let mut mem = Box::new(Memory::new());
        let mut emu = Emulator::new(&mut mem);
        emu.load(&rom, &mut [], config).unwrap();
        let mut frame: Box<Frame> = vec![WHITE; SCREEN_WIDTH * SCREEN_HEIGHT]
            .into_boxed_slice()
            .try_into()
            .unwrap();
        while emu.clock() < clocks {
            let _ = emu.run_until(clocks, &mut frame, &mut []);
        }
        if whole {
            while !emu
                .run_frame(&mut frame, &mut [])
                .events
                .contains(Events::FRAME_READY)
            {}
        }
        (addrs.iter().map(|&a| emu.peek(a)).collect(), frame)
    }

    #[test]
    fn headers_are_valid() {
        for name in NAMES {
            let rom = build(name).unwrap();
            let x = rom[0x134..0x14D]
                .iter()
                .fold(0u8, |x, &b| x.wrapping_sub(b).wrapping_sub(1));
            assert_eq!(rom[0x14D], x, "{name}");
            assert_eq!(&rom[0x104..0x134], &LOGO, "{name}");
        }
        assert!(build("nope").is_none());
    }

    #[test]
    fn event_storm_runs_in_double_speed_with_the_timer_on() {
        // KEY1 bit 7: double speed. TAC: on, 262 kHz. SC: a transfer on
        // the fast internal clock is (re)armed.
        let r = run(
            "event-storm",
            u64::from(CLOCK_HZ) / 10,
            &[0xFF4D, 0xFF07, 0xFF02],
        );
        assert_eq!(r[0] & 0x80, 0x80, "double speed");
        assert_eq!(r[1] & 0x07, 0x05, "TAC");
        assert_eq!(r[2] & 0x03, 0x03, "SC fast internal");
    }

    #[test]
    fn dma_storm_copies_to_oam_and_wram() {
        // The last DMA was started from C000.
        let r = run("dma-storm", u64::from(CLOCK_HZ) / 10, &[0xFF46]);
        assert_eq!(r[0], 0xC0);
    }

    #[test]
    fn halt_irq_idles_with_the_timer_irq_enabled() {
        let r = run(
            "halt-irq",
            u64::from(CLOCK_HZ) / 10,
            &[0xFFFF, 0xFF07, 0xFF06],
        );
        assert_eq!(r, [0x04, 0xFD, 0xF0]);
    }
    #[test]
    fn worst_case_has_ten_objects_on_a_line_stat_irqs_and_hdma_running() {
        // OAM X of object 9 (=8+6*9), STAT sources, the handler's counter.
        let r = run(
            "worst-case",
            u64::from(CLOCK_HZ) / 10,
            &[0xFE25, 0xFF41, 0xFFFF, 0xFF4D, 0xC000],
        );
        assert_eq!(r[0], 62);
        assert_eq!(r[1] & 0x78, 0x78);
        assert_eq!(r[2], 0x06);
        assert_eq!(r[3] & 0x80, 0x80, "double speed");
        assert_ne!(r[4], 0, "STAT handler ran");
        // HBlank HDMA: ROM 0104-0133 (the logo) copied to 8904, and a
        // transfer active.
        let logo: Vec<u16> = (0x8904..0x8934).collect();
        let mut addrs = logo.clone();
        addrs.push(0xFF55);
        let r = run("worst-case", u64::from(CLOCK_HZ) / 10, &addrs);
        assert_eq!(&r[..48], &LOGO);
        assert_eq!(r[48] & 0x80, 0, "HDMA active");
    }

    #[test]
    fn worst_case_services_the_timer_irq_at_16_khz() {
        // The handler's 16-bit counter at C002 over one frame: 70,224 / 256
        // = 274.3 overflows, each serviced.
        let at = u64::from(CLOCK_HZ) / 10;
        let count = |c| {
            let r = run("worst-case", c, &[0xC002, 0xC003, 0xFFFF, 0xFF06]);
            assert_eq!((r[2], r[3]), (0x06, 0xE0), "IE, TMA");
            u16::from_le_bytes([r[0], r[1]])
        };
        let n = count(at + 70_224).wrapping_sub(count(at));
        assert!((274..=275).contains(&n), "{n} timer IRQs in a frame");
        // The vectors: STAT jumps past the timer handler.
        let rom = build("worst-case").unwrap();
        assert_eq!(rom[0x48..0x4B], *STAT_JP);
        assert_eq!(rom[0x50..0x64], TIMER_HANDLER_NR50);
        assert_eq!(
            build("worst-case@0").unwrap()[0x50..0x64],
            TIMER_HANDLER_HRAM
        );
        assert_eq!(
            rom[0x68..0x68 + STAT_HANDLER_NR50.len()],
            *STAT_HANDLER_NR50
        );
    }

    #[test]
    fn worst_case_plays_pcm_through_nr50() {
        // At 16 points 1,000 clocks apart (about 4 samples): NR50's left
        // volume is the sample the counter at C002 points to (or the one
        // before, if the run stopped inside the handler); the right volume
        // is that, or that XOR 7 after the STAT handler's toggle.
        let at = u64::from(CLOCK_HZ) / 10;
        let mut seen = [false; 8];
        for k in 0..16 {
            let r = run("worst-case", at + k * 1_000, &[0xC002, 0xFF24]);
            let (pos, nr50) = (r[0], r[1]);
            let s = |p: u8| PCM_TABLE[usize::from(p)] >> 4;
            let (left, right) = (nr50 >> 4 & 7, nr50 & 7);
            let ok = |s: u8| left == s && (right == s || right == s ^ 7);
            assert!(
                ok(s(pos)) || ok(s(pos.wrapping_sub(1))),
                "{k}: {pos} {nr50:#04x}"
            );
            seen[usize::from(left)] = true;
        }
        assert!(seen.iter().filter(|&&b| b).count() >= 4, "{seen:?}");
    }

    #[test]
    fn worst_case_writes_scx_once_per_visible_line() {
        // Any frame-long window holds 144 mode-3 periods: SCX advances by
        // exactly 144 (one INC per line, none in VBlank).
        let at = u64::from(CLOCK_HZ) / 10;
        let a = run("worst-case", at, &[0xFF43]);
        let b = run("worst-case", at + 70_224, &[0xFF43]);
        assert_eq!(b[0].wrapping_sub(a[0]), 144);
    }

    #[test]
    fn worst_case_plays_all_four_channels_on_both_sides() {
        // NR50, NR51, NR52 half a second in: the sweep (negate, shift 0)
        // has run 64 calculations and left CH1 on.
        let r = run(
            "worst-case",
            u64::from(CLOCK_HZ) / 2,
            &[0xFF24, 0xFF25, 0xFF26],
        );
        // NR50 (the PCM samples, worst_case_plays_pcm_through_nr50): VIN
        // off.
        assert_eq!([r[0] & 0x88, r[1], r[2]], [0x00, 0xFF, 0xFF]);
        // Masks: one channel each, and none at all (the plain program).
        for ch in 0..4u8 {
            let r = run(&format!("worst-case@{:X}", 1 << ch), 70_224, &[0xFF26]);
            assert_eq!(r[0], 0xF0 | 1 << ch, "CH{}", ch + 1);
        }
        assert_eq!(
            build("worst-case@0").unwrap()[0x150..0x150 + WORST_CASE.len()],
            *WORST_CASE
        );
        // Same code (the titles differ).
        assert_eq!(
            build("worst-case@F").unwrap()[0x150..],
            build("worst-case").unwrap()[0x150..]
        );
        assert!(build("worst-case@G").is_none() && build("worst-case@10").is_none());
    }

    #[test]
    fn apu_writes_toggles_nr50_at_8_khz_with_every_channel_on() {
        // Over one frame: TMA, TAC, IE, NR52 (all four on), NR51.
        let addrs = [0xFF06, 0xFF07, 0xFFFF, 0xFF26, 0xFF25];
        let r = run("apu-writes", 70_224, &addrs);
        assert_eq!(r, [0xE0, 0xFD, 0x04, 0xFF, 0xFF]);
        // The handler ran an odd and an even number of times 512 clocks
        // apart: NR50 (or FF80) toggled between them.
        let at = |name, c| run(name, c, &[0xFF24, 0xFF80]);
        let (a, b) = (at("apu-writes", 70_224), at("apu-writes", 70_224 + 512));
        assert_eq!(a[0] ^ b[0], 0x07);
        let (a, b) = (
            at("apu-writes@hram", 70_224),
            at("apu-writes@hram", 70_224 + 512),
        );
        assert_eq!((a[0], a[1] ^ b[1]), (0x77, 0x07));
    }

    #[test]
    fn cgb_raster_writes_palettes_and_scx_every_line() {
        // Double speed, LCD and BG on, BCPS auto-incrementing. One line
        // (456 dots) later the loop has advanced BCPS by 34 and written a
        // new SCX.
        let at = u64::from(CLOCK_HZ) / 10;
        let a = run("cgb-raster", at, &[0xFF4D, 0xFF40, 0xFF68, 0xFF43]);
        let b = run("cgb-raster", at + 456, &[0xFF68, 0xFF43]);
        assert_eq!(a[0] & 0x80, 0x80, "double speed");
        assert_eq!(a[1], 0x91);
        assert_eq!(a[2] & 0x80, 0x80, "auto-increment");
        assert_ne!(a[2], b[0], "BCPS advanced");
        assert_ne!(a[3], b[1], "SCX written");
    }

    #[test]
    fn dmg_raster_draws_bg_window_and_ten_objects_per_line_in_dmg_mode() {
        // Object shades only DMG mode can produce: the caller's OBP0/OBP1
        // colors (CGB mode would use OBJ palette RAM instead).
        let red = [1, 2, 3, 4].map(|r| rgb555(r * 7, 0, 0));
        let green = [1, 2, 3, 4].map(|g| rgb555(0, g * 7, 0));
        let config = Config {
            dmg_palettes: DmgPalettes {
                obj0: red,
                obj1: green,
                ..DmgPalettes::GRAYSCALE
            },
        };
        let at = u64::from(CLOCK_HZ) / 10;
        let (r, frame) = run_frame("dmg-raster", at, true, config, &[0xFF4D, 0xFF40]);
        assert_eq!(r[0], 0xFF, "KEY1 locked: DMG compatibility mode");
        assert_eq!(r[1], 0xF7, "LCD, window, 8x16 objects, BG");
        let obj = |p: u16| red.contains(&p) || green.contains(&p);
        let changes = |row: &[u16]| row.windows(2).filter(|w| w[0] != w[1]).count();
        for y in 0..SCREEN_HEIGHT {
            let row = &frame[y * SCREEN_WIDTH..(y + 1) * SCREEN_WIDTH];
            // Objects: on lines 0-15, 36-51, 72-87, 108-123 (8x16 at
            // Y=16+36k), the tenth (X=62, OBP1) visible past the ninth.
            let want = (y % 36) < 16 && y < 124;
            assert_eq!(row.iter().any(|&p| obj(p)), want, "objects on line {y}");
            assert_eq!(
                row[56..62].iter().any(|&p| green.contains(&p)),
                want,
                "tenth object, line {y}"
            );
            assert!(row[62..].iter().all(|&p| !obj(p)), "line {y}");
            // BG (x 64-79, no objects): four colors across each tile row,
            // so nearly every pixel differs from the last.
            assert!(changes(&row[64..80]) >= 12, "BG on line {y}");
            // Window (x 80-159, WX=87): solid tile 4, so one shade, or
            // two around the mid-line BGP write.
            assert!(changes(&row[80..]) <= 1, "window on line {y}");
        }
    }

    #[test]
    fn oam_lines_rewrites_four_object_xs_in_every_hblank() {
        // The X of objects 0, 10, 20 and 30, a frame apart: all four took
        // every write (OAM was open), 144 a frame (one per visible line).
        let xs = [0xFE01, 0xFE29, 0xFE51, 0xFE79];
        let at = u64::from(CLOCK_HZ) / 10;
        let mut prev: Option<u8> = None;
        for k in 0..4 {
            let r = run("oam-lines", at + k * 70_224, &xs);
            assert!(r.iter().all(|&x| x == r[0]), "frame {k}: {r:?}");
            if let Some(p) = prev {
                assert_eq!(r[0].wrapping_sub(p), 144, "frame {k}");
            }
            prev = Some(r[0]);
        }
        // The rest of OAM is as set up: 8x16 objects at Y=16+36k, X=8+6i,
        // and LCDC has the LCD, 8x16 objects, objects and BG on.
        let r = run(
            "oam-lines",
            at,
            &[0xFE00, 0xFE05, 0xFE9C, 0xFE9D, 0xFF40, 0xFF4D],
        );
        assert_eq!(r[..5], [16, 14, 124, 62, 0x97]);
        assert_eq!(r[5] & 0x80, 0, "single speed");
    }

    // --- Storms ---

    /// Runs `name` to clock `start`, then to each of the next `n` clocks,
    /// and returns `peek(addr)` at each (the state after the instruction
    /// that reaches the clock).
    fn samples(name: &str, start: u64, n: u64, addr: u16) -> Vec<u8> {
        let rom = build(name).unwrap();
        let mut mem = Box::new(Memory::new());
        let mut emu = Emulator::new(&mut mem);
        emu.load(&rom, &mut [], Config::default()).unwrap();
        let mut frame: Box<Frame> = vec![WHITE; SCREEN_WIDTH * SCREEN_HEIGHT]
            .into_boxed_slice()
            .try_into()
            .unwrap();
        (start..start + n)
            .map(|t| {
                while emu.clock() < t {
                    let _ = emu.run_until(t, &mut frame, &mut []);
                }
                emu.peek(addr)
            })
            .collect()
    }

    /// How often `peek(addr) & mask` changes in `period` clocks of `name`,
    /// from 0.1 s in, having checked that it repeats with that period (two
    /// periods compared clock by clock).
    fn changes_per_period(name: &str, addr: u16, mask: u8, period: u64) -> usize {
        let s = samples(name, u64::from(CLOCK_HZ) / 10, 2 * period + 1, addr);
        let p = period as usize;
        assert!(
            (0..=p).all(|i| (s[i] ^ s[i + p]) & mask == 0),
            "{name}: {addr:#06x} does not repeat every {period} clocks"
        );
        s[..=p]
            .windows(2)
            .filter(|w| (w[0] ^ w[1]) & mask != 0)
            .count()
    }

    /// Events per 70,224-clock frame, rounded, at `n` per `clocks`.
    fn per_frame(n: u64, clocks: u64) -> u64 {
        (n * 70_224 + clocks / 2) / clocks
    }

    /// Clocks of one pass of a storm loop of 2 M-cycle steps, double speed.
    const STORM_LOOP_CLOCKS: u64 = 2 * STORM_LOOP_MCYCLES;

    #[test]
    fn storm_control_writes_hram_64_times_per_131_mcycles_over_the_background() {
        // Every write changes FF80 (00 / FF): 64 per loop pass of 262
        // clocks in double speed, 17,154 per 70,224-clock frame.
        assert_eq!(
            changes_per_period("storm-control", 0xFF80, 0xFF, STORM_LOOP_CLOCKS),
            64
        );
        assert_eq!(per_frame(64, STORM_LOOP_CLOCKS), 17_154);
        // The background: double speed, the picture (LCDC, object 9's X,
        // WX), all four channels on both sides, no interrupts.
        let at = u64::from(CLOCK_HZ) / 10;
        let addrs = [0xFF4D, 0xFF40, 0xFE25, 0xFF4B, 0xFF26, 0xFF25, 0xFFFF];
        let r = run("storm-control", at, &addrs);
        assert_eq!(r[0] & 0x80, 0x80, "double speed");
        assert_eq!(r[1..], [0xB7, 62, 0x57, 0xFF, 0xFF, 0x00]);
        // Single speed: the same loop at 4 clocks per M-cycle.
        let r = run("storm-control@hram:1x", at, &[0xFF4D]);
        assert_eq!(r[0] & 0x80, 0, "single speed");
        assert_eq!(
            changes_per_period("storm-control@hram:1x", 0xFF80, 0xFF, 2 * STORM_LOOP_CLOCKS),
            64
        );
        // Every listed storm is a ROM; a variant needs a known kind.
        for name in NAMES.iter().filter(|n| n.starts_with("storm-")) {
            assert!(storm(name).is_some(), "{name}");
        }
        assert_eq!(NAMES.iter().filter(|n| n.starts_with("storm-")).count(), 6);
        for bad in ["storm-nope", "storm-control@x", "storm-control:1x"] {
            assert!(build(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn storm_apu_writes_nr32_every_2_mcycles_with_every_channel_playing() {
        // NR32 reads back bits 5-6 (the volume): 20 / 40 alternate.
        assert_eq!(
            changes_per_period("storm-apu", 0xFF1C, 0x60, STORM_LOOP_CLOCKS),
            64
        );
        let at = u64::from(CLOCK_HZ) / 10;
        let r = run("storm-apu", at, &[0xFF26, 0xFF4D, 0xFF40]);
        assert_eq!(r[0], 0xFF, "APU and all four channels on");
        assert_eq!((r[1] & 0x80, r[2]), (0x80, 0xB7));
        // Variants: another register (CH2's envelope keeps its DAC on),
        // the APU off, and none for a register outside FF10-FF3F.
        assert_eq!(
            changes_per_period("storm-apu@17", 0xFF17, 0xFF, STORM_LOOP_CLOCKS),
            64
        );
        assert_eq!(run("storm-apu@17", at, &[0xFF26])[0], 0xFF);
        assert_eq!(run("storm-apu@off", at, &[0xFF26])[0] & 0x8F, 0);
        assert_eq!(
            build("storm-apu").unwrap()[0x150..],
            build("storm-apu@1c").unwrap()[0x150..]
        );
        for bad in [
            "storm-apu@40",
            "storm-apu@0f",
            "storm-apu@1",
            "storm-apu@xx",
        ] {
            assert!(build(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn storm_spsw_switches_speed_32_times_per_332_clocks() {
        // KEY1 bit 7 is the speed. Per pair of switches: LD (HL),B and the
        // one-byte STOP in single speed (12 clocks), the idle M-cycle and
        // the same two in double speed (8 clocks); 16 pairs and the JR in
        // single speed (12 clocks): 332 clocks, 6,769 switches a frame.
        assert_eq!(changes_per_period("storm-spsw", 0xFF4D, 0x80, 332), 32);
        assert_eq!(per_frame(32, 332), 6_769);
        // The interrupt that keeps STOP at one byte stays pending and
        // unserviced, and the picture is on.
        let at = u64::from(CLOCK_HZ) / 10;
        let r = run("storm-spsw", at, &[0xFFFF, 0xFF0F, 0xFF40]);
        assert_eq!((r[0], r[1] & 0x04, r[2]), (0x04, 0x04, 0xB7));
        assert!(build("storm-spsw@x").is_none());
    }

    #[test]
    fn storm_div_holds_div_at_zero_with_the_timer_on() {
        // DIV (the counter's bits 8-15, or 9-16 in double speed) would
        // count once every 128 clocks: it reads 0 at each of 64 points
        // 1,111 clocks apart only because the loop keeps resetting it.
        let at = u64::from(CLOCK_HZ) / 10;
        for k in 0..64 {
            let r = run("storm-div", at + k * 1_111, &[0xFF04]);
            assert_eq!(r[0], 0, "DIV at point {k}");
        }
        let r = run("storm-div", at, &[0xFF07, 0xFF06, 0xFF4D, 0xFF40, 0xFF26]);
        assert_eq!(r[0] & 0x07, 0x05, "TAC: on, 262 kHz");
        assert_eq!((r[1], r[2] & 0x80, r[3], r[4]), (0xFF, 0x80, 0xB7, 0xFF));
        // The TAC variants write TAC itself: on / off, and two rates.
        assert_eq!(
            changes_per_period("storm-div@tac", 0xFF07, 0x07, STORM_LOOP_CLOCKS),
            64
        );
        assert_eq!(
            changes_per_period("storm-div@tac-rate", 0xFF07, 0x07, STORM_LOOP_CLOCKS),
            64
        );
        assert_eq!(
            build("storm-div").unwrap()[0x150..],
            build("storm-div@div").unwrap()[0x150..]
        );
    }

    /// The source blocks ([`storm_gdma_table`]'s numbers) in the 16 VRAM
    /// blocks 8800-88FF of `storm-gdma` at clock `at`, lowest first.
    fn gdma_blocks(name: &str, at: u64) -> Vec<u16> {
        let addrs: Vec<u16> = (0x8800..0x8900).collect();
        let vram = run(name, at, &addrs);
        let mut ks: Vec<u16> = vram
            .as_chunks::<16>()
            .0
            .iter()
            .map(|b| {
                assert!(b[2..].iter().all(|&x| x == 0x5A), "{at}: {b:02x?}");
                u16::from_le_bytes([b[0], b[1]])
            })
            .collect();
        ks.sort_unstable();
        ks
    }

    #[test]
    fn storm_gdma_copies_32_blocks_per_675_mcycles_to_8800() {
        // A block is LD (HL),B, LD (C),A and the transfer's 17 M-cycles;
        // 32 of them and the JR: 675 M-cycles, 1,350 clocks in double
        // speed, 1,665 blocks a frame. The source runs on through the
        // address space, so the 16 destination blocks (the destination's
        // high byte is rewritten before each) hold the last 16 source
        // blocks, which the table in ROM bank 1 numbers: ten loop passes
        // later they are 320 blocks further.
        let pass = 2 * (32 * (2 + 2 + 17) + 3);
        assert_eq!((pass, per_frame(32, pass)), (1_350, 1_665));
        // The loop starts when the speed switch's halt ends, 65,536 clocks
        // in, and copies the table's blocks (ROM 4000-7FFF) from about
        // 109,000 to 152,000.
        let at = 120_000;
        let a = gdma_blocks("storm-gdma", at);
        let b = gdma_blocks("storm-gdma", at + 10 * pass);
        for ks in [&a, &b] {
            assert!(ks.windows(2).all(|w| w[1] == w[0] + 1), "{ks:?}");
            assert!(ks[0] >= 0x400 && ks[15] < 0x800, "{ks:?}");
        }
        assert_eq!(b[15] - a[15], 320);
        let r = run("storm-gdma", at, &[0xFF4D, 0xFF40]);
        assert_eq!((r[0] & 0x80, r[1]), (0x80, 0xB7));
        // `@free` leaves the destination alone: the same blocks, all over
        // VRAM, 32 per 611 M-cycles.
        assert_eq!(
            build("storm-gdma").unwrap()[0x150..],
            build("storm-gdma@pinned").unwrap()[0x150..]
        );
        let free = build("storm-gdma@free").unwrap();
        assert_ne!(
            free[0x150..0x4000],
            build("storm-gdma").unwrap()[0x150..0x4000]
        );
        assert_eq!(free[0x4000..0x4010], storm_gdma_table()[..16]);
    }

    #[test]
    fn storm_ppu_writes_lcdc_every_2_mcycles_with_the_lcd_on() {
        // LCDC alternates B7 / AB (bits 2, 3 and 4), the LCD staying on.
        let s = samples("storm-ppu", u64::from(CLOCK_HZ) / 10, 600, 0xFF40);
        assert!(s.iter().all(|&v| v == 0xB7 || v == 0xAB), "{s:02x?}");
        assert_eq!(
            changes_per_period("storm-ppu", 0xFF40, 0x1C, STORM_LOOP_CLOCKS),
            64
        );
        let at = u64::from(CLOCK_HZ) / 10;
        let r = run("storm-ppu", at, &[0xFF4D, 0xFF26]);
        assert_eq!((r[0] & 0x80, r[1]), (0x80, 0xFF));
        assert_eq!(
            build("storm-ppu").unwrap()[0x150..],
            build("storm-ppu@lcdc").unwrap()[0x150..]
        );
        // The other registers, and single speed.
        for (v, addr) in [("scx", 0xFF43), ("scy", 0xFF42), ("wx", 0xFF4B)] {
            let name = format!("storm-ppu@{v}");
            assert_eq!(
                changes_per_period(&name, addr, 0xFF, STORM_LOOP_CLOCKS),
                64,
                "{name}"
            );
        }
        for v in ["bgp", "bcpd", "lyc", "stat"] {
            assert!(build(&format!("storm-ppu@{v}")).is_some(), "{v}");
        }
        assert_eq!(
            changes_per_period("storm-ppu@scx:1x", 0xFF43, 0xFF, 2 * STORM_LOOP_CLOCKS),
            64
        );
        assert!(build("storm-ppu@nope").is_none());
        // The sweep variant: any register, 00 / FF.
        assert_eq!(
            changes_per_period("storm-io@80", 0xFF80, 0xFF, STORM_LOOP_CLOCKS),
            64
        );
        assert!(build("storm-io").is_none() && build("storm-io@1").is_none());
    }
}
