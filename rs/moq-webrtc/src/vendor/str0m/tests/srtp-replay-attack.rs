use std::time::{Duration, Instant};

use str0m::format::Codec;
use str0m::media::MediaKind;
use str0m::net::Receive;
use str0m::rtp::{ExtensionValues, RawPacket, RtpWrite, SeqNo, Ssrc};
use str0m::{Event, Input, Output, Rtc, RtcError};

mod common;
use common::{TestRtc, connect_l_r, connect_l_r_with_rtc, init_crypto_default, init_log};

const EXPECTED_PACKETS: usize = 50;
const REPLAY_PER_PACKET: usize = 5;

#[test]
pub fn srtp_replay_attack_rtp_mode() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let (mut l, mut r) = connect_l_r();
    let mid = "aud".into();

    let ssrc_tx: Ssrc = 42.into();
    l.direct_api().declare_media(mid, MediaKind::Audio);
    l.direct_api().declare_stream_tx(ssrc_tx, None, mid, None);
    r.direct_api().declare_media(mid, MediaKind::Audio);

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    let params = l.params_opus();
    let ssrc = l.direct_api().stream_tx_by_mid(mid, None).unwrap().ssrc();
    assert_eq!(params.spec().codec, Codec::Opus);
    let pt = params.pt();
    let mut write_at = l.last + Duration::from_millis(20);
    let mut seq_no: SeqNo = 0_u64.into();
    let mut time = 0;
    let mut send_count = 0;
    const TIME_INTERVAL: u32 = 960;

    // Process the DTLS Handshake, before we start duplicating SRTP packets
    progress_with_replay(&mut l, &mut r, 1)?;

    loop {
        if l.start + l.duration() > write_at && send_count < EXPECTED_PACKETS {
            seq_no.inc();
            time += TIME_INTERVAL;
            write_at = l.last + Duration::from_millis(20);
            let wallclock = l.start + l.duration();
            let mut direct = l.direct_api();
            let stream = direct.stream_tx(&ssrc).unwrap();
            let exts = ExtensionValues {
                audio_level: Some(-42),
                voice_activity: Some(false),
                ..Default::default()
            };

            stream
                .write_rtp(RtpWrite::new(pt, seq_no, time, wallclock, [1, 3, 3, 7]).ext_vals(exts));
            send_count += 1;
        }

        progress_with_replay(&mut l, &mut r, REPLAY_PER_PACKET)?;

        if l.duration() > Duration::from_secs(5) {
            break;
        }
    }

    let rtp_raw_rx: Vec<_> = r
        .events
        .iter()
        .filter_map(|(_, e)| {
            if let Some(RawPacket::RtpRx(header, payload)) = e.as_raw_packet() {
                Some((header, payload))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(rtp_raw_rx.len(), EXPECTED_PACKETS);

    let rtp: Vec<_> = r
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
    assert_eq!(rtp.len(), EXPECTED_PACKETS);
    Ok(())
}

#[test]
pub fn srtp_replay_attack_frame_mode() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let now = Instant::now();
    let rtc1 = Rtc::builder()
        .set_rtp_mode(true)
        .enable_raw_packets(true)
        .build(now);
    let rtc2 = Rtc::builder()
        .enable_raw_packets(true)
        // release packet straight away
        .set_reordering_size_audio(0)
        .build(now);

    let (mut l, mut r) = connect_l_r_with_rtc(rtc1, rtc2);

    let mid = "aud".into();

    let ssrc_tx: Ssrc = 42.into();
    l.direct_api().declare_media(mid, MediaKind::Audio);
    l.direct_api().declare_stream_tx(ssrc_tx, None, mid, None);
    r.direct_api().declare_media(mid, MediaKind::Audio);

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    let params = l.params_opus();
    let ssrc = l.direct_api().stream_tx_by_mid(mid, None).unwrap().ssrc();
    assert_eq!(params.spec().codec, Codec::Opus);
    let pt = params.pt();
    let mut write_at = l.last + Duration::from_millis(20);
    let mut seq_no: SeqNo = 0_u64.into();
    let mut time = 0;
    let mut send_count = 0;
    const TIME_INTERVAL: u32 = 960;

    // Process the DTLS Handshake, before we start duplicating SRTP packets
    progress_with_replay(&mut l, &mut r, 1)?;

    loop {
        if l.start + l.duration() > write_at && send_count < EXPECTED_PACKETS {
            seq_no.inc();
            time += TIME_INTERVAL;
            write_at = l.last + Duration::from_millis(20);
            let wallclock = l.start + l.duration();
            let mut direct = l.direct_api();
            let stream = direct.stream_tx(&ssrc).unwrap();
            let exts = ExtensionValues {
                audio_level: Some(-42),
                voice_activity: Some(false),
                ..Default::default()
            };

            stream
                .write_rtp(RtpWrite::new(pt, seq_no, time, wallclock, [1, 3, 3, 7]).ext_vals(exts));
            send_count += 1;
        }

        progress_with_replay(&mut l, &mut r, REPLAY_PER_PACKET)?;

        if l.duration() > Duration::from_secs(5) {
            break;
        }
    }

    let rtp_raw_rx: Vec<_> = r
        .events
        .iter()
        .filter_map(|(_, e)| {
            if let Some(RawPacket::RtpRx(header, payload)) = e.as_raw_packet() {
                Some((header, payload))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(rtp_raw_rx.len(), EXPECTED_PACKETS);

    let media: Vec<_> = r
        .events
        .iter()
        .filter_map(|(_, e)| {
            if let Event::MediaData(v) = e {
                Some(v)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(media.len(), EXPECTED_PACKETS);
    Ok(())
}

pub fn progress_with_replay(
    l: &mut TestRtc,
    r: &mut TestRtc,
    replay: usize,
) -> Result<(), RtcError> {
    let (f, t) = if l.last < r.last { (l, r) } else { (r, l) };

    loop {
        f.span
            .in_scope(|| f.rtc.handle_input(Input::Timeout(f.last)))?;

        match f.span.in_scope(|| f.rtc.poll_output())? {
            Output::Timeout(v) => {
                let tick = f.last + Duration::from_millis(10);
                f.last = if v == f.last { tick } else { tick.min(v) };
                break;
            }
            Output::Transmit(v) => {
                let data = v.contents;
                for _ in 0..replay {
                    let input = Input::Receive(
                        f.last,
                        Receive {
                            proto: v.proto,
                            source: v.source,
                            destination: v.destination,
                            contents: (&*data).try_into().unwrap(),
                        },
                    );
                    t.span.in_scope(|| t.rtc.handle_input(input)).unwrap();
                }
            }
            Output::Event(v) => {
                f.events.push((f.last, v));
            }
        }
    }

    Ok(())
}
