use std::net::Ipv4Addr;
use std::time::Duration;

use str0m::format::{Codec, CodecExtra};
use str0m::media::{Direction, MediaKind};
use str0m::rtp::RtpWrite;
use str0m::{Event, RtcError};

mod common;
use common::{Peer, TestRtc, init_log, progress};
use common::{av1_data, h264_data, h265_data, h266_data, init_crypto_default, vp8_data, vp9_data};

#[test]
pub fn test_vp8_keyframes_detection() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);
    let mut r = TestRtc::new(Peer::Right);

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    // The change is on the L (sending side) with Direction::SendRecv.
    let mut change = l.sdp_api();
    let mid = change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None);
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

    let params = l.params_vp8();
    assert_eq!(params.spec().codec, Codec::Vp8);
    let pt = params.pt();

    let data = vp8_data();

    for (relative, header, payload) in data {
        // Keep RTC time progressed to be "in sync" with the test data.
        while (l.last - max) < relative {
            progress(&mut l, &mut r)?;
        }

        let absolute = max + relative;

        let mut direct = l.direct_api();
        let tx = direct.stream_tx_by_mid(mid, None).unwrap();
        tx.write_rtp(
            RtpWrite::new(
                pt,
                header.sequence_number(None),
                header.timestamp,
                absolute,
                payload,
            )
            .marker(header.marker)
            .ext_vals(header.ext_vals)
            .nackable(true),
        );

        progress(&mut l, &mut r)?;

        if l.duration() > Duration::from_secs(5) {
            break;
        }
    }

    let iter = r.events.iter().filter_map(|(_, e)| {
        if let Event::MediaData(d) = e {
            Some(d)
        } else {
            None
        }
    });

    for data in iter {
        let CodecExtra::Vp8(extra) = data.codec_extra else {
            panic!("Got non VP8 CodecExtra")
        };
        let assume_keyframe = data.seq_range.contains(&14260.into())
            || data.seq_range.contains(&14262.into())
            || data.seq_range.contains(&14265.into());
        if extra.is_keyframe {
            assert!(assume_keyframe, "Expected keyframe");
        } else {
            assert!(!assume_keyframe, "Not expected keyframe");
        }
    }

    Ok(())
}

#[test]
pub fn test_vp9_keyframes_detection() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);
    let mut r = TestRtc::new(Peer::Right);

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    // The change is on the L (sending side) with Direction::SendRecv.
    let mut change = l.sdp_api();
    let mid = change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None);
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

    let params = l.params_vp9();
    assert_eq!(params.spec().codec, Codec::Vp9);
    let pt = params.pt();

    let data = vp9_data();

    for (relative, header, payload) in data {
        // Keep RTC time progressed to be "in sync" with the test data.
        while (l.last - max) < relative {
            progress(&mut l, &mut r)?;
        }

        let absolute = max + relative;

        let mut direct = l.direct_api();
        let tx = direct.stream_tx_by_mid(mid, None).unwrap();
        tx.write_rtp(
            RtpWrite::new(
                pt,
                header.sequence_number(None),
                header.timestamp,
                absolute,
                payload,
            )
            .marker(header.marker)
            .ext_vals(header.ext_vals)
            .nackable(true),
        );

        progress(&mut l, &mut r)?;

        if l.duration() > Duration::from_secs(5) {
            break;
        }
    }

    let iter = r.events.iter().filter_map(|(_, e)| {
        if let Event::MediaData(d) = e {
            Some(d)
        } else {
            None
        }
    });

    for data in iter {
        let CodecExtra::Vp9(extra) = data.codec_extra else {
            panic!("Got non VP9 CodecExtra")
        };
        let assume_keyframe = data.seq_range.contains(&19357.into())
            || data.seq_range.contains(&20296.into())
            || data.seq_range.contains(&20301.into())
            || data.seq_range.contains(&20351.into());
        if extra.is_keyframe {
            assert!(assume_keyframe, "Expected keyframe");
        } else {
            assert!(!assume_keyframe, "Not expected keyframe");
        }
    }

    Ok(())
}

#[test]
pub fn test_h264_keyframes_detection() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);
    let mut r = TestRtc::new(Peer::Right);

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    // The change is on the L (sending side) with Direction::SendRecv.
    let mut change = l.sdp_api();
    let mid = change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None);
    let (offer, pending) = change.apply().unwrap();

    let answer = r.rtc.sdp_api().accept_offer(offer)?;
    l.rtc.sdp_api().accept_answer(pending, answer)?;

    loop {
        if l.is_connected() || r.is_connected() {
            break;
        }
        let _ = progress(&mut l, &mut r);
    }

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    let params = l.params_h264();
    assert_eq!(params.spec().codec, Codec::H264);
    let pt = params.pt();

    let data = h264_data();

    for (relative, header, payload) in data {
        // Keep RTC time progressed to be "in sync" with the test data.
        while (l.last - max) < relative {
            let _ = progress(&mut l, &mut r);
        }

        let absolute = max + relative;

        let mut direct = l.direct_api();
        let tx = direct.stream_tx_by_mid(mid, None).unwrap();
        tx.write_rtp(
            RtpWrite::new(
                pt,
                header.sequence_number(None),
                header.timestamp,
                absolute,
                payload,
            )
            .marker(header.marker)
            .ext_vals(header.ext_vals)
            .nackable(true),
        );

        let _ = progress(&mut l, &mut r);

        if l.duration() > Duration::from_secs(5) {
            break;
        }
    }

    let iter = r.events.iter().filter_map(|(_, e)| {
        if let Event::MediaData(d) = e {
            Some(d)
        } else {
            None
        }
    });

    for data in iter {
        let CodecExtra::H264(extra) = data.codec_extra else {
            panic!("Got non H264 CodecExtra")
        };
        let assume_keyframe = data.seq_range.contains(&19249.into())
            || data.seq_range.contains(&19251.into())
            || data.seq_range.contains(&19301.into())
            || data.seq_range.contains(&19351.into())
            || data.seq_range.contains(&19403.into())
            || data.seq_range.contains(&19453.into())
            || data.seq_range.contains(&19503.into());
        if extra.is_keyframe {
            assert!(assume_keyframe, "Expected keyframe");
        } else {
            assert!(!assume_keyframe, "Not expected keyframe");
        }
    }

    Ok(())
}

#[test]
fn test_av1_keyframes_detection() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);
    let mut r = TestRtc::new(Peer::Right);

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    // The change is on the L (sending side) with Direction::SendOnly.
    let mut change = l.sdp_api();
    let mid = change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None);
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

    let params = l.params_av1();
    assert_eq!(params.spec().codec, Codec::Av1);
    let pt = params.pt();

    let data = av1_data();

    for (relative, header, payload) in data {
        // Keep RTC time progressed to be "in sync" with the test data.
        while (l.last - max) < relative {
            progress(&mut l, &mut r)?;
        }

        let absolute = max + relative;

        let mut direct = l.direct_api();
        let tx = direct.stream_tx_by_mid(mid, None).unwrap();
        tx.write_rtp(
            RtpWrite::new(
                pt,
                header.sequence_number(None),
                header.timestamp,
                absolute,
                payload,
            )
            .marker(header.marker)
            .ext_vals(header.ext_vals)
            .nackable(true),
        );

        progress(&mut l, &mut r)?;

        if l.duration() > Duration::from_secs(5) {
            break;
        }
    }

    let iter = r.events.iter().filter_map(|(_, e)| {
        if let Event::MediaData(d) = e {
            Some(d)
        } else {
            None
        }
    });

    for data in iter {
        let CodecExtra::Av1(extra) = data.codec_extra else {
            panic!("Got non AV1 CodecExtra")
        };
        let assume_keyframe =
            data.seq_range.contains(&7486.into()) || data.seq_range.contains(&7485.into());
        if extra.is_keyframe {
            assert!(assume_keyframe, "Expected keyframe");
        } else {
            assert!(!assume_keyframe, "Not expected keyframe");
        }
    }

    Ok(())
}

#[test]
pub fn test_h265_keyframes_detection() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);
    let mut r = TestRtc::new(Peer::Right);

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    // The change is on the L (sending side) with Direction::SendOnly.
    let mut change = l.sdp_api();
    let mid = change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None);
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

    let params = l.params_h265();
    assert_eq!(params.spec().codec, Codec::H265);
    let pt = params.pt();

    let expected_keyframe_seqs: Vec<u16> = include_str!("data/h265.txt")
        .lines()
        .filter(|l| l.contains("Keyframe"))
        .filter_map(|l| {
            let i = l.find("Seq=")?;
            let s = &l[i + 4..];
            let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
            s[..end].parse::<u16>().ok()
        })
        .collect();

    let data = h265_data();

    for (relative, header, payload) in data {
        // Keep RTC time progressed to be "in sync" with the test data.
        while (l.last - max) < relative {
            progress(&mut l, &mut r)?;
        }

        let absolute = max + relative;

        let mut direct = l.direct_api();
        let tx = direct.stream_tx_by_mid(mid, None).unwrap();
        tx.write_rtp(
            RtpWrite::new(
                pt,
                header.sequence_number(None),
                header.timestamp,
                absolute,
                payload,
            )
            .marker(header.marker)
            .ext_vals(header.ext_vals)
            .nackable(true),
        );

        progress(&mut l, &mut r)?;

        if l.duration() > Duration::from_secs(5) {
            break;
        }
    }

    let iter = r.events.iter().filter_map(|(_, e)| {
        if let Event::MediaData(d) = e {
            Some(d)
        } else {
            None
        }
    });

    for data in iter {
        let CodecExtra::H265(extra) = data.codec_extra else {
            panic!("Got non H265 CodecExtra")
        };
        let assume_keyframe = expected_keyframe_seqs
            .iter()
            .any(|&s| data.seq_range.contains(&(u64::from(s)).into()));
        if extra.is_keyframe {
            assert!(assume_keyframe, "Expected keyframe");
        } else {
            assert!(!assume_keyframe, "Not expected keyframe");
        }
    }

    Ok(())
}

#[test]
pub fn test_h266_keyframes_detection() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    // H.266 is opt-in (disabled by default), so enable it on both peers.
    let mut l = TestRtc::new_with_config(Peer::Left, |c| c.enable_h266(true));
    let mut r = TestRtc::new_with_config(Peer::Right, |c| c.enable_h266(true));

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    // The change is on the L (sending side) with Direction::SendOnly.
    let mut change = l.sdp_api();
    let mid = change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None);
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

    let params = l.params_h266();
    assert_eq!(params.spec().codec, Codec::H266);
    let pt = params.pt();

    let expected_keyframe_seqs: Vec<u16> = include_str!("data/h266.txt")
        .lines()
        .filter(|l| l.contains("Keyframe"))
        .filter_map(|l| {
            let i = l.find("Seq=")?;
            let s = &l[i + 4..];
            let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
            s[..end].parse::<u16>().ok()
        })
        .collect();

    let data = h266_data();

    for (relative, header, payload) in data {
        // Keep RTC time progressed to be "in sync" with the test data.
        while (l.last - max) < relative {
            progress(&mut l, &mut r)?;
        }

        let absolute = max + relative;

        let mut direct = l.direct_api();
        let tx = direct.stream_tx_by_mid(mid, None).unwrap();
        tx.write_rtp(
            RtpWrite::new(
                pt,
                header.sequence_number(None),
                header.timestamp,
                absolute,
                payload,
            )
            .marker(header.marker)
            .ext_vals(header.ext_vals)
            .nackable(true),
        );

        progress(&mut l, &mut r)?;

        if l.duration() > Duration::from_secs(5) {
            break;
        }
    }

    let iter = r.events.iter().filter_map(|(_, e)| {
        if let Event::MediaData(d) = e {
            Some(d)
        } else {
            None
        }
    });

    for data in iter {
        let CodecExtra::H266(extra) = data.codec_extra else {
            panic!("Got non H266 CodecExtra")
        };
        let assume_keyframe = expected_keyframe_seqs
            .iter()
            .any(|&s| data.seq_range.contains(&(u64::from(s)).into()));
        if extra.is_keyframe {
            assert!(assume_keyframe, "Expected keyframe");
        } else {
            assert!(!assume_keyframe, "Not expected keyframe");
        }
    }

    Ok(())
}
