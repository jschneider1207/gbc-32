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

/// The ROM image for `name`, or `None` if there is no such ROM.
pub fn build(name: &str) -> Option<Vec<u8>> {
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
}
