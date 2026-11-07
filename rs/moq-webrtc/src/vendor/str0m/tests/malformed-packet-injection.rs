//! Regression tests for panics reachable from packets a peer (or, for the SRTP/SRTCP
//! cases, anyone who can send a datagram to the socket) puts on the wire.
//!
//! str0m's policy is that a panic means a bug in str0m, never bad input. Every test
//! here feeds input through a public API and asserts we survive it.

use std::net::{Ipv4Addr, SocketAddr};
use std::panic::AssertUnwindSafe;
use std::time::Instant;

use str0m::media::MediaKind;
use str0m::net::{Protocol, Receive};
use str0m::rtp::Ssrc;
use str0m::rtp::rtcp::{Rtcp, Twcc};
use str0m::{Input, RtcError};

mod common;
use common::{TestRtc, connect_l_r, init_crypto_default, init_log};

const L_ADDR: SocketAddr = SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)), 1000);
const R_ADDR: SocketAddr = SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::new(2, 2, 2, 2)), 2000);

/// Feed one raw datagram straight into `Rtc::handle_input`, exactly as a socket read would.
///
/// Returns `None` when the demuxer rejects the datagram outright (not our concern here).
fn inject(rtc: &mut TestRtc, bytes: &[u8]) -> Option<Result<(), RtcError>> {
    let now = rtc.last;
    let contents = bytes.try_into().ok()?;

    let input = Input::Receive(
        now,
        Receive {
            proto: Protocol::Udp,
            source: L_ADDR,
            destination: R_ADDR,
            contents,
        },
    );

    Some(rtc.rtc.handle_input(input))
}

/// Run `f` and report a panic as `Err(message)` rather than unwinding out of the test,
/// so a sweep can report *every* bad length instead of stopping at the first.
fn catch(f: impl FnOnce()) -> Result<(), String> {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let res = std::panic::catch_unwind(AssertUnwindSafe(f));
    std::panic::set_hook(prev);

    res.map_err(|e| {
        e.downcast_ref::<String>()
            .cloned()
            .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_else(|| "<non-string panic>".to_string())
    })
}

/// Build a bare RTCP-shaped datagram of exactly `len` bytes.
///
/// byte0 = 0x80 and byte1 = 201 (Receiver Report) is what `MultiplexKind` uses to route
/// a datagram to the SRTCP path, so this is all it takes to reach `unprotect_rtcp`.
fn rtcp_shaped(len: usize) -> Vec<u8> {
    let mut v = vec![0u8; len];
    if len > 0 {
        v[0] = 0x80;
    }
    if len > 1 {
        v[1] = 201;
    }
    v
}

/// Build an RTP-shaped datagram: 12-byte minimal header (no CSRC, no extension)
/// plus `payload_len` bytes of payload.
fn rtp_shaped(pt: u8, ssrc: Ssrc, seq: u16, payload_len: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(12 + payload_len);
    v.push(0x80); // version 2, no padding, no extension, csrc count 0
    v.push(pt); // marker 0
    v.extend_from_slice(&seq.to_be_bytes());
    v.extend_from_slice(&0u32.to_be_bytes()); // timestamp
    v.extend_from_slice(&(*ssrc).to_be_bytes());
    v.resize(12 + payload_len, 0);
    v
}

/// A datagram that demuxes as SRTCP but is too short to hold the SRTCP header,
/// the SRTCP index and the AEAD tag must be dropped, not panicked on.
///
/// The guard in `SrtpContext::unprotect_rtcp` only requires `SRTCP_INDEX_LEN + TAG_LEN`
/// (20 bytes), but the code then builds `vec![0; buf.len() - TAG_LEN - SRTCP_INDEX_LEN]`
/// and immediately writes 8 bytes into it. For 20..28 bytes that vec is shorter than 8.
///
/// This is reachable *before any authentication*: `Rtc::do_handle_receive` routes RTCP
/// straight to the session with no source or ICE check, and `unprotect_rtcp` is the
/// first code to touch the bytes.
#[test]
fn srtcp_shorter_than_srtcp_overhead() {
    init_log();
    init_crypto_default();

    let (_l, mut r) = connect_l_r();

    let mut panics = Vec::new();

    for len in 3..=64usize {
        let packet = rtcp_shaped(len);

        if let Err(msg) = catch(|| {
            // Ignore the Result, only the absence of a panic matters.
            let _ = inject(&mut r, &packet);
        }) {
            panics.push((len, msg));
        }
    }

    assert!(
        panics.is_empty(),
        "short SRTCP datagrams panicked at these lengths: {:#?}",
        panics
    );
}

/// An RTP packet whose payload is shorter than the AEAD tag must be dropped.
///
/// `unprotect_rtp` guards on `buf.len() < TAG_LEN`, but then computes
/// `input.len() - TAG_LEN` where `input` is everything *after* the RTP header. A 12-byte
/// header plus a 4..15 byte payload underflows: a subtract-overflow panic in debug, and in
/// release a wrapped `out_len` that makes `rx_scratch.resize()` abort.
///
/// Also pre-authentication, since for the GCM profiles the decrypt *is* the authentication.
#[test]
fn srtp_payload_shorter_than_aead_tag() {
    init_log();
    init_crypto_default();

    let (_l, mut r) = connect_l_r();

    let mid = "aud".into();
    let ssrc: Ssrc = 42.into();
    r.direct_api().declare_media(mid, MediaKind::Audio);
    r.direct_api().expect_stream_rx(ssrc, None, mid, None);

    let pt = *r.params_opus().pt();

    let mut panics = Vec::new();

    for payload_len in 0..=32usize {
        // A fresh seq each time so the replay/dupe filter doesn't short-circuit us.
        let packet = rtp_shaped(pt, ssrc, payload_len as u16, payload_len);

        if let Err(msg) = catch(|| {
            let _ = inject(&mut r, &packet);
        }) {
            panics.push((payload_len, msg));
        }
    }

    assert!(
        panics.is_empty(),
        "short RTP payloads panicked at these payload lengths: {:#?}",
        panics
    );
}

/// REMB carries an attacker-controlled SSRC count in a single byte, and the parser
/// indexes `buf[16 + i * 4 ..]` for each of them without checking the buffer holds them.
///
/// Reachable by an SRTCP-authenticated peer, i.e. the remote side of any call.
#[test]
fn remb_ssrc_count_beyond_buffer() {
    init_log();

    // RTCP header: version 2, fmt 15 (application layer), PT 206 (payload specific feedback).
    let mut buf = vec![0x80 | 15, 206, 0, 4];
    buf.extend_from_slice(&1u32.to_be_bytes()); // sender ssrc
    buf.extend_from_slice(&0u32.to_be_bytes()); // media ssrc, must be zero
    buf.extend_from_slice(b"REMB"); // unique identifier
    buf.push(255); // ssrc count: claims 255 trailing SSRCs...
    buf.extend_from_slice(&[0x1a, 0x20, 0xdf]); // exp + mantissa

    assert_eq!(buf.len(), 20, "...but the packet ends right here");

    let res = catch(|| {
        // Must be an error, never a panic.
        let _ = Rtcp::try_from(&buf[..]);
    });

    assert!(
        res.is_ok(),
        "parsing REMB with an oversized ssrc count panicked: {}",
        res.unwrap_err()
    );

    // It must not come out as a REMB. (Falling through to the generic
    // application-specific feedback parser is fine.)
    assert!(
        !matches!(Rtcp::try_from(&buf[..]), Ok(Rtcp::Remb(_))),
        "a REMB claiming 255 SSRCs in a 20 byte packet must be rejected"
    );
}

/// A TWCC run-length vector chunk encodes each packet status in 2 bits, so `0b11` is
/// perfectly representable on the wire. The parser accepts it (mapping it to
/// `PacketStatus::Unknown` and consuming no delta), but `TwccIter::next` has no arm for
/// `Unknown` and falls through to `unreachable!()`.
///
/// Reachable by an SRTCP-authenticated peer.
#[test]
fn twcc_vector_double_with_unknown_symbol() {
    init_log();

    // RTCP header: version 2, fmt 15 (transport wide), PT 205 (transport layer feedback).
    let mut buf = vec![0x80 | 15, 205, 0, 5];
    buf.extend_from_slice(&1u32.to_be_bytes()); // sender ssrc
    buf.extend_from_slice(&2u32.to_be_bytes()); // media ssrc
    buf.extend_from_slice(&0u16.to_be_bytes()); // base seq
    buf.extend_from_slice(&7u16.to_be_bytes()); // status count: one full VectorDouble
    buf.extend_from_slice(&[0, 0, 0]); // reference time (24 bit)
    buf.push(0); // feedback count

    // 0b11 in the top bits marks a "vector, 2-bit symbols" chunk. The first symbol is
    // 0b11, the remaining six are 0b00 (not received).
    buf.extend_from_slice(&0xF000u16.to_be_bytes());

    let Ok(Rtcp::Twcc(twcc)) = Rtcp::try_from(&buf[..]) else {
        panic!("a well-formed TWCC chunk should parse");
    };
    let _: Twcc = twcc.clone();

    let res = catch(move || {
        let count = twcc.into_iter(Instant::now(), 0.into()).count();
        // Reaching here at all is the point.
        let _ = count;
    });

    assert!(
        res.is_ok(),
        "iterating a TWCC report with a 0b11 vector symbol panicked: {}",
        res.unwrap_err()
    );
}
