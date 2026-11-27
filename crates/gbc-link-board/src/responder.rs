//! The stop-and-wait half both servers share ([`crate::loader`] and
//! [`crate::link_session`]; `docs/reference/link-protocol.md`): frame
//! capture, resend detection by the raw bytes, the stored reply, and the
//! `Nak` for a frame that did not decode. One copy, so a fix to it (K1:
//! a corrupt resend must not replace the stored reply) reaches both.
//!
//! The caller feeds every received byte to [`Responder::feed`]. A frame
//! that ends there comes back as a [`Received`]:
//! - [`Received::BadFrame`]: it failed COBS, CRC or its length check. Its
//!   `Nak` ([`gbc_link::DecodeError::nak_code`], with the frame's `seq`
//!   when the CRC vouches for it, else 0) is encoded in a buffer of its
//!   own, so the stored reply survives it: a resend corrupted on the way,
//!   after the first answer was lost, must not turn the next good
//!   resend's replay into that `Nak`. Nothing is remembered.
//! - [`Received::Request`]: it decoded. The caller may handle it without
//!   storing it (a message that is never answered from the store, such as
//!   the session's `Input`); otherwise it asks [`Request::resend`] first:
//!   a frame byte-identical to the last answered request (same `seq`,
//!   same content) is a resend and gets the stored reply back, not to be
//!   redone. `Hello` is never a resend (each PC session restarts `seq` at
//!   0). A fresh request is answered with [`Request::answer`], which
//!   encodes the reply, stores it and remembers the request.
//!
//! Counting is the caller's: every outcome is visible in the return
//! values (a `None` wire is a reply that did not fit [`REPLY_MAX`]).

use gbc_link::{FrameDecoder, MAX_ENCODED, Message};

/// The largest chunk the board takes (`HelloReply.max_chunk`), and the
/// size of the chunks it dumps: the protocol's maximum.
pub const MAX_CHUNK: u16 = gbc_link::MAX_CHUNK as u16;
const _: () = assert!(MAX_CHUNK as usize == gbc_link::MAX_CHUNK);

/// The largest reply body of either server: `HelloReply` (protocol u16,
/// max_chunk u16, flash_len u32) and the loader's `RomDone` (hash u64), 8
/// bytes; `Nak` has 1, `Ack` and `AlreadyLoaded` none (`tools/gbc-pc`'s
/// `tests/board.rs` and `tests/link_session.rs` encode each).
pub const REPLY_BODY_MAX: usize = 8;

/// One reply frame's buffer: at least [`REPLY_FRAME_MAX`], with room to
/// spare.
pub const REPLY_MAX: usize = 32;
/// The largest reply frame on the wire: type, seq, body and CRC,
/// COBS-encoded, plus the delimiter.
pub const REPLY_FRAME_MAX: usize = gbc_link::cobs::max_encoded_len(2 + REPLY_BODY_MAX + 4) + 1;
const _: () = assert!(REPLY_FRAME_MAX <= REPLY_MAX);

/// Encodes a reply into `out`: its length, or `None` if it does not fit.
pub fn encode_reply(msg: &Message<'_>, seq: u8, out: &mut [u8]) -> Option<usize> {
    gbc_link::encode(msg, seq, out).ok()
}

/// See the module docs.
pub struct Responder {
    dec: FrameDecoder,
    store: Store,
}

/// Everything but the decoder, so a decoded message (which borrows the
/// decoder) can be answered with `&mut` to it.
struct Store {
    /// The frame being received, as on the wire (no delimiter), to tell a
    /// resend from a new request.
    raw: [u8; MAX_ENCODED],
    raw_len: usize,
    /// The last answered request, as on the wire; length 0 = none.
    last_req: [u8; MAX_ENCODED],
    last_req_len: usize,
    /// The reply to `last_req`, replayed for a resend.
    reply: [u8; REPLY_MAX],
    reply_len: usize,
    /// The `Nak` for a frame that did not decode: kept apart from `reply`
    /// so the stored reply survives it.
    bad_reply: [u8; REPLY_MAX],
}

/// A frame that ended at [`Responder::feed`].
pub enum Received<'a> {
    /// It did not decode: send `nak` (`None`: it did not fit
    /// [`REPLY_MAX`], never expected).
    BadFrame { nak: Option<&'a [u8]> },
    /// It decoded.
    Request(Request<'a>),
}

/// A decoded frame. Dropping it without [`Request::answer`] stores
/// nothing (the last answered request and its reply stay).
pub struct Request<'a> {
    pub seq: u8,
    pub msg: Message<'a>,
    store: &'a mut Store,
    raw_len: usize,
}

impl Default for Responder {
    fn default() -> Self {
        Self::new()
    }
}

impl Responder {
    pub const fn new() -> Self {
        Self {
            dec: FrameDecoder::new(),
            store: Store {
                raw: [0; MAX_ENCODED],
                raw_len: 0,
                last_req: [0; MAX_ENCODED],
                last_req_len: 0,
                reply: [0; REPLY_MAX],
                reply_len: 0,
                bad_reply: [0; REPLY_MAX],
            },
        }
    }

    /// One received byte; the frame it ends, if any. A lone 0x00 (an
    /// empty frame) is ignored.
    pub fn feed(&mut self, b: u8) -> Option<Received<'_>> {
        let st = &mut self.store;
        if b != 0 {
            // Past MAX_ENCODED the decoder reports Oversize; the raw copy
            // is then never compared.
            if let Some(slot) = st.raw.get_mut(st.raw_len) {
                *slot = b;
                st.raw_len += 1;
            }
        }
        let decoded = self.dec.feed(b);
        if b == 0 && decoded.is_none() {
            st.raw_len = 0;
        }
        let decoded = decoded?;
        let raw_len = core::mem::take(&mut st.raw_len);
        Some(match decoded {
            Ok((seq, msg)) => Received::Request(Request {
                seq,
                msg,
                store: st,
                raw_len,
            }),
            Err(e) => {
                let nak = Message::Nak { code: e.nak_code() };
                let n = encode_reply(&nak, e.seq().unwrap_or(0), &mut st.bad_reply);
                Received::BadFrame {
                    nak: n.and_then(|n| st.bad_reply.get(..n)),
                }
            }
        })
    }
}

impl<'a> Request<'a> {
    /// `Ok(stored reply)` if this frame is byte-identical to the last
    /// answered request (never for `Hello`): send that, don't redo the
    /// work. Otherwise the request back, to be handled.
    pub fn resend(self) -> Result<&'a [u8], Self> {
        let st = &*self.store;
        let resent = !matches!(self.msg, Message::Hello { .. })
            && st.last_req_len != 0
            && st.last_req.get(..st.last_req_len) == st.raw.get(..self.raw_len);
        if resent {
            Ok(self.store.reply.get(..self.store.reply_len).unwrap_or(&[]))
        } else {
            Err(self)
        }
    }

    /// Answers with `reply`: encodes it (with the request's `seq`), stores
    /// it and remembers this request for [`Request::resend`]. The wire to
    /// send; `None` if it did not fit [`REPLY_MAX`] (never expected: the
    /// const assertion above; a resend then gets nothing either).
    pub fn answer(self, reply: &Message<'_>) -> Option<&'a [u8]> {
        let st = self.store;
        let n = encode_reply(reply, self.seq, &mut st.reply);
        st.reply_len = n.unwrap_or(0);
        if let (Some(dst), Some(src)) = (
            st.last_req.get_mut(..self.raw_len),
            st.raw.get(..self.raw_len),
        ) {
            dst.copy_from_slice(src);
            st.last_req_len = self.raw_len;
        }
        n.and_then(|n| st.reply.get(..n))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gbc_link::NakCode;

    extern crate std;
    use std::vec::Vec;

    fn wire(msg: &Message<'_>, seq: u8) -> Vec<u8> {
        let mut out = [0u8; gbc_link::MAX_FRAME];
        let n = gbc_link::encode(msg, seq, &mut out).unwrap();
        out[..n].to_vec()
    }

    /// What the responder did with one frame, owned.
    #[derive(Debug, PartialEq, Eq)]
    enum Outcome {
        Bad(Vec<u8>),
        Resend(Vec<u8>),
        Answered(Vec<u8>),
        Unstored,
        Nothing,
    }

    /// Feeds `frame`; answers a fresh request `Ack` (or leaves `Input`
    /// unanswered, as the session does).
    fn send(r: &mut Responder, frame: &[u8]) -> Outcome {
        let mut out = Outcome::Nothing;
        for &b in frame {
            let Some(rx) = r.feed(b) else { continue };
            out = match rx {
                Received::BadFrame { nak } => Outcome::Bad(nak.unwrap().to_vec()),
                Received::Request(req) if matches!(req.msg, Message::Input { .. }) => {
                    Outcome::Unstored
                }
                Received::Request(req) => match req.resend() {
                    Ok(w) => Outcome::Resend(w.to_vec()),
                    Err(req) => {
                        let seq = req.seq;
                        let w = req.answer(&Message::Ack).unwrap().to_vec();
                        assert_eq!(w, wire(&Message::Ack, seq));
                        Outcome::Answered(w)
                    }
                },
            };
        }
        out
    }

    const END: Message<'static> = Message::RomEnd;

    #[test]
    fn a_resend_gets_the_stored_reply() {
        let mut r = Responder::new();
        let f = wire(&END, 5);
        assert_eq!(send(&mut r, &f), Outcome::Answered(wire(&Message::Ack, 5)));
        assert_eq!(send(&mut r, &f), Outcome::Resend(wire(&Message::Ack, 5)));
        // Another seq is a new request.
        assert_eq!(
            send(&mut r, &wire(&END, 6)),
            Outcome::Answered(wire(&Message::Ack, 6))
        );
    }

    #[test]
    fn hello_is_never_a_resend() {
        let mut r = Responder::new();
        let f = wire(&Message::Hello { protocol: 1 }, 0);
        assert!(matches!(send(&mut r, &f), Outcome::Answered(_)));
        assert!(matches!(send(&mut r, &f), Outcome::Answered(_)));
    }

    #[test]
    fn a_bad_frame_is_naked_and_keeps_the_stored_reply() {
        let mut r = Responder::new();
        let f = wire(&END, 7);
        send(&mut r, &f);
        let mut bad = f.clone();
        bad[1] ^= 0x40; // fails the CRC
        let Outcome::Bad(nak) = send(&mut r, &bad) else {
            panic!("not a bad frame")
        };
        let mut dec = FrameDecoder::new();
        let (_, m) = dec.feed_slice(&nak).1.unwrap().unwrap();
        assert!(matches!(m, Message::Nak { code } if code != NakCode::UnexpectedType));
        // K1: the good resend still gets the stored reply, not the Nak.
        assert_eq!(send(&mut r, &f), Outcome::Resend(wire(&Message::Ack, 7)));
    }

    #[test]
    fn an_unanswered_request_stores_nothing() {
        let mut r = Responder::new();
        let f = wire(&END, 1);
        send(&mut r, &f);
        assert_eq!(
            send(&mut r, &wire(&Message::Input { buttons: 3 }, 1)),
            Outcome::Unstored
        );
        assert_eq!(send(&mut r, &f), Outcome::Resend(wire(&Message::Ack, 1)));
    }
}
