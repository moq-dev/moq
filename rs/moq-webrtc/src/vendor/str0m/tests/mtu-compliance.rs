use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use crate::vendor::str0m::format::Codec;
use crate::vendor::str0m::media::{Direction, MediaKind};
use crate::vendor::str0m::{DATAGRAM_MTU_TARGET_MAX, DATAGRAM_MTU_TARGET_MIN, Event, Rtc, RtcError};

use super::common;
use common::{Peer, TestRtc, init_crypto_default, init_log, progress_strict_mtu};

/// End-to-end MTU compliance: walk a full ICE/DTLS/SRTP session and
/// assert every outgoing datagram on either side has `len() <= mtu`. The
/// check fires inside the test pump via [`progress_strict_mtu`], so any
/// oversized datagram fails the test at the source.
fn run_mtu_compliance(mtu: usize) -> Result<(), RtcError> {
	init_log();
	init_crypto_default();

	let now = Instant::now();
	let l_rtc = Rtc::builder().set_mtu(mtu..=mtu).build(now);
	let r_rtc = Rtc::builder().set_mtu(mtu..=mtu).build(now);

	let mut l = TestRtc::new_with_rtc(Peer::Left.span(), l_rtc);
	let mut r = TestRtc::new_with_rtc(Peer::Right.span(), r_rtc);

	l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
	r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

	let mut change = l.sdp_api();
	let audio_mid = change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
	let (offer, pending) = change.apply().unwrap();
	let answer = r.rtc.sdp_api().accept_offer(offer)?;
	l.rtc.sdp_api().accept_answer(pending, answer)?;

	// ICE + DTLS handshake under strict MTU.
	loop {
		if l.is_connected() && r.is_connected() {
			break;
		}
		progress_strict_mtu(&mut l, &mut r, mtu)?;
	}

	let max = l.last.max(r.last);
	l.last = max;
	r.last = max;

	// Audio writes (Opus) at typical 20 ms cadence.
	let params = l.params_opus();
	assert_eq!(params.spec().codec, Codec::Opus);
	let pt = params.pt();
	let audio_payload = vec![0xAAu8; 160];

	let deadline = Duration::from_secs(2);
	while l.duration() < deadline {
		let wallclock = l.start + l.duration();
		let time = l.duration().into();
		if let Some(w) = l.writer(audio_mid) {
			w.write(pt, wallclock, time, audio_payload.clone())?;
		}
		progress_strict_mtu(&mut l, &mut r, mtu)?;
	}

	// Sanity: r received the audio.
	let media_events = r
		.events
		.iter()
		.filter(|(_, e)| matches!(e, Event::MediaData(_)))
		.count();
	assert!(
		media_events >= 1,
		"expected at least one MediaData event at r, got {media_events}"
	);

	Ok(())
}

#[test]
fn mtu_compliance_min() -> Result<(), RtcError> {
	run_mtu_compliance(DATAGRAM_MTU_TARGET_MIN)
}

#[test]
fn mtu_compliance_mid() -> Result<(), RtcError> {
	run_mtu_compliance((DATAGRAM_MTU_TARGET_MIN + DATAGRAM_MTU_TARGET_MAX) / 2)
}

#[test]
fn mtu_compliance_max() -> Result<(), RtcError> {
	run_mtu_compliance(DATAGRAM_MTU_TARGET_MAX)
}
