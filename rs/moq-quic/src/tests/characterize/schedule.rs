//! Golden tests of the order the client sends stream and datagram bytes in
//!
//! Each golden lists the packets the server received carrying stream or datagram data, one per
//! line: `s<stream>@<offset>+<len>` for a STREAM frame (`!` marks FIN) and `d+<len>` for a
//! DATAGRAM. A scheduler change that reorders bytes shows up as a diff of these lines.

use super::*;

const LEN: usize = 3000;

/// Drives the pair to idle, asserting the server received stream and datagram frames in the
/// packets `golden` lists, one per line
#[track_caller]
fn assert_golden(pair: &mut Pair, golden: &str) {
	pair.server.capture_inbound_packets = true;
	pair.drive();
	pair.server.capture_inbound_packets = false;
	let mut lines = Vec::new();
	for payload in mem::take(&mut pair.server.captured_packets) {
		let mut line = Vec::new();
		for frame in frame::Iter::new(payload.into()).unwrap() {
			match frame.unwrap() {
				Frame::Stream(s) => {
					let fin = if s.fin { "!" } else { "" };
					let id = u64::from(s.id);
					line.push(format!("s{id}@{}+{}{fin}", s.offset, s.data.len()));
				}
				Frame::Datagram(d) => line.push(format!("d+{}", d.data.len())),
				_ => {}
			}
		}
		if !line.is_empty() {
			lines.push(line.join(" "));
		}
	}
	let golden: Vec<_> = golden.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
	assert_eq!(lines.join("\n"), golden.join("\n"));
}

/// Opens a client uni stream at `priority` and queues `LEN` bytes and a FIN on it
fn queue(pair: &mut Pair, ch: ConnectionHandle, priority: i64) -> StreamId {
	let s = pair.client_streams(ch).open(Dir::Uni, Default::default()).unwrap();
	pair.client_send(ch, s).set_priority(priority).unwrap();
	assert_eq!(pair.client_send(ch, s).write(&[0; LEN]).unwrap(), LEN);
	pair.client_send(ch, s).finish().unwrap();
	s
}

/// A connected pair with the server's flush of post-handshake traffic out of the way
fn idle(fairness: bool) -> (Pair, ConnectionHandle) {
	let (mut pair, client_ch, _) = connected_with(|_| {}, |t| t.send_fairness = fairness);
	pair.drive();
	(pair, client_ch)
}

/// Higher priorities drain first, whatever order the streams opened in
#[test]
fn priorities() {
	let _guard = subscribe();
	let (mut pair, ch) = idle(true);
	queue(&mut pair, ch, 0);
	queue(&mut pair, ch, 10);
	queue(&mut pair, ch, -5);
	assert_golden(
		&mut pair,
		"
        s6@0+1424
        s6@1424+1422
        s6@2846+154! s2@0+1264
        s2@1264+1422
        s2@2686+314! s10@0+1104
        s10@1104+1422
        s10@2526+474!
        ",
	);
}

/// Equal-priority streams take turns, a packet at a time
#[test]
fn equal_priority_fair() {
	let _guard = subscribe();
	let (mut pair, ch) = idle(true);
	for _ in 0..3 {
		queue(&mut pair, ch, 0);
	}
	assert_golden(
		&mut pair,
		"
        s2@0+1424
        s6@0+1424
        s10@0+1424
        s2@1424+1422
        s6@1424+1422
        s10@1424+1422
        s2@2846+154! s6@2846+154! s10@2846+154!
        ",
	);
}

/// Without fairness, equal-priority streams drain in the order they were opened
#[test]
fn equal_priority_unfair() {
	let _guard = subscribe();
	let (mut pair, ch) = idle(false);
	for _ in 0..3 {
		queue(&mut pair, ch, 0);
	}
	assert_golden(
		&mut pair,
		"
        s2@0+1424
        s2@1424+1422
        s2@2846+154! s6@0+1264
        s6@1264+1422
        s6@2686+314! s10@0+1104
        s10@1104+1422
        s10@2526+474!
        ",
	);
}

/// A lost packet's data waits behind a higher-priority stream queued after the loss
#[test]
fn retransmit_behind_higher_priority() {
	let _guard = subscribe();
	let (mut pair, ch) = idle(true);
	queue(&mut pair, ch, 0);

	// Lose the first packet of the flight
	let now = pair.time;
	pair.client.drive(now, pair.server.addr);
	let _ = pair.client.outbound.pop_front().unwrap();
	queue(&mut pair, ch, 5);
	assert_golden(
		&mut pair,
		"
        s2@1424+1422
        s2@2846+154!
        s6@0+1424
        s6@1424+1422
        s6@2846+154!
        s2@0+1424
        ",
	);
}

/// Datagrams go before stream data of equal priority, and one that does not fit ends the packet
#[test]
fn datagrams_interleaved() {
	let _guard = subscribe();
	let (mut pair, ch) = idle(true);
	queue(&mut pair, ch, 0);
	for _ in 0..3 {
		pair.client_datagrams(ch)
			.send(vec![0; 500].into(), Default::default())
			.unwrap();
	}
	queue(&mut pair, ch, 0);
	pair.client_datagrams(ch)
		.send(vec![0; 1000].into(), Default::default())
		.unwrap();
	assert_golden(
		&mut pair,
		"
        d+500 d+500
        d+500
        d+1000 s2@0+421
        s6@0+1424
        s2@421+1422
        s6@1424+1422
        s2@1843+1157! s6@2846+154!
        ",
	);
}

/// A stream longer than the congestion window loses a packet of its first flight; the lost
/// range goes out again ahead of the stream's unsent data
#[test]
fn retransmit_within_stream() {
	let _guard = subscribe();
	let (mut pair, ch) = idle(true);
	let s = pair.client_streams(ch).open(Dir::Uni, Default::default()).unwrap();
	assert_eq!(pair.client_send(ch, s).write(&[0; 16_000]).unwrap(), 16_000);

	// Lose the second packet of the first flight
	let now = pair.time;
	pair.client.drive(now, pair.server.addr);
	let _ = pair.client.outbound.remove(1).unwrap();
	assert_golden(
		&mut pair,
		"
        s2@0+1424
        s2@2846+1422
        s2@4268+1422
        s2@5690+1422
        s2@7112+1422
        s2@8534+1422
        s2@9956+1422
        s2@1424+1422
        s2@11378+1422
        s2@12800+1422
        s2@14222+1422
        s2@15644+356
        ",
	);
}
