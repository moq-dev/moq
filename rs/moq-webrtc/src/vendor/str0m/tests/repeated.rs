use std::time::Duration;

use str0m::format::Codec;
use str0m::media::MediaKind;
use str0m::rtp::{ExtensionValues, RtpWrite, Ssrc};
use str0m::{Event, RtcError};

mod common;
use common::{connect_l_r, init_crypto_default, init_log, progress};

#[test]
pub fn repeated() -> Result<(), RtcError> {
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

    let mut write_at = l.last + Duration::from_millis(300);

    // Repeat the 3 a bunch of times.
    let mut counts: Vec<u64> = vec![0, 1, 2, 3, 2, 3, 3];

    loop {
        if l.start + l.duration() > write_at && !counts.is_empty() {
            write_at = l.last + Duration::from_millis(300);
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

            stream.write_rtp(
                RtpWrite::new(pt, seq_no, time, wallclock, [0x01, 0x02, 0x03, 0x04]).ext_vals(exts),
            );
        }

        progress(&mut l, &mut r)?;

        if l.duration() > Duration::from_secs(30) {
            break;
        }
    }

    let packets: Vec<_> = r
        .events
        .iter()
        .filter_map(|(_, e)| {
            let Event::RtpPacket(v) = e else {
                return None;
            };
            Some(v)
        })
        .collect();

    // Should only be the 4 unique sequence numbers.
    assert_eq!(packets.len(), 4);

    let h0 = packets[0].header.clone();
    let h1 = packets[1].header.clone();

    assert_eq!(h0.sequence_number, 47000);
    assert_eq!(h1.sequence_number, 47001);

    assert_eq!(h0.timestamp, 47_000_000);
    assert_eq!(h1.timestamp, 47_001_000);

    assert_eq!(h0.ext_vals.audio_level, Some(-42));
    assert_eq!(h1.ext_vals.audio_level, Some(-43));

    assert!(!h0.marker);
    assert!(!h1.marker);

    Ok(())
}
