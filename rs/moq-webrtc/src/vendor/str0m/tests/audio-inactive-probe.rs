use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use netem::Bitrate;
use str0m::media::{Direction, MediaKind};
use str0m::{Rtc, RtcError};

mod common;
use common::{Peer, TestRtc, init_crypto_default, init_log, progress};

/// Similar test but with bandwidth estimation enabled to increase probe likelihood
#[test]
pub fn audio_stream_then_inactive_with_bwe() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    // Only sender (L) needs BWE enabled
    let rtc1 = Rtc::builder()
        .enable_bwe(Some(Bitrate::kbps(800)))
        .build(Instant::now());

    let mut l = TestRtc::new_with_rtc(Peer::Left.span(), rtc1);
    let mut r = TestRtc::new(Peer::Right);

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    // Step 1: Negotiate an audio stream with BWE
    let mut change = l.sdp_api();
    let mid = change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
    let (offer, pending) = change.apply().unwrap();

    let answer = r.rtc.sdp_api().accept_offer(offer)?;
    l.rtc.sdp_api().accept_answer(pending, answer)?;

    // Wait for connection
    loop {
        if l.is_connected() || r.is_connected() {
            break;
        }
        progress(&mut l, &mut r)?;
    }

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    let params = l.params_opus();
    let pt = params.pt();

    // Step 2: Send enough audio data to potentially trigger BWE probing
    let data = vec![1_u8; 80];

    for _ in 0..50 {
        let wallclock = l.start + l.duration();
        let time = l.duration().into();

        if let Some(writer) = l.writer(mid) {
            let _ = writer.write(pt, wallclock, time, data.clone());
        }

        progress(&mut l, &mut r)?;

        if l.duration() > Duration::from_secs(2) {
            break;
        }
    }

    // Step 3: Disable the stream immediately (without draining probe queue)
    let mut change = l.sdp_api();
    change.set_direction(mid, Direction::Inactive);
    let (offer, pending) = change.apply().unwrap();

    let answer = r.rtc.sdp_api().accept_offer(offer)?;
    l.rtc.sdp_api().accept_answer(pending, answer)?;

    // Step 4: Continue progressing - this is where probe_queue access might fail
    // if the pacer tries to use probe_queue when there's no video queue available
    loop {
        progress(&mut l, &mut r)?;

        if l.duration() > Duration::from_secs(240) {
            break;
        }
    }

    Ok(())
}
