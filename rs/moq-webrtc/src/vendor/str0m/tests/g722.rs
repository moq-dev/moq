//! G722 timestamp / clock-rate tests.
//!
//! G722 samples audio at 16 kHz but, per RFC 3551 §4.5.2, its RTP clock rate is
//! 8000 Hz (kept at 8000 to stay backwards compatible with RFC 1890, which
//! incorrectly used this value):
//! <https://en.wikipedia.org/wiki/RTP_payload_formats#cite_note-55>

use std::collections::VecDeque;
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use str0m::format::Codec;
use str0m::media::{Direction, Frequency, MediaKind, MediaTime};
use str0m::rtp::{RtpWrite, Ssrc};
use str0m::{Event, Rtc, RtcError};

mod common;
use common::{Peer, TestRtc, connect_l_r_with_rtc, init_crypto_default, init_log, progress};

/// G722 is a 16 kHz codec, but per RFC 3551 §4.5.2 its RTP timestamp clock runs at
/// 8000 Hz. In the media (samples/frame) API str0m presents G722 as 16 kHz both
/// when writing and when reading, and maps to/from the 8 kHz RTP clock on the
/// wire internally. See
/// <https://en.wikipedia.org/wiki/RTP_payload_formats#cite_note-55>
#[test]
pub fn g722_media_mode_is_16khz_both_ways() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new_with_config(Peer::Left, |c| c.clear_codecs().enable_g722(true, false));
    let mut r =
        TestRtc::new_with_config(Peer::Right, |c| c.clear_codecs().enable_g722(true, false));

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    let mut change = l.sdp_api();
    let mid = change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
    let (offer, pending) = change.apply().unwrap();

    let answer = r.rtc.sdp_api().accept_offer(offer)?;
    l.rtc.sdp_api().accept_answer(pending, answer)?;

    loop {
        if l.is_connected() || r.is_connected() {
            break;
        }
        progress(&mut l, &mut r)?;
    }

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    let params = l
        .rtc
        .codec_config()
        .find(|p| p.spec().codec == Codec::G722)
        .cloned()
        .expect("G722 to be negotiated");
    // The codec is considered 16 kHz in the media API.
    assert_eq!(params.spec().clock_rate, Frequency::SIXTEEN_KHZ);
    let pt = params.pt();

    // 20 ms of G722 at 64 kbit/s = 160 octets. Each 20 ms frame advances the
    // 16 kHz media time by 320 samples (and the 8 kHz RTP timestamp by 160).
    let data = vec![3_u8; 160];
    let mut samples: u64 = 0;

    loop {
        {
            let wallclock = l.start + l.duration();
            let time = MediaTime::new(samples, Frequency::SIXTEEN_KHZ);
            l.writer(mid)
                .unwrap()
                .write(pt, wallclock, time, data.clone())?;
        }
        samples += 320;

        progress(&mut l, &mut r)?;

        if l.duration() > Duration::from_secs(2) {
            break;
        }
    }

    let media: Vec<_> = r
        .events
        .iter()
        .filter_map(|(_, e)| match e {
            Event::MediaData(d) => Some(d),
            _ => None,
        })
        .collect();

    assert!(!media.is_empty(), "R received no G722 MediaData");

    for d in &media {
        // The media (samples/frame) API presents G722 as a 16 kHz codec ...
        assert_eq!(d.params.spec().clock_rate, Frequency::SIXTEEN_KHZ);
        // ... and the received media time is in that same 16 kHz clock rate.
        assert_eq!(
            d.time.frequency(),
            Frequency::SIXTEEN_KHZ,
            "media-mode G722 receive time should be 16 kHz"
        );
        // The 16 kHz media time advances in whole 20 ms frames (320 samples).
        assert_eq!(
            d.time.numer() % 320,
            0,
            "expected 16 kHz frame-aligned time"
        );
    }

    Ok(())
}

fn rtp_mode_g722(peer: Peer, now: Instant) -> Rtc {
    let mut b = Rtc::builder()
        .set_rtp_mode(true)
        .enable_raw_packets(true)
        .clear_codecs()
        .enable_g722(true, false);
    if let Some(crypto) = peer.crypto_provider() {
        b = b.set_crypto_provider(crypto);
    }
    b.build(now)
}

/// In RTP mode the user works directly with raw RTP timestamps, which for G722
/// are the 8 kHz wire values (RFC 3551 §4.5.2). str0m must not apply any 16 kHz
/// scaling in either direction. See
/// <https://en.wikipedia.org/wiki/RTP_payload_formats#cite_note-55>
#[test]
pub fn g722_rtp_mode_is_8khz_both_ways() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let now = Instant::now();
    let (mut l, mut r) = connect_l_r_with_rtc(
        rtp_mode_g722(Peer::Left, now),
        rtp_mode_g722(Peer::Right, now),
    );

    let mid = "aud".into();
    let ssrc: Ssrc = 1.into();

    l.direct_api().declare_media(mid, MediaKind::Audio);
    l.direct_api().declare_stream_tx(ssrc, None, mid, None);
    r.direct_api().declare_media(mid, MediaKind::Audio);
    r.direct_api().expect_stream_rx(ssrc, None, mid, None);

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    let pt = l
        .rtc
        .codec_config()
        .find(|p| p.spec().codec == Codec::G722)
        .map(|p| p.pt())
        .expect("G722 PT");

    // The user supplies the raw 8 kHz RTP timestamp; 20 ms frames advance it by 160.
    let base_ts: u32 = 8_000_000;
    let data: &[u8] = &[0x1, 0x2, 0x3, 0x4];

    let mut to_write: VecDeque<(u32, u64)> = VecDeque::from(vec![
        (base_ts, 100),
        (base_ts + 160, 101),
        (base_ts + 320, 102),
    ]);

    let mut write_at = l.last + Duration::from_millis(300);

    loop {
        if l.start + l.duration() > write_at {
            write_at = l.last + Duration::from_millis(300);
            if let Some((time, seq)) = to_write.pop_front() {
                let wallclock = l.start + l.duration();
                let mut direct = l.direct_api();
                let stream = direct.stream_tx(&ssrc).unwrap();
                stream.write_rtp(RtpWrite::new(pt, seq.into(), time, wallclock, data));
            }
        }

        progress(&mut l, &mut r)?;

        if l.duration() > Duration::from_secs(4) {
            break;
        }
    }

    let media: Vec<_> = r
        .events
        .iter()
        .filter_map(|(_, e)| match e {
            Event::RtpPacket(v) => Some(v),
            _ => None,
        })
        .collect();

    assert_eq!(media.len(), 3, "expected 3 RTP packets at R");

    // RTP mode passes the 8 kHz wire timestamp through verbatim (no 16 kHz scaling).
    assert_eq!(media[0].header.timestamp, base_ts);
    assert_eq!(media[1].header.timestamp, base_ts + 160);
    assert_eq!(media[2].header.timestamp, base_ts + 320);

    for v in &media {
        assert_eq!(
            v.time.frequency(),
            Frequency::EIGHT_KHZ,
            "RTP-mode G722 time should be at the 8 kHz RTP clock rate"
        );
    }

    Ok(())
}

/// Cross-mode: a sample (frame) mode sender writes 16 kHz media time, and an
/// RTP mode receiver sees the 8 kHz wire timestamps (RFC 3551 §4.5.2). This
/// verifies the 16 kHz -> 8 kHz mapping happens on the send side. See
/// <https://en.wikipedia.org/wiki/RTP_payload_formats#cite_note-55>
#[test]
pub fn g722_sample_send_to_rtp_receive() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let now = Instant::now();
    // L writes via the sample/frame API (16 kHz media time).
    let rtc_l = Rtc::builder()
        .clear_codecs()
        .enable_g722(true, false)
        .build(now);
    // R reads raw RTP packets (8 kHz wire clock).
    let rtc_r = Rtc::builder()
        .set_rtp_mode(true)
        .clear_codecs()
        .enable_g722(true, false)
        .build(now);

    let (mut l, mut r) = connect_l_r_with_rtc(rtc_l, rtc_r);

    let mid = "aud".into();
    let ssrc: Ssrc = 1.into();

    l.direct_api().declare_media(mid, MediaKind::Audio);
    l.direct_api().declare_stream_tx(ssrc, None, mid, None);
    r.direct_api().declare_media(mid, MediaKind::Audio);
    r.direct_api().expect_stream_rx(ssrc, None, mid, None);

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    let pt = l
        .rtc
        .codec_config()
        .find(|p| p.spec().codec == Codec::G722)
        .map(|p| p.pt())
        .expect("G722 PT");

    let data = vec![7u8; 160];
    // 5 frames of 20 ms; the 16 kHz media time advances 320 samples per frame.
    let mut frames: VecDeque<u64> = VecDeque::from(vec![0, 320, 640, 960, 1280]);
    let mut write_at = l.last + Duration::from_millis(300);

    loop {
        if l.start + l.duration() > write_at {
            write_at = l.last + Duration::from_millis(300);
            if let Some(samples) = frames.pop_front() {
                let wallclock = l.start + l.duration();
                let time = MediaTime::new(samples, Frequency::SIXTEEN_KHZ);
                l.writer(mid)
                    .unwrap()
                    .write(pt, wallclock, time, data.clone())?;
            }
        }

        progress(&mut l, &mut r)?;

        if l.duration() > Duration::from_secs(4) {
            break;
        }
    }

    let media: Vec<_> = r
        .events
        .iter()
        .filter_map(|(_, e)| match e {
            Event::RtpPacket(v) => Some(v),
            _ => None,
        })
        .collect();

    assert_eq!(media.len(), 5, "expected 5 RTP packets at R");

    // The RTP receiver sees the 8 kHz wire clock; each 20 ms frame advances 160.
    let base = media[0].header.timestamp;
    for (i, v) in media.iter().enumerate() {
        assert_eq!(
            v.time.frequency(),
            Frequency::EIGHT_KHZ,
            "RTP-mode receive time should be 8 kHz"
        );
        assert_eq!(
            v.header.timestamp,
            base + (i as u32) * 160,
            "8 kHz wire timestamp mismatch at frame {i}"
        );
    }

    Ok(())
}

/// Cross-mode: an RTP mode sender writes raw 8 kHz wire timestamps, and a sample
/// (frame) mode receiver sees 16 kHz media time. This verifies the 8 kHz -> 16 kHz
/// mapping happens on the receive side. See
/// <https://en.wikipedia.org/wiki/RTP_payload_formats#cite_note-55>
#[test]
pub fn g722_rtp_send_to_sample_receive() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let now = Instant::now();
    // L writes raw RTP at the 8 kHz wire clock.
    let rtc_l = Rtc::builder()
        .set_rtp_mode(true)
        .clear_codecs()
        .enable_g722(true, false)
        .build(now);
    // R reads frames via the sample API (16 kHz media time), no reorder hold-back.
    let rtc_r = Rtc::builder()
        .set_reordering_size_audio(0)
        .clear_codecs()
        .enable_g722(true, false)
        .build(now);

    let (mut l, mut r) = connect_l_r_with_rtc(rtc_l, rtc_r);

    let mid = "aud".into();
    let ssrc_tx: Ssrc = 1.into();

    l.direct_api().declare_media(mid, MediaKind::Audio);
    l.direct_api().declare_stream_tx(ssrc_tx, None, mid, None);
    r.direct_api().declare_media(mid, MediaKind::Audio);

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    let pt = l
        .rtc
        .codec_config()
        .find(|p| p.spec().codec == Codec::G722)
        .map(|p| p.pt())
        .expect("G722 PT");
    let ssrc = l.direct_api().stream_tx_by_mid(mid, None).unwrap().ssrc();

    let data: &[u8] = &[0x1, 0x2, 0x3, 0x4];
    let base_ts: u32 = 8_000_000;
    // 5 frames of 20 ms; the 8 kHz wire timestamp advances 160 per frame.
    let mut frames: VecDeque<(u32, u64)> = VecDeque::from(vec![
        (base_ts, 100),
        (base_ts + 160, 101),
        (base_ts + 320, 102),
        (base_ts + 480, 103),
        (base_ts + 640, 104),
    ]);
    let mut write_at = l.last + Duration::from_millis(300);

    loop {
        if l.start + l.duration() > write_at {
            write_at = l.last + Duration::from_millis(300);
            if let Some((ts, seq)) = frames.pop_front() {
                let wallclock = l.start + l.duration();
                let mut direct = l.direct_api();
                let stream = direct.stream_tx(&ssrc).unwrap();
                stream.write_rtp(RtpWrite::new(pt, seq.into(), ts, wallclock, data));
            }
        }

        progress(&mut l, &mut r)?;

        if l.duration() > Duration::from_secs(4) {
            break;
        }
    }

    let media: Vec<_> = r
        .events
        .iter()
        .filter_map(|(_, e)| match e {
            Event::MediaData(v) => Some(v),
            _ => None,
        })
        .collect();

    assert_eq!(media.len(), 5, "expected 5 frames at R");

    // The sample receiver presents G722 as 16 kHz; each 20 ms frame advances 320.
    let base = media[0].time.numer();
    for (i, m) in media.iter().enumerate() {
        assert_eq!(m.params.spec().clock_rate, Frequency::SIXTEEN_KHZ);
        assert_eq!(
            m.time.frequency(),
            Frequency::SIXTEEN_KHZ,
            "sample-mode receive time should be 16 kHz"
        );
        assert_eq!(
            m.time.numer(),
            base + (i as u64) * 320,
            "16 kHz media time mismatch at frame {i}"
        );
    }

    Ok(())
}
