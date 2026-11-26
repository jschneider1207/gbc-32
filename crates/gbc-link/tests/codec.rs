//! Frame codec tests: round trips, corruption, resync, oversize, length
//! checks, and a byte soup that must never panic.

use gbc_link::{
    DecodeError, EncodeError, FrameDecoder, MAX_CHUNK, MAX_ENCODED, MAX_FRAME, MAX_STATS_EXTRA,
    Message, NakCode, crc32, encode, ty,
};

fn frame(msg: &Message<'_>, seq: u8) -> Vec<u8> {
    let mut out = [0u8; MAX_FRAME];
    let n = encode(msg, seq, &mut out).unwrap();
    out[..n].to_vec()
}

/// Feeds `bytes` and collects every result, in order, as owned debug
/// strings (messages borrow the decoder).
fn feed_all(d: &mut FrameDecoder, bytes: &[u8]) -> Vec<Result<(u8, String), DecodeError>> {
    let mut out = Vec::new();
    for &b in bytes {
        if let Some(r) = d.feed(b) {
            out.push(r.map(|(s, m)| (s, format!("{m:?}"))));
        }
    }
    out
}

fn decode_one(bytes: &[u8]) -> Result<(u8, String), DecodeError> {
    let mut d = FrameDecoder::new();
    let mut r = feed_all(&mut d, bytes);
    assert_eq!(r.len(), 1, "expected exactly one frame result: {r:?}");
    r.remove(0)
}

fn pattern(n: usize, seed: u8) -> Vec<u8> {
    // Includes zeros, so COBS has blocks to split.
    (0..n)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

fn all_messages<'a>(big: &'a [u8], small: &'a [u8], extra: &'a [u8]) -> Vec<Message<'a>> {
    let mut v = vec![
        Message::Hello { protocol: 1 },
        Message::Hello { protocol: 0xFFFF },
        Message::RomBegin {
            len: 4 << 20,
            hash: 0x0123_4567_89AB_CDEF,
        },
        Message::RomChunk {
            offset: 0,
            data: big,
        },
        Message::RomChunk {
            offset: 0xFFFF_FFFF,
            data: &[],
        },
        Message::RomChunk {
            offset: 1024,
            data: small,
        },
        Message::RomEnd,
        Message::SaveBegin {
            len: 0x8000,
            hash: u64::MAX,
        },
        Message::SaveChunk {
            offset: 7,
            data: big,
        },
        Message::SaveChunk {
            offset: 0,
            data: &[],
        },
        Message::SaveDumpRequest,
        Message::Input { buttons: 0 },
        Message::Input { buttons: 0xA5 },
        Message::Ack,
        Message::HelloReply {
            protocol: 1,
            max_chunk: MAX_CHUNK as u16,
            flash_len: (8 << 20) - 256,
        },
        Message::AlreadyLoaded,
        Message::RomDone { hash: 0 },
        Message::Stats {
            frames: 1,
            cycles_mean: 2,
            cycles_max: 3,
            extra: &[],
        },
        Message::Stats {
            frames: u32::MAX,
            cycles_mean: 0,
            cycles_max: 0x0100_0000,
            extra,
        },
    ];
    for c in 1..=10 {
        v.push(Message::Nak {
            code: NakCode::from_u8(c),
        });
    }
    v.push(Message::Nak {
        code: NakCode::Other(0),
    });
    v.push(Message::Nak {
        code: NakCode::Other(0xFF),
    });
    v
}

#[test]
fn every_message_round_trips() {
    let big = pattern(MAX_CHUNK, 0);
    let small = pattern(300, 9);
    let extra = pattern(MAX_STATS_EXTRA, 3);
    let msgs = all_messages(&big, &small, &extra);
    let mut types = std::collections::BTreeSet::new();
    for (i, m) in msgs.iter().enumerate() {
        for seq in [0u8, 1, 0x7F, 0xFF, i as u8] {
            let f = frame(m, seq);
            assert!(f.len() <= MAX_FRAME);
            assert_eq!(f.last(), Some(&0));
            assert!(!f[..f.len() - 1].contains(&0), "0x00 inside a frame");
            let mut d = FrameDecoder::new();
            let mut got = None;
            for &b in &f {
                if let Some(r) = d.feed(b) {
                    let (s, back) = r.unwrap();
                    assert_eq!(s, seq);
                    assert_eq!(&back, m);
                    got = Some(());
                }
            }
            assert!(got.is_some(), "no frame for {m:?}");
        }
        types.insert(m.type_code());
    }
    assert_eq!(types.len(), 14, "every message type covered");
}

#[test]
fn max_size_messages_fit_max_frame_exactly() {
    let big = pattern(MAX_CHUNK, 1);
    // All non-zero data maximizes COBS overhead.
    let nz = vec![0x55u8; MAX_CHUNK];
    let mut longest = 0;
    for data in [&big[..], &nz[..]] {
        let f = frame(
            &Message::RomChunk {
                offset: 0x0101_0101,
                data,
            },
            0x42,
        );
        longest = longest.max(f.len());
    }
    assert!(longest <= MAX_FRAME);
    assert_eq!(MAX_FRAME, 1040);
    assert_eq!(MAX_ENCODED, MAX_FRAME - 1);
}

#[test]
fn feed_slice_returns_frames_one_at_a_time() {
    let mut wire = frame(&Message::Ack, 1);
    wire.extend(frame(&Message::RomEnd, 2));
    let mut d = FrameDecoder::new();
    let (n, r) = d.feed_slice(&wire);
    assert_eq!(r, Some(Ok((1, Message::Ack))));
    let rest = &wire[n..];
    let (m, r) = d.feed_slice(rest);
    assert_eq!(r, Some(Ok((2, Message::RomEnd))));
    assert_eq!(m, rest.len());
    assert_eq!(d.feed_slice(&[1, 2, 3]), (3, None));
}

#[test]
fn empty_frames_are_ignored() {
    let mut wire = vec![0, 0, 0];
    wire.extend(frame(&Message::Input { buttons: 3 }, 9));
    wire.extend([0, 0]);
    let mut d = FrameDecoder::new();
    let r = feed_all(&mut d, &wire);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].as_ref().unwrap().0, 9);
}

#[test]
fn any_single_bit_flip_is_detected() {
    let data = pattern(40, 5);
    let f = frame(
        &Message::RomChunk {
            offset: 77,
            data: &data,
        },
        3,
    );
    // Every bit of every byte before the delimiter.
    for i in 0..f.len() - 1 {
        for bit in 0..8 {
            let mut g = f.clone();
            g[i] ^= 1 << bit;
            let mut d = FrameDecoder::new();
            let r = feed_all(&mut d, &g);
            // A flip to 0x00 splits the frame in two; every piece must be
            // an error, and no piece may decode as a message.
            assert!(!r.is_empty(), "byte {i} bit {bit}: no result");
            for x in &r {
                assert!(x.is_err(), "byte {i} bit {bit}: accepted {x:?}");
            }
        }
    }
}

#[test]
fn bit_flip_in_body_is_a_crc_error() {
    let data = pattern(40, 5);
    let f = frame(
        &Message::RomChunk {
            offset: 77,
            data: &data,
        },
        3,
    );
    // Flipping bit 7 never makes a byte 0x00 here unless it was 0x80. A
    // flipped data byte must be a CRC error; a flipped COBS code byte may
    // instead break the block structure.
    let mut crc_errors = 0;
    for i in 0..f.len() - 1 {
        let mut g = f.clone();
        g[i] ^= 0x80;
        if g[i] == 0 {
            continue;
        }
        match decode_one(&g) {
            Err(DecodeError::BadCrc) => crc_errors += 1,
            Err(DecodeError::BadCobs) => {}
            r => panic!("byte {i}: {r:?}"),
        }
    }
    assert!(crc_errors > 40, "only {crc_errors} CRC errors");
}

#[test]
fn garbage_then_valid_frame_resyncs() {
    let good = frame(&Message::RomDone { hash: 0xDEAD_BEEF }, 0x10);
    let mut wire = vec![0x12, 0x34, 0xFF, 0x01];
    // A partial valid frame with its delimiter lost.
    wire.extend(&good[..good.len() / 2]);
    wire.push(0);
    wire.extend(&good);
    let mut d = FrameDecoder::new();
    let r = feed_all(&mut d, &wire);
    assert_eq!(r.len(), 2, "{r:?}");
    assert!(r[0].is_err());
    assert_eq!(
        r[1],
        Ok((
            0x10,
            format!("{:?}", Message::RomDone { hash: 0xDEAD_BEEF })
        ))
    );
}

#[test]
fn oversize_frame_is_dropped_then_decoder_recovers() {
    let mut wire = vec![0x7Eu8; MAX_ENCODED + 1];
    wire.push(0);
    wire.extend(frame(&Message::Ack, 5));
    // Far beyond the buffer, too.
    wire.extend(vec![0x01u8; 5 * MAX_ENCODED]);
    wire.push(0);
    wire.extend(frame(&Message::AlreadyLoaded, 6));
    let mut d = FrameDecoder::new();
    let r = feed_all(&mut d, &wire);
    assert_eq!(r.len(), 4, "{r:?}");
    assert_eq!(r[0], Err(DecodeError::Oversize));
    assert_eq!(r[1].as_ref().unwrap().0, 5);
    assert_eq!(r[2], Err(DecodeError::Oversize));
    assert_eq!(r[3].as_ref().unwrap().0, 6);
}

#[test]
fn frame_of_exactly_the_buffer_size_is_accepted() {
    // A frame that fills the decoder's buffer exactly is valid; one byte
    // more is oversize.
    let mut d = gbc_link::Decoder::<8>::new();
    let f = frame(&Message::Input { buttons: 1 }, 1); // 1+1+1+4 = 7 payload
    assert_eq!(f.len() - 1, 8);
    let r: Vec<_> = f
        .iter()
        .filter_map(|&b| d.feed(b).map(|r| r.is_ok()))
        .collect();
    assert_eq!(r, [true]);
    let mut d = gbc_link::Decoder::<7>::new();
    let r: Vec<_> = f
        .iter()
        .filter_map(|&b| d.feed(b).map(|r| r.err()))
        .collect();
    assert_eq!(r, [Some(DecodeError::Oversize)]);
}

/// Builds a frame with an arbitrary type and raw body (valid CRC).
fn raw_frame(t: u8, seq: u8, body: &[u8]) -> Vec<u8> {
    let mut payload = vec![t, seq];
    payload.extend_from_slice(body);
    let crc = crc32(&payload);
    payload.extend(crc.to_le_bytes());
    let mut out = vec![0u8; gbc_link::cobs::max_encoded_len(payload.len()) + 1];
    let n = gbc_link::cobs::encode(&payload, &mut out).unwrap();
    out.truncate(n);
    out.push(0);
    out
}

#[test]
fn wrong_body_length_is_bad_length() {
    let fixed: &[(u8, usize)] = &[
        (ty::HELLO, 2),
        (ty::ROM_BEGIN, 12),
        (ty::ROM_END, 0),
        (ty::SAVE_BEGIN, 12),
        (ty::SAVE_DUMP_REQUEST, 0),
        (ty::INPUT, 1),
        (ty::ACK, 0),
        (ty::NAK, 1),
        (ty::HELLO_REPLY, 8),
        (ty::ALREADY_LOADED, 0),
        (ty::ROM_DONE, 8),
    ];
    for &(t, len) in fixed {
        assert!(
            decode_one(&raw_frame(t, 4, &vec![1; len])).is_ok(),
            "{t:#x}"
        );
        if len > 0 {
            assert_eq!(
                decode_one(&raw_frame(t, 4, &vec![1; len - 1])),
                Err(DecodeError::BadLength { ty: t, seq: 4 }),
                "{t:#x} truncated"
            );
        }
        assert_eq!(
            decode_one(&raw_frame(t, 4, &vec![1; len + 1])),
            Err(DecodeError::BadLength { ty: t, seq: 4 }),
            "{t:#x} long"
        );
    }
    // Variable bodies: a minimum and a maximum.
    for (t, min, max) in [
        (ty::ROM_CHUNK, 4, 4 + MAX_CHUNK),
        (ty::SAVE_CHUNK, 4, 4 + MAX_CHUNK),
        (ty::STATS, 12, 12 + MAX_STATS_EXTRA),
    ] {
        assert!(decode_one(&raw_frame(t, 4, &vec![1; min])).is_ok());
        assert!(decode_one(&raw_frame(t, 4, &vec![1; max])).is_ok());
        assert_eq!(
            decode_one(&raw_frame(t, 4, &vec![1; min - 1])),
            Err(DecodeError::BadLength { ty: t, seq: 4 })
        );
    }
    // A chunk over MAX_CHUNK needs a bigger decoder to even arrive.
    let f = raw_frame(ty::ROM_CHUNK, 4, &vec![1; 4 + MAX_CHUNK + 1]);
    let mut d = gbc_link::Decoder::<2000>::new();
    let r: Vec<_> = f
        .iter()
        .filter_map(|&b| d.feed(b).map(|r| r.err()))
        .collect();
    assert_eq!(
        r,
        [Some(DecodeError::BadLength {
            ty: ty::ROM_CHUNK,
            seq: 4
        })]
    );
}

#[test]
fn unknown_types_are_errors() {
    let known: Vec<u8> = {
        let (b, s, e) = (vec![], vec![], vec![]);
        all_messages(&b, &s, &e)
            .iter()
            .map(Message::type_code)
            .collect()
    };
    for t in 0..=255u8 {
        if known.contains(&t) {
            continue;
        }
        let r = decode_one(&raw_frame(t, 8, &[]));
        assert_eq!(r, Err(DecodeError::UnknownType { ty: t, seq: 8 }));
        assert_eq!(r.as_ref().unwrap_err().nak_code(), NakCode::UnexpectedType);
        assert_eq!(r.unwrap_err().seq(), Some(8));
    }
}

#[test]
fn short_and_non_cobs_frames() {
    // Payload of 5 bytes (one short of type+seq+crc).
    let mut out = [0u8; 16];
    let n = gbc_link::cobs::encode(&[1, 2, 3, 4, 5], &mut out).unwrap();
    let mut f = out[..n].to_vec();
    f.push(0);
    assert_eq!(decode_one(&f), Err(DecodeError::TooShort));
    // A code byte running past the end.
    assert_eq!(decode_one(&[0x09, 1, 2, 0]), Err(DecodeError::BadCobs));
    assert_eq!(DecodeError::BadCrc.nak_code(), NakCode::BadCrc);
    assert_eq!(DecodeError::BadCobs.nak_code(), NakCode::BadFrame);
    assert_eq!(DecodeError::BadCrc.seq(), None);
}

#[test]
fn encode_errors() {
    let big = vec![0u8; MAX_CHUNK + 1];
    let mut out = [0u8; 2 * MAX_FRAME];
    assert_eq!(
        encode(
            &Message::RomChunk {
                offset: 0,
                data: &big
            },
            0,
            &mut out
        ),
        Err(EncodeError::BodyTooLarge)
    );
    assert_eq!(
        encode(
            &Message::SaveChunk {
                offset: 0,
                data: &big
            },
            0,
            &mut out
        ),
        Err(EncodeError::BodyTooLarge)
    );
    let extra = vec![0u8; MAX_STATS_EXTRA + 1];
    let stats = Message::Stats {
        frames: 0,
        cycles_mean: 0,
        cycles_max: 0,
        extra: &extra,
    };
    assert_eq!(encode(&stats, 0, &mut out), Err(EncodeError::BodyTooLarge));
    // Every buffer shorter than the frame fails cleanly.
    let f = frame(&Message::Hello { protocol: 1 }, 0);
    for len in 0..f.len() {
        assert_eq!(
            encode(&Message::Hello { protocol: 1 }, 0, &mut out[..len]),
            Err(EncodeError::BufferTooSmall),
            "len {len}"
        );
    }
    assert_eq!(
        encode(&Message::Hello { protocol: 1 }, 0, &mut out[..f.len()]),
        Ok(f.len())
    );
}

/// Deterministic LCG byte soup, biased toward 0x00 so many short frames
/// get parsed, plus mutated valid frames. Must never panic.
#[test]
fn byte_soup_never_panics() {
    let mut s: u64 = 0x1234_5678_9ABC_DEF0;
    let mut next = || {
        s = s
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (s >> 33) as u32
    };
    let mut d = FrameDecoder::new();
    let mut small = gbc_link::Decoder::<16>::new();
    let mut results = 0usize;
    for _ in 0..2_000_000 {
        let r = next();
        let b = if r % 16 == 0 { 0 } else { (r >> 8) as u8 };
        results += usize::from(d.feed(b).is_some());
        let _ = small.feed(b);
    }
    assert!(results > 1000);
    // Valid frames with random byte corruptions.
    let data = pattern(200, 7);
    let base = frame(
        &Message::SaveChunk {
            offset: 3,
            data: &data,
        },
        1,
    );
    for _ in 0..20_000 {
        let mut g = base.clone();
        let k = 1 + next() % 4;
        for _ in 0..k {
            let i = next() as usize % g.len();
            g[i] = next() as u8;
        }
        for &b in &g {
            let _ = d.feed(b);
        }
        // Back in sync afterwards.
        let _ = d.feed(0);
        let r = feed_all(&mut d, &frame(&Message::Ack, 2));
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].as_ref().unwrap().0, 2);
    }
}
