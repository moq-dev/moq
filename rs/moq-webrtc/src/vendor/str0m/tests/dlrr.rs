use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use str0m::media::{Direction, MediaKind};
use str0m::rtp::RawPacket;
use str0m::rtp::rtcp::{ReportBlock, Rtcp};
use str0m::{Rtc, RtcError};
use tracing::info_span;

mod common;
use common::{TestRtc, init_crypto_default, init_log, negotiate, progress};

/// Verify that a StreamTx responds to an incoming RRTR with a DLRR.
///
/// L sends audio to R. R's StreamRx generates RRTR (Receiver Reference Time Report,
/// RFC 3611 §4.4) alongside its Receiver Reports. L's StreamTx should respond with a
/// DLRR (Delay since Last Receiver Report, RFC 3611 §4.5) in its next Sender Report,
/// allowing R to compute RTT.
#[test]
pub fn dlrr_response_to_rrtr() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let now = Instant::now();
    let l_rtc = Rtc::builder().enable_raw_packets(true).build(now);
    let r_rtc = Rtc::builder().enable_raw_packets(true).build(now);

    let mut l = TestRtc::new_with_rtc(info_span!("L"), l_rtc);
    let mut r = TestRtc::new_with_rtc(info_span!("R"), r_rtc);

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    // L sends audio to R: L gets a StreamTx, R gets a StreamRx.
    let mid = negotiate(&mut l, &mut r, |change| {
        change.add_media(MediaKind::Audio, Direction::SendOnly, None, None, None)
    });

    loop {
        if l.is_connected() || r.is_connected() {
            break;
        }
        progress(&mut l, &mut r)?;
    }

    // Sync clocks.
    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    let params = l.params_opus();
    let pt = params.pt();
    let data = [1_u8; 80];

    // Run ~15s of synthetic time. R will send RRTR with its Receiver Reports
    // (~every 5s), and L should reply with DLRR in its next Sender Report.
    loop {
        let wallclock = l.start + l.duration();
        let time = l.duration().into();
        l.writer(mid).unwrap().write(pt, wallclock, time, data)?;
        progress(&mut l, &mut r)?;
        if l.duration() > Duration::from_secs(15) {
            break;
        }
    }

    let media_ssrc = l
        .events
        .iter()
        .find_map(|(_, e)| match e.as_raw_packet() {
            Some(RawPacket::RtpTx(header, _)) => Some(header.ssrc),
            _ => None,
        })
        .expect("L should have sent RTP");

    // R should have sent at least one RRTR, using its local RTCP SSRC rather than
    // the remote media SSRC it is reporting on.
    let rrtr_ssrc = r
        .events
        .iter()
        .find_map(|(_, e)| {
            let Some(RawPacket::RtcpTx(Rtcp::ExtendedReport(xr))) = e.as_raw_packet() else {
                return None;
            };
            xr.blocks
                .iter()
                .any(|b| matches!(b, ReportBlock::Rrtr(_)))
                .then_some(xr.ssrc)
        })
        .expect("R should have sent at least one RRTR");
    assert_ne!(rrtr_ssrc, media_ssrc);

    // L should have sent at least one DLRR in response.
    let dlrr_reports: Vec<_> = l
        .events
        .iter()
        .filter_map(|(_, e)| {
            if let Some(RawPacket::RtcpTx(Rtcp::ExtendedReport(xr))) = e.as_raw_packet() {
                if xr.blocks.iter().any(|b| matches!(b, ReportBlock::Dlrr(_))) {
                    return Some(xr);
                }
            }
            None
        })
        .collect();
    assert!(
        !dlrr_reports.is_empty(),
        "L should have sent DLRR in response to RRTR"
    );

    // Verify the DLRR contains valid data.
    let dlrr = dlrr_reports[0]
        .blocks
        .iter()
        .find_map(|b| match b {
            ReportBlock::Dlrr(d) => Some(d),
            _ => None,
        })
        .unwrap();
    assert!(!dlrr.items.is_empty(), "DLRR should have at least one item");
    let item = &dlrr.items[0];
    assert_eq!(item.ssrc, rrtr_ssrc);

    // The last_rr_time field must be the middle 32 bits of the RRTR's NTP timestamp.
    // Verify it's non-zero and that R received the same value back (round-trip integrity).
    assert!(
        item.last_rr_time != 0,
        "DLRR last_rr_time should be non-zero"
    );

    // Verify R actually received the DLRR that L sent.
    let r_received_dlrr = r.events.iter().any(|(_, e)| {
        if let Some(RawPacket::RtcpRx(Rtcp::ExtendedReport(xr))) = e.as_raw_packet() {
            xr.blocks.iter().any(|b| {
                if let ReportBlock::Dlrr(d) = b {
                    d.items.iter().any(|i| i.last_rr_time == item.last_rr_time)
                } else {
                    false
                }
            })
        } else {
            false
        }
    });
    assert!(
        r_received_dlrr,
        "R should have received the DLRR with matching last_rr_time"
    );

    Ok(())
}
