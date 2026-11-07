use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

mod common;
use common::{TestRtc, init_crypto_default, init_log, negotiate, progress};
use str0m::Rtc;
use str0m::format::{Codec, CodecSpec, FormatParams, PayloadParams};
use str0m::media::{Direction, Frequency, MediaKind, MediaTime, Mid};
use str0m::rtp::RtpWrite;
use str0m::{Event, RtcError};
use tracing::{Span, info_span};

#[test]
fn negotiates_all_supported_clock_rates() {
    init_log();
    init_crypto_default();

    let params = comfort_noise_params();
    let (l, r) = with_params(info_span!("L"), &params, info_span!("R"), &params);

    for rtc in [&l, &r] {
        let negotiated: Vec<_> = rtc
            .codec_config()
            .iter()
            .filter(|p| p.spec().codec == Codec::CN)
            .map(|p| (*p.pt(), p.spec().clock_rate.get()))
            .collect();
        assert_eq!(
            negotiated,
            vec![
                (13, 8_000),
                (96, 16_000),
                (97, 24_000),
                (98, 32_000),
                (99, 48_000),
            ]
        );
    }
}

#[test]
fn frame_mode_round_trips_all_supported_clock_rates() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let params = comfort_noise_params();
    let (mut l, mut r, mid) = connected_with_params(&params, false);

    for (index, param) in params.iter().enumerate() {
        let wallclock = l.start + l.duration();
        let time = MediaTime::new(
            param.spec().clock_rate.get() as u64,
            param.spec().clock_rate,
        );
        l.writer(mid)
            .unwrap()
            .write(param.pt(), wallclock, time, vec![index as u8 + 1])?;
        progress(&mut l, &mut r)?;
    }

    progress_until_cn_events(&mut l, &mut r, false)?;

    let received: Vec<_> = r
        .events
        .iter()
        .filter_map(|(_, event)| match event {
            Event::MediaData(data) if data.params.spec().codec == Codec::CN => Some((
                *data.pt,
                data.params.spec().clock_rate.get(),
                data.data.as_ref().to_vec(),
            )),
            _ => None,
        })
        .collect();

    assert_eq!(received, expected_cn_events());
    Ok(())
}

#[test]
fn frame_mode_switching_through_cn_does_not_stall_primary_audio() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let params = [pcmu(), comfort_noise(13, 8_000)];
    let (mut l, mut r, mid) = connected_with_default_audio_reordering(&params);

    for (pt, timestamp, payload) in [(0_u8, 0_u64, 1_u8), (13, 160, 42), (0, 320, 2)] {
        let wallclock = l.start + l.duration();
        l.writer(mid).unwrap().write(
            pt.into(),
            wallclock,
            MediaTime::new(timestamp, Frequency::EIGHT_KHZ),
            [payload],
        )?;
        progress(&mut l, &mut r)?;
    }

    let deadline = l.duration() + Duration::from_secs(2);
    while l.duration() < deadline {
        progress(&mut l, &mut r)?;
    }

    let received: Vec<_> = r
        .events
        .iter()
        .filter_map(|(_, event)| match event {
            Event::MediaData(data) => Some((data.params.spec().codec, data.data.as_ref().to_vec())),
            _ => None,
        })
        .collect();
    assert_eq!(
        received,
        vec![
            (Codec::PCMU, vec![1]),
            (Codec::CN, vec![42]),
            (Codec::PCMU, vec![2]),
        ],
        "the CN sequence number must not look like a lost PCMU packet"
    );
    Ok(())
}

#[test]
fn frame_mode_cn_never_sets_marker_bit() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let params = [comfort_noise(13, 8_000)];
    let (mut l, mut r, mid) = connected_with_params(&params, false);
    let wallclock = l.start + l.duration();
    l.writer(mid).unwrap().start_of_talkspurt(true).write(
        params[0].pt(),
        wallclock,
        MediaTime::new(8_000, Frequency::EIGHT_KHZ),
        [42],
    )?;

    let deadline = l.duration() + Duration::from_secs(2);
    while l.duration() < deadline {
        progress(&mut l, &mut r)?;
        if let Some(data) = r.events.iter().find_map(|(_, event)| match event {
            Event::MediaData(data) if data.params.spec().codec == Codec::CN => Some(data),
            _ => None,
        }) {
            assert!(
                !data.audio_start_of_talk_spurt,
                "an RFC 3389 CN packet must not carry the RTP marker bit"
            );
            return Ok(());
        }
    }

    panic!("timed out waiting for Comfort Noise payload");
}

#[test]
fn rtp_mode_round_trips_all_supported_clock_rates() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let params = comfort_noise_params();
    let (mut l, mut r, mid) = connected_with_params(&params, true);
    let ssrc = l.direct_api().stream_tx_by_mid(mid, None).unwrap().ssrc();

    for (index, param) in params.iter().enumerate() {
        let wallclock = l.start + l.duration();
        let timestamp = param.spec().clock_rate.get();
        l.direct_api()
            .stream_tx(&ssrc)
            .unwrap()
            .write_rtp(RtpWrite::new(
                param.pt(),
                (index as u64 + 1).into(),
                timestamp,
                wallclock,
                [index as u8 + 1],
            ));
        progress(&mut l, &mut r)?;
    }

    progress_until_cn_events(&mut l, &mut r, true)?;

    let received: Vec<_> = r
        .events
        .iter()
        .filter_map(|(_, event)| match event {
            Event::RtpPacket(packet) => Some((
                *packet.header.payload_type,
                packet.time.frequency().get(),
                packet.payload.as_ref().to_vec(),
            )),
            _ => None,
        })
        .collect();

    assert_eq!(received, expected_cn_events());
    Ok(())
}

fn with_params(
    span_l: Span,
    params_l: &[PayloadParams],
    span_r: Span,
    params_r: &[PayloadParams],
) -> (TestRtc, TestRtc) {
    let mut l = build_params_with_mode(span_l, params_l, false);
    let mut r = build_params_with_mode(span_r, params_r, false);

    negotiate(&mut l, &mut r, |change| {
        change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
    });

    (l, r)
}

fn connected_with_params(params: &[PayloadParams], rtp_mode: bool) -> (TestRtc, TestRtc, Mid) {
    connected_with_reordering(params, rtp_mode, Some(0))
}

fn connected_with_default_audio_reordering(params: &[PayloadParams]) -> (TestRtc, TestRtc, Mid) {
    connected_with_reordering(params, false, None)
}

fn connected_with_reordering(
    params: &[PayloadParams],
    rtp_mode: bool,
    reordering_size_audio: Option<usize>,
) -> (TestRtc, TestRtc, Mid) {
    let mut l = build_params(info_span!("L"), params, rtp_mode, reordering_size_audio);
    let mut r = build_params(info_span!("R"), params, rtp_mode, reordering_size_audio);

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    let mid = negotiate(&mut l, &mut r, |change| {
        change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None)
    });

    while !l.is_connected() || !r.is_connected() {
        progress(&mut l, &mut r).expect("clean progress");
    }

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    (l, r, mid)
}

fn build_params_with_mode(span: Span, params: &[PayloadParams], rtp_mode: bool) -> TestRtc {
    build_params(span, params, rtp_mode, Some(0))
}

fn build_params(
    span: Span,
    params: &[PayloadParams],
    rtp_mode: bool,
    reordering_size_audio: Option<usize>,
) -> TestRtc {
    let mut builder = Rtc::builder().clear_codecs().set_rtp_mode(rtp_mode);
    if let Some(size) = reordering_size_audio {
        builder = builder.set_reordering_size_audio(size);
    }
    let config = builder.codec_config();
    for param in params {
        config.add_config(
            param.pt(),
            param.resend(),
            param.spec().codec,
            param.spec().clock_rate,
            param.spec().channels,
            param.spec().format,
        );
    }
    TestRtc::new_with_rtc(span, builder.build(Instant::now()))
}

fn progress_until_cn_events(
    l: &mut TestRtc,
    r: &mut TestRtc,
    rtp_mode: bool,
) -> Result<(), RtcError> {
    let deadline = l.duration() + Duration::from_secs(2);
    while l.duration() < deadline {
        let count = r
            .events
            .iter()
            .filter(|(_, event)| {
                if rtp_mode {
                    matches!(event, Event::RtpPacket(_))
                } else {
                    matches!(
                        event,
                        Event::MediaData(data)
                            if data.params.spec().codec == Codec::CN
                    )
                }
            })
            .count();
        if count == 5 {
            return Ok(());
        }
        progress(l, r)?;
    }

    panic!("timed out waiting for all five Comfort Noise payloads");
}

fn comfort_noise(pt: u8, clock_rate: u32) -> PayloadParams {
    PayloadParams::new(
        pt.into(),
        None,
        CodecSpec {
            codec: Codec::CN,
            channels: None,
            clock_rate: Frequency::new(clock_rate).unwrap(),
            format: FormatParams::default(),
        },
    )
}

fn pcmu() -> PayloadParams {
    PayloadParams::new(
        0.into(),
        None,
        CodecSpec {
            codec: Codec::PCMU,
            channels: None,
            clock_rate: Frequency::EIGHT_KHZ,
            format: FormatParams::default(),
        },
    )
}

fn comfort_noise_params() -> [PayloadParams; 5] {
    [
        comfort_noise(13, 8_000),
        comfort_noise(96, 16_000),
        comfort_noise(97, 24_000),
        comfort_noise(98, 32_000),
        comfort_noise(99, 48_000),
    ]
}

fn expected_cn_events() -> Vec<(u8, u32, Vec<u8>)> {
    vec![
        (13, 8_000, vec![1]),
        (96, 16_000, vec![2]),
        (97, 24_000, vec![3]),
        (98, 32_000, vec![4]),
        (99, 48_000, vec![5]),
    ]
}
