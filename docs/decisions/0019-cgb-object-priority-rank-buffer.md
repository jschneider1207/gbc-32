# 0019: CGB object priority through a per-pixel rank buffer

- Status: Accepted
- Date: 2026-11-22

## Context
On CGB, object-to-object priority follows the OAM index; in DMG
compatibility mode it follows X (Pan Docs: OAM byte 3 / object priority).
Mode-3 stall timing (`sorted_object_stalls`) depends on objects being
fetched in X order, and `fix_attrs` and the renderer's fetches use the same
order. SameBoy resolves the priority per pixel when a fetched object row is
merged into the FIFO (`fifo_overlay_object_row`), not by fetch order.

## Decision
Implemented in commit 17db555.
- Objects are still fetched in X order. `order` stays the X sort, so stall
  timing, `fix_attrs` and fetches are unchanged.
- The OAM-index rule is applied where a fetched object merges into the line
  buffer. A per-pixel rank buffer, `Memory::obj_buf.rank`, holds the object's
  OAM slot in CGB mode and 0 in compatibility mode. A later-fetched object
  with a lower OAM index replaces opaque pixels that have not been output
  yet.
- The rank compare runs only when the object's 8 pixels overlap an earlier
  object.
- The buffer lives in `Memory`, not `Renderer`: in `Renderer` it would push
  `Hw` over its 864 B size assert.
- OPRI writes after boot are stored but do not change the priority mode.
  SameBoy applies them only while the boot ROM runs (or in KEY0 PGB mode)
  (`memory.c`), so after boot the mode is fixed by reset.

## Alternatives considered
- Fetching (and sorting) objects by OAM index: rejected, since it would
  change the X-ordered mode-3 stalls that the timing already models.
- A rank buffer in `Renderer`: rejected for the `Hw` size assert above
  (`Hw` would be 1,008 B against the 864 B assert).
- Calling `fetch_object` once per object (the first version): cgb-acid2
  +4.21% x86 Ir, over the 3% bench gate. One out-of-line `fetch_objects`
  call per span, with the rank compare only on overlap, replaced it
  (implementing agent's measurement, session 07; not in the commit body).

## Consequences
- cgb-acid2 passes; cgb-acid-hell goes from 23 to 2 differing pixels.
- DMG compatibility mode is bit-identical: the diff-fingerprint against
  a431161 differs only for cgb-acid2 and cgb-acid-hell (of 3822).
- Bench (x86 Ir): cgb-acid2 +1.94%, cgb-raster +1.72%, worst-case +0.47%,
  others within +-0.33%.
- Total `.text` 83,434 -> 83,342 B (fetch loop moved out of line from
  render; reset palette conversion is a loop of calls instead of an
  unrolled 650-byte loop).
- Split spans stay identical to one span.
