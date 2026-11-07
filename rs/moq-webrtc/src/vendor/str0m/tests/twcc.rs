use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use str0m::format::Codec;
use str0m::media::{Direction, MediaKind};
use str0m::rtp::rtcp::Twcc;
use str0m::{Rtc, RtcError};
use tracing::info_span;

mod common;
use common::{TestRtc, init_crypto_default, init_log, negotiate, progress};

#[test]
pub fn twcc() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let now = Instant::now();
    let l_rtc = Rtc::builder().enable_raw_packets(true).build(now);
    let r_rtc = Rtc::builder().enable_raw_packets(true).build(now);

    let mut l = TestRtc::new_with_rtc(info_span!("L"), l_rtc);
    let mut r = TestRtc::new_with_rtc(info_span!("R"), r_rtc);

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    let mid = negotiate(&mut l, &mut r, |change| {
        change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None)
    });

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

    let data_a = [1_u8; 80];

    loop {
        {
            let wallclock = l.start + l.duration();
            let time = l.duration().into();
            l.writer(mid).unwrap().write(pt, wallclock, time, data_a)?;
        }

        progress(&mut l, &mut r)?;

        if l.duration() > Duration::from_secs(10) {
            break;
        }
    }

    let (sent_twcc, received_twcc) = {
        use str0m::rtp::{RawPacket, rtcp::Rtcp};
        let r_twcc: Vec<_> = r
            .events
            .iter()
            .filter_map(|(_, e)| {
                if let Some(RawPacket::RtcpTx(Rtcp::Twcc(twcc))) = e.as_raw_packet() {
                    Some(twcc)
                } else {
                    None
                }
            })
            .collect();

        let l_twcc: Vec<_> = l
            .events
            .iter()
            .filter_map(|(_, e)| {
                if let Some(RawPacket::RtcpRx(Rtcp::Twcc(twcc))) = e.as_raw_packet() {
                    Some(twcc)
                } else {
                    None
                }
            })
            .collect();
        (r_twcc, l_twcc)
    };

    assert!(!sent_twcc.is_empty(), "Should've sent TWCC");
    assert!(
        sent_twcc.len() == received_twcc.len(),
        "The number of TWCC packets received should match what was sent"
    );
    assert!(
        sent_twcc == received_twcc,
        "The same TWCC packets that were sent should be received"
    );

    let sent_is_consecutive = sent_twcc
        .iter()
        .fold((true, None), |(consecutive, last), packet| {
            let consecutive = consecutive
                && last
                    .map(|l: &Twcc| l.feedback_count.wrapping_add(1) == packet.feedback_count)
                    .unwrap_or(true);

            (consecutive, Some(packet))
        })
        .0;

    assert!(!sent_twcc.is_empty(), "Should have sent some TWCC");
    assert!(
        sent_is_consecutive,
        "Sent TWCC packets should contain no gaps in terms of `feedback_count`"
    );

    Ok(())
}
