use std::collections::VecDeque;
use std::time::{Duration, Instant};

use netem::{NetemConfig, Probability, RandomLoss};
use str0m::format::Codec;
use str0m::media::MediaKind;
use str0m::media::Pt;
use str0m::rtp::{ExtensionValues, RawPacket, RtpWrite, Ssrc, Vp8Descriptor, Vp8Patch};
use str0m::{Event, RtcError};

mod common;
use common::{connect_l_r, init_crypto_default, init_log, progress};

const VP8_PAYLOAD: [u8; 6] = [0x90, 0xf0, 0x01, 0x02, 0xa3, 0x00];
const VP8_REWRITTEN_PAYLOAD: [u8; 6] = [0x90, 0xf0, 0x7e, 0x44, 0xbf, 0x00];

#[test]
pub fn rtp_direct_ssrc() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let (mut l, mut r) = connect_l_r();

    let mid = "aud".into();

    // In this example we are not using RID to identify the stream, we are simply
    // using SSRC 1 as knowledge shared between sending and receiving side.
    let ssrc: Ssrc = 1.into();

    l.direct_api().declare_media(mid, MediaKind::Audio);

    l.direct_api().declare_stream_tx(ssrc, None, mid, None);

    r.direct_api().declare_media(mid, MediaKind::Audio);

    r.direct_api().expect_stream_rx(ssrc, None, mid, None);

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    let params = l.params_opus();
    let ssrc = l.direct_api().stream_tx_by_mid(mid, None).unwrap().ssrc();
    assert_eq!(params.spec().codec, Codec::Opus);
    let pt = params.pt();

    let to_write: Vec<&[u8]> = vec![
        // 1
        &[0x1, 0x2, 0x3, 0x4],
        // 3
        &[0x9, 0xa, 0xb, 0xc],
        // 2
        &[0x5, 0x6, 0x7, 0x8],
    ];

    let mut to_write: VecDeque<_> = to_write.into();

    let mut write_at = l.last + Duration::from_millis(300);

    let mut counts: Vec<u64> = vec![0, 3, 1];

    loop {
        if l.start + l.duration() > write_at {
            write_at = l.last + Duration::from_millis(300);
            if let Some(packet) = to_write.pop_front() {
                let wallclock = l.start + l.duration();

                let mut direct = l.direct_api();
                let stream = direct.stream_tx(&ssrc).unwrap();

                let count = counts.remove(0);
                let time = (count * 1000 + 47_000_000) as u32;
                let seq_no = (47_000 + count).into();

                let exts = ExtensionValues {
                    audio_level: Some(-42 - count as i8),
                    voice_activity: Some(false),
                    ..Default::default()
                };

                stream.write_rtp(RtpWrite::new(pt, seq_no, time, wallclock, packet).ext_vals(exts));
            }
        }

        progress(&mut l, &mut r)?;

        if l.duration() > Duration::from_secs(10) {
            break;
        }
    }

    let media: Vec<_> = r
        .events
        .iter()
        .filter_map(|(_, e)| {
            if let Event::RtpPacket(v) = e {
                Some(v)
            } else {
                None
            }
        })
        .collect();

    assert_eq!(media.len(), 3);

    let h0 = media[0].header.clone();
    let h1 = media[1].header.clone();
    let h2 = media[2].header.clone();

    assert_eq!(h0.sequence_number, 47000);
    assert_eq!(h1.sequence_number, 47003);
    assert_eq!(h2.sequence_number, 47001);

    assert_eq!(h0.timestamp, 47_000_000);
    assert_eq!(h1.timestamp, 47_003_000);
    assert_eq!(h2.timestamp, 47_001_000);

    assert_eq!(h0.ext_vals.audio_level, Some(-42));
    assert_eq!(h1.ext_vals.audio_level, Some(-45));
    assert_eq!(h2.ext_vals.audio_level, Some(-43));

    assert!(!h0.marker);
    assert!(!h1.marker);
    assert!(!h2.marker);

    assert!(l.media(mid).is_some());
    assert!(l.direct_api().stream_tx_by_mid(mid, None).is_some());
    l.direct_api().remove_media(mid);
    assert!(l.media(mid).is_none());
    assert!(l.direct_api().stream_tx_by_mid(mid, None).is_none());

    assert!(r.media(mid).is_some());
    assert!(r.direct_api().stream_rx_by_mid(mid, None).is_some());
    r.direct_api().remove_media(mid);
    assert!(r.media(mid).is_none());
    assert!(r.direct_api().stream_rx_by_mid(mid, None).is_none());

    Ok(())
}

#[test]
pub fn rtp_direct_vp8_patch() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let (mut l, mut r) = connect_l_r();

    let mid = "vid".into();
    let ssrc: Ssrc = 1.into();

    l.direct_api().declare_media(mid, MediaKind::Video);
    l.direct_api().declare_stream_tx(ssrc, None, mid, None);

    r.direct_api().declare_media(mid, MediaKind::Video);
    r.direct_api().expect_stream_rx(ssrc, None, mid, None);

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    let params = l.params_vp8();
    assert_eq!(params.spec().codec, Codec::Vp8);
    let pt = params.pt();
    let wallclock = l.start + l.duration();

    {
        let mut direct = l.direct_api();
        let stream = direct.stream_tx(&ssrc).unwrap();

        stream.write_rtp(
            RtpWrite::new(pt, 47_000.into(), 47_000_000, wallclock, VP8_PAYLOAD)
                .vp8_patch(vp8_patch()),
        );

        stream.write_rtp(RtpWrite::new(
            pt,
            47_001.into(),
            47_001_000,
            wallclock,
            [20, 21, 22],
        ));
    }

    loop {
        progress(&mut l, &mut r)?;

        let has_two_media_packets = r
            .events
            .iter()
            .filter(|(_, e)| matches!(e, Event::RtpPacket(_)))
            .take(2)
            .count()
            == 2;

        if has_two_media_packets || l.duration() > Duration::from_secs(10) {
            break;
        }
    }

    let mut media: Vec<_> = r
        .events
        .iter()
        .filter_map(|(_, e)| {
            if let Event::RtpPacket(v) = e {
                Some(v)
            } else {
                None
            }
        })
        .collect();

    media.sort_by_key(|packet| packet.header.sequence_number);

    assert_eq!(media.len(), 2);
    assert_eq!(media[0].header.sequence_number, 47_000);
    assert_eq!(media[0].payload.as_ref(), VP8_REWRITTEN_PAYLOAD.as_slice());
    assert_eq!(media[1].header.sequence_number, 47_001);
    assert_eq!(media[1].payload.as_ref(), &[20, 21, 22]);

    Ok(())
}

#[test]
pub fn rtp_direct_vp8_patch_survives_rtx() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let (mut l, mut r) = connect_l_r();

    let mid = "vid".into();
    let ssrc: Ssrc = 42.into();
    let ssrc_rtx: Ssrc = 44.into();

    l.direct_api().declare_media(mid, MediaKind::Video);
    l.direct_api()
        .declare_stream_tx(ssrc, Some(ssrc_rtx), mid, None)
        .set_rtx_cache(32, Duration::from_secs(3), None);

    r.direct_api().declare_media(mid, MediaKind::Video);
    r.direct_api()
        .expect_stream_rx(ssrc, Some(ssrc_rtx), mid, None);

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    let params = l.params_vp8();
    assert_eq!(params.spec().codec, Codec::Vp8);
    let pt = params.pt();
    let rtx_pt = params.resend().unwrap();
    let rewritten_seq = 47_002_u64;
    let original_seq_bytes = (rewritten_seq as u16).to_be_bytes();

    for index in 0_u64..=1 {
        let wallclock = l.start + l.duration();
        l.direct_api().stream_tx(&ssrc).unwrap().write_rtp(
            RtpWrite::new(
                pt,
                (47_000 + index).into(),
                47_000_000 + index as u32 * 1_000,
                wallclock,
                [index as u8],
            )
            .nackable(true),
        );

        progress(&mut l, &mut r)?;
    }

    r.set_netem(NetemConfig::new().loss(RandomLoss::new(Probability::ONE)));

    let wallclock = l.start + l.duration();
    l.direct_api().stream_tx(&ssrc).unwrap().write_rtp(
        RtpWrite::new(pt, rewritten_seq.into(), 47_002_000, wallclock, VP8_PAYLOAD)
            .nackable(true)
            .vp8_patch(vp8_patch()),
    );

    progress(&mut l, &mut r)?;
    r.set_netem(NetemConfig::new());

    for index in 3_u64..=7 {
        let wallclock = l.start + l.duration();
        l.direct_api().stream_tx(&ssrc).unwrap().write_rtp(
            RtpWrite::new(
                pt,
                (47_000 + index).into(),
                47_000_000 + index as u32 * 1_000,
                wallclock,
                [index as u8],
            )
            .nackable(true),
        );

        progress(&mut l, &mut r)?;
    }

    let rtx_payload = loop {
        progress(&mut l, &mut r)?;

        if let Some(payload) = rtx_payload_for_seq(&r.events, rtx_pt, &original_seq_bytes) {
            break payload;
        }

        if l.duration() > Duration::from_secs(10) {
            panic!("rewritten packet should be retransmitted over RTX");
        }
    };

    assert_eq!(rtx_payload.get(2..), Some(VP8_REWRITTEN_PAYLOAD.as_slice()));

    let recovered_packet = r.events.iter().find_map(|(_, event)| match event {
        Event::RtpPacket(packet) if packet.header.sequence_number == rewritten_seq as u16 => {
            Some(packet)
        }
        _ => None,
    });

    assert_eq!(
        recovered_packet.map(|packet| packet.payload.as_ref()),
        Some(VP8_REWRITTEN_PAYLOAD.as_slice())
    );

    Ok(())
}

fn vp8_patch() -> Vp8Patch {
    Vp8Descriptor::parse(&VP8_PAYLOAD)
        .expect("valid VP8 descriptor")
        .patch()
        .picture_id(0x7e)
        .tl0_pic_idx(0x44)
        .key_idx(0x1f)
        .build()
        .expect("valid VP8 patch")
}

fn rtx_payload_for_seq<'a>(
    events: &'a [(Instant, Event)],
    rtx_pt: Pt,
    original_seq_bytes: &[u8; 2],
) -> Option<&'a [u8]> {
    events
        .iter()
        .find_map(|(_, event)| match event.as_raw_packet() {
            Some(RawPacket::RtpRx(header, payload))
                if header.payload_type == rtx_pt && payload.starts_with(original_seq_bytes) =>
            {
                Some(payload.as_slice())
            }
            _ => None,
        })
}
