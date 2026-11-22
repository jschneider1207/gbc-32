//! Object (sprite) selection for one scanline: the mode-2 OAM scan and the
//! drawing priority order.
//!
//! Hardware behavior (Pan Docs: Object Attribute Memory, "Selection
//! priority" and "Drawing priority"):
//! - The PPU scans OAM in order (FE00 to FE9F) and selects the first (up to)
//!   10 objects whose Y range covers LY. Only Y is checked: objects with
//!   X=0 or X>=168 are invisible but still count toward the 10.
//! - On CGB the scan happens even while LCDC.1 (OBJ enable) is clear; that
//!   bit is only checked when pixels are mixed (SameBoy Core/display.c,
//!   "On the CGB, this bit is checked only when the pixel is actually
//!   popped from the FIFO").
//! - Drawing priority in DMG compatibility mode on a CGB is DMG-style: the
//!   smaller X wins, and for equal X the earlier OAM entry wins. The CGB
//!   boot ROM selects this through OPRI (FF6C) bit 0 = 1 for DMG games
//!   (Pan Docs: CGB Registers, OPRI, "1 = DMG-style priority"; SameBoy
//!   Core/memory.c sets `object_priority = GB_OBJECT_PRIORITY_X` when OPRI
//!   bit 0 is written as 1, and Core/display.c then uses priority 0 for
//!   every object, so the first object fetched (lowest X, then lowest OAM
//!   index) keeps each pixel). CGB mode (OAM-index priority, OPRI = 0)
//!   arrives in M5.

/// At most this many objects are selected per line (Pan Docs: OAM).
pub(crate) const MAX_PER_LINE: usize = 10;

/// The objects selected by the OAM scan for one line, in OAM order. Small
/// and `Copy` (31 bytes): the timing code keeps one per line.
///
/// Y and X are the values the scan read; the renderer reads the tile index
/// and attributes from OAM when it fetches the object (SameBoy keeps
/// `objects_x`/`objects_y` from the scan the same way).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LineObjects {
    len: u8,
    /// OAM entry number (0-39) of each selected object.
    index: [u8; MAX_PER_LINE],
    /// OAM byte 0 (screen Y + 16).
    y: [u8; MAX_PER_LINE],
    /// OAM byte 1 (screen X + 8).
    x: [u8; MAX_PER_LINE],
}

impl LineObjects {
    pub(crate) const EMPTY: Self = Self {
        len: 0,
        index: [0; MAX_PER_LINE],
        y: [0; MAX_PER_LINE],
        x: [0; MAX_PER_LINE],
    };

    /// Runs the whole OAM scan for line `ly` at once. `tall` is LCDC.2
    /// (8x16 objects) as of the scan.
    pub(crate) fn scan(oam: &[u8; 0xA0], ly: u8, tall: bool) -> Self {
        let mut objs = Self::EMPTY;
        for (i, entry) in oam.as_chunks::<4>().0.iter().enumerate() {
            objs.consider(i as u8, entry[0], entry[1], ly, tall);
        }
        objs
    }

    /// An object with OAM Y `y` covers line `ly`. `tall`: 8x16 objects.
    #[inline]
    pub(crate) fn covers(y: u8, ly: u8, tall: bool) -> bool {
        let height = if tall { 16 } else { 8 };
        // The object covers screen lines y-16 .. y-16+height-1, i.e.
        // y <= ly+16 < y+height. Widened to u16: no wrap at the top.
        (u16::from(ly) + 16).wrapping_sub(u16::from(y)) < height
    }

    /// One step of the OAM scan: selects OAM entry `index` (with OAM bytes
    /// `y` and `x` as the scan read them) if its Y range covers `ly` and
    /// fewer than 10 objects are selected. Call in OAM order. Exposed so
    /// the timing code can scan incrementally (e.g. with OAM DMA running).
    #[inline]
    pub(crate) fn consider(&mut self, index: u8, y: u8, x: u8, ly: u8, tall: bool) {
        let n = self.len as usize;
        if n >= MAX_PER_LINE {
            return;
        }
        if Self::covers(y, ly, tall) {
            self.index[n] = index;
            self.y[n] = y;
            self.x[n] = x;
            self.len += 1;
        }
    }

    #[inline]
    pub(crate) const fn len(&self) -> usize {
        self.len as usize
    }

    /// The selected objects' X positions (OAM byte 1, screen X + 8) in
    /// OAM order.
    #[cfg(test)]
    pub(crate) fn x_positions(&self) -> &[u8] {
        &self.x[..self.len()]
    }

    /// OAM entry numbers in OAM order.
    #[cfg(test)]
    pub(crate) fn indices(&self) -> &[u8] {
        &self.index[..self.len()]
    }

    /// The selected object at position `slot` (OAM order): `(OAM entry
    /// number, Y, X)`.
    #[inline]
    pub(crate) fn get(&self, slot: usize) -> (u8, u8, u8) {
        let s = slot % MAX_PER_LINE;
        (self.index[s], self.y[s], self.x[s])
    }

    /// Slots (positions in OAM order) from highest to lowest drawing
    /// priority, DMG-style (compatibility mode): ascending X, ties by OAM
    /// order. Only the first [`len`](Self::len) entries are meaningful.
    pub(crate) fn priority_order(&self) -> [u8; MAX_PER_LINE] {
        let mut order = [0u8; MAX_PER_LINE];
        let n = self.len();
        // Stable insertion sort by X; the slots are already in OAM order,
        // so equal X keeps the earlier OAM entry first.
        for slot in 0..n {
            let x = self.x[slot];
            let mut j = slot;
            while j > 0 && self.x[order[j - 1] as usize % MAX_PER_LINE] > x {
                order[j] = order[j - 1];
                j -= 1;
            }
            order[j] = slot as u8;
        }
        order
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oam_with(entries: &[(usize, u8, u8)]) -> [u8; 0xA0] {
        let mut oam = [0u8; 0xA0];
        for &(i, y, x) in entries {
            oam[i * 4] = y;
            oam[i * 4 + 1] = x;
        }
        oam
    }

    #[test]
    fn y_range_8x8_and_8x16() {
        // y=16: screen lines 0-7 (8x8) or 0-15 (8x16).
        let oam = oam_with(&[(3, 16, 50)]);
        assert_eq!(LineObjects::scan(&oam, 0, false).indices(), &[3]);
        assert_eq!(LineObjects::scan(&oam, 7, false).indices(), &[3]);
        assert_eq!(LineObjects::scan(&oam, 8, false).len(), 0);
        assert_eq!(LineObjects::scan(&oam, 8, true).indices(), &[3]);
        assert_eq!(LineObjects::scan(&oam, 15, true).indices(), &[3]);
        assert_eq!(LineObjects::scan(&oam, 16, true).len(), 0);
        // Partly above the screen: y=10 covers lines 0-1 (8x8).
        let oam = oam_with(&[(0, 10, 50)]);
        assert_eq!(LineObjects::scan(&oam, 1, false).len(), 1);
        assert_eq!(LineObjects::scan(&oam, 2, false).len(), 0);
        // y=0 never covers a visible line, even 8x16 (lines -16..-1).
        let oam = oam_with(&[(0, 0, 50)]);
        assert!((0..=153).all(|ly| LineObjects::scan(&oam, ly, true).len() == 0));
    }

    #[test]
    fn eleventh_object_is_dropped_and_offscreen_x_counts() {
        // Entries 0 and 1 are off-screen horizontally (X=0, X=168) but
        // still take two of the 10 slots; entry 2 is on a different line.
        let mut oam = oam_with(&[(0, 20, 0), (1, 20, 168), (2, 40, 30)]);
        for i in 3..12 {
            oam[i * 4] = 20;
            oam[i * 4 + 1] = 8 + i as u8;
        }
        let objs = LineObjects::scan(&oam, 5, false);
        assert_eq!(objs.indices(), &[0, 1, 3, 4, 5, 6, 7, 8, 9, 10]);
        assert_eq!(
            objs.x_positions(),
            &[0, 168, 11, 12, 13, 14, 15, 16, 17, 18]
        );
    }

    #[test]
    fn dmg_priority_is_by_x_then_oam_order() {
        // OAM order: X = 50, 20, 50, 20, 90.
        let oam = oam_with(&[
            (0, 16, 50),
            (1, 16, 20),
            (2, 16, 50),
            (3, 16, 20),
            (4, 16, 90),
        ]);
        let objs = LineObjects::scan(&oam, 0, false);
        let order = objs.priority_order();
        let mut by_index = [0u8; 5];
        for (k, &s) in order[..objs.len()].iter().enumerate() {
            by_index[k] = objs.get(s as usize).0;
        }
        assert_eq!(objs.len(), 5);
        assert_eq!(by_index, [1, 3, 0, 2, 4]);
    }
}
