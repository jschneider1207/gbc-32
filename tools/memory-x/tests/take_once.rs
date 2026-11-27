//! Host tests of the board crate's `take_once` module (review Q4). The
//! board crate builds only for thumbv7em and has no test harness, and the
//! module uses only `core`, so it is compiled here as it is there. It sits
//! in memory-x because that is the root-workspace crate of the board's
//! build support (the linker sections the module's docs depend on).

#[path = "../../../crates/gbc-nucleo-h753/src/take_once.rs"]
mod take_once;

use std::sync::{Arc, Barrier};

fn first_site() -> Option<&'static mut [u8; 4]> {
    crate::take_once!([u8; 4] = [1, 2, 3, 4])
}

fn second_site() -> Option<&'static mut [u8; 4]> {
    crate::take_once!([u8; 4] = [5, 6, 7, 8])
}

#[test]
fn a_site_hands_out_its_buffer_once() {
    let buf = first_site().expect("first take");
    assert_eq!(*buf, [1, 2, 3, 4]);
    buf[0] = 9;
    assert!(first_site().is_none(), "second take of the same site");
    assert!(first_site().is_none(), "third take of the same site");
    assert_eq!(buf[0], 9);
}

#[test]
fn sites_are_independent() {
    let a = second_site().expect("this site's first take");
    assert_eq!(*a, [5, 6, 7, 8]);
    assert!(second_site().is_none());
    // A third site, taken after the second is spent.
    let b = crate::take_once!(u32 = 7).expect("a fresh site");
    assert_eq!(*b, 7);
}

#[test]
fn attributes_reach_the_buffer() {
    let b = crate::take_once!(
        // SAFETY: a host ELF data section; any bytes are fine for `u64`.
        #[unsafe(link_section = ".data.take_once_test")]
        u64 = 0x1234,
    )
    .expect("first take");
    assert_eq!(*b, 0x1234);
}

#[test]
fn one_winner_among_racing_threads() {
    fn racing_site() -> Option<&'static mut u32> {
        crate::take_once!(u32 = 0)
    }
    const THREADS: usize = 8;
    let start = Arc::new(Barrier::new(THREADS));
    let handles: Vec<_> = (0..THREADS)
        .map(|_| {
            let start = Arc::clone(&start);
            std::thread::spawn(move || {
                start.wait();
                racing_site().is_some()
            })
        })
        .collect();
    let winners = handles
        .into_iter()
        .map(|h| h.join().unwrap())
        .filter(|&won| won)
        .count();
    assert_eq!(winners, 1);
}
