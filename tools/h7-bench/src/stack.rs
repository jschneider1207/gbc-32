//! Stack depth, from the paint cortex-m-rt's startup code leaves
//! (`paint-stack`): the stack region, from the end of the statics to the
//! top of RAM, starts out filled with 0xCCCC_CCCC.

const PAINT: u32 = 0xCCCC_CCCC;

/// (deepest stack use so far in bytes, size of the stack region in bytes).
/// A use equal to the size means the stack ran into the statics below it.
pub fn usage() -> (usize, usize) {
    unsafe extern "C" {
        static _stack_end: u32;
        static _stack_start: u32;
    }
    let (end, start) = (&raw const _stack_end, &raw const _stack_start);
    let mut p = end;
    // SAFETY: `_stack_end` and `_stack_start` are cortex-m-rt's linker
    // symbols for the bounds of the stack region, word-aligned RAM that is
    // always readable; the words below the current stack pointer are not
    // otherwise accessed, and a volatile read of them is harmless.
    unsafe {
        while p < start && p.read_volatile() == PAINT {
            p = p.add(1);
        }
    }
    (start as usize - p as usize, start as usize - end as usize)
}
