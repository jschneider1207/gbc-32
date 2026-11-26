//! The PC input bitmap contract: `gbc_link::buttons` (what `gbc-pc` sends
//! in `Message::Input`) decodes, bit for bit, to the Game Boy button of
//! the same name in the firmware (`gbc_firmware::buttons::from_pc`). The
//! two crates don't depend on each other (ADR 0036), so this is the one
//! place both are visible.

use gbc_firmware::buttons::{Buttons, from_pc};
use gbc_link::{FrameDecoder, Message, buttons as link};

#[test]
fn every_link_bit_is_the_firmware_button_of_the_same_name() {
    let pairs = [
        (link::A, Buttons::A),
        (link::B, Buttons::B),
        (link::SELECT, Buttons::SELECT),
        (link::START, Buttons::START),
        (link::RIGHT, Buttons::RIGHT),
        (link::LEFT, Buttons::LEFT),
        (link::UP, Buttons::UP),
        (link::DOWN, Buttons::DOWN),
    ];
    let mut all = 0u8;
    for (bit, b) in pairs {
        assert_eq!(bit.count_ones(), 1, "{b:?}");
        assert_eq!(all & bit, 0, "{b:?} shares a bit");
        all |= bit;
        assert_eq!(from_pc(bit), b, "{b:?}");
    }
    assert_eq!(all, 0xFF);
    assert_eq!(from_pc(link::A | link::DOWN), Buttons::A | Buttons::DOWN);
}

#[test]
fn input_frames_carry_the_bitmap_unchanged() {
    let mut buf = [0u8; 64];
    for bits in [0u8, link::START, 0xFF, link::LEFT | link::B] {
        let n = gbc_link::encode(&Message::Input { buttons: bits }, 7, &mut buf).unwrap();
        let mut dec = FrameDecoder::new();
        let (last, head) = buf[..n].split_last().unwrap();
        for &b in head {
            assert!(dec.feed(b).is_none());
        }
        let got = dec.feed(*last).unwrap().unwrap();
        assert_eq!(got, (7, Message::Input { buttons: bits }));
    }
}

/// `gbc-pc play`'s keys (the desktop front end's, docs/STATUS.md) give the
/// firmware the right button, each alone and all together.
#[test]
fn play_keys_map_to_all_eight_buttons() {
    use gbc_pc::input::{Key, KeySet};
    let pairs = [
        (Key::Up, Buttons::UP),
        (Key::Down, Buttons::DOWN),
        (Key::Left, Buttons::LEFT),
        (Key::Right, Buttons::RIGHT),
        (Key::Z, Buttons::B),
        (Key::X, Buttons::A),
        (Key::Enter, Buttons::START),
        (Key::RightShift, Buttons::SELECT),
        (Key::Backspace, Buttons::SELECT),
    ];
    for (k, b) in pairs {
        assert_eq!(from_pc(KeySet::of(&[k]).buttons()), b, "{k:?}");
    }
    assert_eq!(KeySet::of(&[Key::Esc, Key::F5]).buttons(), 0);
    let all: Vec<Key> = pairs.iter().map(|p| p.0).collect();
    assert_eq!(KeySet::of(&all).buttons(), 0xFF);
    assert_eq!(
        from_pc(KeySet::of(&[Key::X, Key::Down, Key::Esc]).buttons()),
        Buttons::A | Buttons::DOWN
    );
}
