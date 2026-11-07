use std::time::Duration;

use str0m::format::Codec;
use str0m::media::{MediaKind, Pt};
use str0m::rtp::{ExtensionValues, RtpWrite, Ssrc, Vp8Descriptor};
use str0m::{Event, RtcError};

mod common;
use common::{connect_l_r, init_crypto_default, init_log, progress};

const VP8_PAYLOAD: [u8; 6] = [0x90, 0xf0, 0x01, 0x02, 0xa3, 0x00];
const VP8_REWRITTEN_PICTURE_ID_PAYLOAD: [u8; 6] = [0x90, 0xf0, 0x7e, 0x02, 0xa3, 0x00];

#[test]
pub fn rtp_direct_csrc_basic() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let (mut l, mut r) = connect_l_r();

    let mid = "aud".into();
    let ssrc: Ssrc = 1.into();

    l.direct_api().declare_media(mid, MediaKind::Audio);
    l.direct_api().declare_stream_tx(ssrc, None, mid, None);

    r.direct_api().declare_media(mid, MediaKind::Audio);
    r.direct_api().expect_stream_rx(ssrc, None, mid, None);

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    let params = l.params_opus();
    assert_eq!(params.spec().codec, Codec::Opus);
    let pt = params.pt();

    let csrc_values: [u32; 15] = [
        u32::MAX,
        0x12_34_56_78,
        1,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
    ];

    let wallclock = l.start + l.duration();
    let exts = ExtensionValues {
        audio_level: Some(-42),
        voice_activity: Some(false),
        ..Default::default()
    };

    l.direct_api().stream_tx(&ssrc).unwrap().write_rtp(
        RtpWrite::new(
            pt,
            47_000.into(),
            47_000_000,
            wallclock,
            [0x1, 0x2, 0x3, 0x4],
        )
        .ext_vals(exts)
        .csrc(&csrc_values[..3]),
    );

    loop {
        progress(&mut l, &mut r)?;

        let has_media_packet = r
            .events
            .iter()
            .any(|(_, e)| matches!(e, Event::RtpPacket(_)));

        if has_media_packet || l.duration() > Duration::from_secs(10) {
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

    assert_eq!(media.len(), 1);

    let h = &media[0].header;
    assert_eq!(h.csrc_count, 3);
    assert_eq!(h.csrc[0], u32::MAX);
    assert_eq!(h.csrc[1], 0x12_34_56_78);
    assert_eq!(h.csrc[2], 1);
    // Remaining slots should be zero
    for i in 3..15 {
        assert_eq!(h.csrc[i], 0, "csrc[{i}] should be 0");
    }

    Ok(())
}

#[test]
pub fn rtp_direct_csrc_max_entries() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let (mut l, mut r) = connect_l_r();

    let mid = "aud".into();
    let ssrc: Ssrc = 2.into();

    l.direct_api().declare_media(mid, MediaKind::Audio);
    l.direct_api().declare_stream_tx(ssrc, None, mid, None);

    r.direct_api().declare_media(mid, MediaKind::Audio);
    r.direct_api().expect_stream_rx(ssrc, None, mid, None);

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    let params = l.params_opus();
    let pt = params.pt();

    let csrc_values: [u32; 15] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

    let wallclock = l.start + l.duration();
    l.direct_api().stream_tx(&ssrc).unwrap().write_rtp(
        RtpWrite::new(
            pt,
            48_000.into(),
            48_000_000,
            wallclock,
            [0xa, 0xb, 0xc, 0xd],
        )
        .csrc(&csrc_values),
    );

    loop {
        progress(&mut l, &mut r)?;

        let has_media_packet = r
            .events
            .iter()
            .any(|(_, e)| matches!(e, Event::RtpPacket(_)));

        if has_media_packet || l.duration() > Duration::from_secs(10) {
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

    assert_eq!(media.len(), 1);

    let h = &media[0].header;
    assert_eq!(h.csrc_count, 15);
    for i in 0..15 {
        assert_eq!(h.csrc[i], (i + 1) as u32, "csrc[{i}] mismatch");
    }

    Ok(())
}

#[test]
#[should_panic(expected = "CSRC count must be <= 15")]
pub fn rtp_direct_csrc_panics_on_too_many_entries() {
    let csrc_values = [0; 16];
    RtpWrite::new(
        Pt::new_with_value(96),
        47_000.into(),
        47_000_000,
        std::time::Instant::now(),
        [0],
    )
    .csrc(&csrc_values);
}

#[test]
pub fn rtp_direct_csrc_with_vp8_patch() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let (mut l, mut r) = connect_l_r();

    let mid = "vid".into();
    let ssrc: Ssrc = 3.into();

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
    let csrc_values = [0x12_34_56_78, 7];
    let wallclock = l.start + l.duration();
    let vp8_patch = Vp8Descriptor::parse(&VP8_PAYLOAD)
        .expect("valid VP8 descriptor")
        .patch()
        .picture_id(0x7e)
        .build()
        .expect("valid VP8 patch");

    l.direct_api().stream_tx(&ssrc).unwrap().write_rtp(
        RtpWrite::new(pt, 49_000.into(), 49_000_000, wallclock, VP8_PAYLOAD)
            .csrc(&csrc_values)
            .vp8_patch(vp8_patch),
    );

    loop {
        progress(&mut l, &mut r)?;

        let has_media_packet = r
            .events
            .iter()
            .any(|(_, e)| matches!(e, Event::RtpPacket(_)));

        if has_media_packet || l.duration() > Duration::from_secs(10) {
            break;
        }
    }

    let mut media = r.events.iter().filter_map(|(_, e)| {
        if let Event::RtpPacket(v) = e {
            Some(v)
        } else {
            None
        }
    });

    let packet = media.next().expect("one RTP packet");
    assert!(media.next().is_none());

    assert_eq!(
        packet.payload.as_ref(),
        VP8_REWRITTEN_PICTURE_ID_PAYLOAD.as_slice()
    );
    assert_eq!(packet.header.csrc_count, 2);
    assert_eq!(packet.header.csrc[0], csrc_values[0]);
    assert_eq!(packet.header.csrc[1], csrc_values[1]);

    Ok(())
}
