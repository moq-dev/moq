//! Exported fuzz targets to get them part of the compilation with feature `_internal_test_exports`.

use std::collections::VecDeque;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use crate::change::{SdpAnswer, SdpOffer};
use crate::format::Codec;
use crate::net::{Protocol, Receive};
use crate::packet::{CodecDepacketizer, CodecExtra, Depacketizer, DepacketizingBuffer, RtpMeta};
use crate::rtp_::{Frequency, MediaTime, Rtcp, RtcpFb, RtpHeader};
use crate::streams::register::ReceiverRegister;
use crate::streams::rtx_cache_buf::EvictingBuffer;
use crate::{Input, Output, Rtc};

use super::Rng;
#[allow(unused)]
use super::setup::{random_config, random_extmap};

pub fn rtx_buffer(data: &[u8]) {
    if data.len() < 4 {
        return;
    }

    let buf_size = u16::from_be_bytes([data[0], data[1]]);
    let max_age = data[2] as u64;
    let max_size = data[3] as usize;
    let mut buf = EvictingBuffer::new(buf_size as usize, Duration::from_secs(max_age), max_size);
    let mut now = Instant::now();
    let mut pos = 0;

    for d in &data[4..] {
        now += Duration::from_millis(*d as u64);
        if d % 2 == 0 {
            buf.maybe_evict(now)
        } else {
            pos += *d as u64;
            buf.push(pos, now, d);
        }
    }
}

pub fn rtp_header(data: &[u8]) -> Option<()> {
    let mut rng = Rng::new(data);
    let exts = random_extmap(&mut rng, 10)?;
    let len = rng.usize(76)?;
    RtpHeader::_parse(rng.slice(len)?, &exts);
    Some(())
}

pub fn rtp_packet(data: &[u8]) -> Option<()> {
    let mut rng = Rng::new(data);
    let exts = random_extmap(&mut rng, 10)?;
    // Maximum RTP packet size is typically around 1500 bytes (MTU)
    let packet_len = rng.usize(1500)?;
    let packet = rng.slice(packet_len)?;

    // Parse the header from the packet
    let header = RtpHeader::_parse(packet, &exts)?;

    // Extract the payload (data after the header)
    let _payload = packet.get(header.header_len..)?;

    Some(())
}

pub fn sdp_offer(data: &[u8]) -> Option<()> {
    let str = std::str::from_utf8(data).ok()?;
    let _ = SdpOffer::from_sdp_string(str);
    Some(())
}

pub fn sdp_answer(data: &[u8]) -> Option<()> {
    let str = std::str::from_utf8(data).ok()?;
    let _ = SdpAnswer::from_sdp_string(str);
    Some(())
}

pub fn depack(data: &[u8]) -> Option<()> {
    let mut rng = Rng::new(data);

    let codec = match rng.u8(11)? {
        0 => Codec::Opus,
        1 => Codec::Vp8,
        2 => Codec::Vp9,
        3 => Codec::H264,
        4 => Codec::H265,
        5 => Codec::H266,
        6 => Codec::Av1,
        7 => Codec::PCMU,
        8 => Codec::PCMA,
        9 => Codec::G722,
        10 => Codec::CN,
        11 => Codec::AmrWb,
        _ => unreachable!(),
    };

    let mut cd: CodecDepacketizer = codec.into();
    // AMR-WB has two on-the-wire layouts (RFC 4867); fuzz both.
    if let CodecDepacketizer::AmrWb(ref mut amr) = cd {
        amr.octet_align = rng.bool()?;
    }

    let mut depack = DepacketizingBuffer::new(cd, rng.usize(300)?);

    let exts = random_extmap(&mut rng, 10)?;

    let start = Instant::now();

    loop {
        let do_push = rng.bool()?;

        if do_push {
            let hlen = rng.usize(76)?;
            let header = RtpHeader::_parse(rng.slice(hlen)?, &exts)?;
            let meta = RtpMeta {
                received: start + Duration::from_millis(rng.u64(10000)?),
                time: MediaTime::new(rng.u64(u64::MAX)?, Frequency::MICROS),
                seq_no: rng.u64(u64::MAX)?.into(),
                header,
                last_sender_info: None,
            };
            let len = rng.usize(1200)?;
            let data = rng.slice(len)?.to_vec();
            depack.push(meta, data);
        } else {
            depack.pop(start, None);
        }
    }
}

pub fn receive_register(data: &[u8]) -> Option<()> {
    let mut rng = Rng::new(data);
    let mut rr = ReceiverRegister::new(None);
    let start = Instant::now();
    loop {
        match rng.u8(2)? {
            0 => {
                let seq = rng.u64(u64::MAX / 2)?;
                let arrival = start + Duration::from_micros(rng.u64(u64::MAX / 100)?);
                let rtp_time = rng.u32(u32::MAX / 2)?;
                let clock_rate = rng.u32(u32::MAX / 2)?;
                rr.update(seq.into(), arrival, rtp_time, clock_rate);
            }
            1 => {
                let now = start + Duration::from_micros(rng.u64(u64::MAX / 100)?);
                let rtt = Duration::from_millis(rng.u64(1000)?);
                rr.nack_report(now, Some(rtt));
            }
            2 => {
                rr.reception_report();
            }
            _ => unreachable!(),
        }
    }
}

/// Fuzz the full `Rtc` receive path: instance init, datagram demux
/// (STUN/DTLS/RTP/RTCP), dispatch, and `poll_output` draining.
pub fn receive(data: &[u8]) -> Option<()> {
    let mut rng = Rng::new(data);

    let base = Instant::now();
    let mut rtc = Rtc::new(base);

    let source = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)), 5000);
    let destination = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(2, 2, 2, 2)), 5001);

    let mut millis: u64 = 0;

    loop {
        // Drive time monotonically forward.
        millis += rng.u64(50)?;
        let now = base + Duration::from_millis(millis);

        if rng.bool()? {
            // Feed a datagram through the full demux + dispatch path.
            let len = rng.usize(2000)?;
            let buf = rng.slice(len)?;
            if let Ok(receive) = Receive::new(Protocol::Udp, source, destination, buf) {
                let _ = rtc.handle_input(Input::Receive(now, receive));
            }
        } else {
            let _ = rtc.handle_input(Input::Timeout(now));
        }

        // Drain outputs (bounded to avoid spinning on a single input).
        for _ in 0..1000 {
            match rtc.poll_output() {
                Ok(Output::Timeout(_)) => break,
                Ok(_) => continue,
                Err(_) => break,
            }
        }

        if !rtc.is_alive() {
            return Some(());
        }
    }
}

/// Fuzz STUN message parsing (the ICE input path).
pub fn stun(data: &[u8]) -> Option<()> {
    let _ = is::stun::StunMessage::parse(data);
    Some(())
}

/// Fuzz RTCP compound-packet parsing and the feedback conversion.
pub fn rtcp(data: &[u8]) -> Option<()> {
    let mut feedback = VecDeque::new();
    Rtcp::read_packet(data, &mut feedback);
    // Exercise the feedback conversion as well.
    let _ = RtcpFb::from_rtcp(feedback.drain(..)).count();
    Some(())
}

/// Fuzz each codec depacketizer directly (bypassing the buffer's gating),
/// exercising the raw `Depacketizer::depacketize` parsers.
pub fn depack_direct(data: &[u8]) -> Option<()> {
    let mut rng = Rng::new(data);

    let codec = match rng.u8(12)? {
        0 => Codec::Opus,
        1 => Codec::Vp8,
        2 => Codec::Vp9,
        3 => Codec::H264,
        4 => Codec::H265,
        5 => Codec::H266,
        6 => Codec::Av1,
        7 => Codec::PCMU,
        8 => Codec::PCMA,
        9 => Codec::G722,
        10 => Codec::CN,
        11 => Codec::Tele,
        12 => Codec::AmrWb,
        _ => unreachable!(),
    };

    let mut depack: CodecDepacketizer = codec.into();
    // AMR-WB has two on-the-wire layouts (RFC 4867); fuzz both.
    if let CodecDepacketizer::AmrWb(ref mut amr) = depack {
        amr.octet_align = rng.bool()?;
    }
    let mut out = Vec::new();
    let mut extra = CodecExtra::None;

    loop {
        let marker = rng.bool()?;
        let len = rng.usize(1500)?;
        let packet = rng.slice(len)?;

        let _ = depack.depacketize(packet, &mut out, &mut extra);
        let _ = depack.is_partition_head(packet);
        let _ = depack.is_partition_tail(marker, packet);

        // Keep memory bounded across iterations.
        if out.len() > 4_000_000 {
            out.clear();
        }
    }
}
