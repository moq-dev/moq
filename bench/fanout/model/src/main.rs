//! Chat fan-out through the model alone: one finished group a cycle, which every
//! subscriber takes before parking again, as moq-net's `track_fanout_group` bench does.
//! Usage: `fanout-model SUBSCRIBERS CYCLES`. The work is deterministic, so its retired
//! instructions, less a run of zero cycles, are the per-group cost whatever the host does.
use std::{hint::black_box, task::Poll};

use bytes::Bytes;
use moq_net::{Timestamp, broadcast, cache, track};

fn main() {
	let mut args = std::env::args().skip(1).map(|a| a.parse::<usize>().unwrap());
	let (subscribers, cycles) = (args.next().unwrap(), args.next().unwrap());
	let mut info = broadcast::Info::default();
	info.pool = cache::Pool::new(cache::Config::default().with_capacity(64 * 1024).with_expiry(cache::DEFAULT_EXPIRY));
	let broadcast = broadcast::Producer::new(info);
	let track = broadcast
		.create_track("bench", track::Info::default().with_timescale(moq_net::Timescale::MILLI))
		.unwrap();
	let mut subs: Vec<_> = (0..subscribers).map(|_| track.subscribe(None)).collect();
	let waiters: Vec<_> = (0..subscribers).map(|_| kio::Waiter::noop()).collect();
	for (s, w) in subs.iter_mut().zip(&waiters) {
		assert!(matches!(s.poll_recv_group(w), Poll::Pending));
	}
	let payload = Bytes::from(vec![0; 64]);
	// Warm up past the first groups' one-time work, so both runs share it.
	for _ in 0..(16 + cycles) {
		let mut group = track.append_group().unwrap();
		group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
		group.finish().unwrap();
		for (s, w) in subs.iter_mut().zip(&waiters) {
			match s.poll_recv_group(w) {
				Poll::Ready(Ok(Some(g))) => {
					black_box(g);
				}
				_ => unreachable!(),
			}
			assert!(matches!(s.poll_recv_group(w), Poll::Pending));
		}
	}
	black_box(&broadcast);
}
