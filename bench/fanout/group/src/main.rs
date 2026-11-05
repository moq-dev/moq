//! A relay's frame fan-out: every subscriber drains one finished 512-frame group in batches,
//! as moq-net's `group_relay_objects/plain/512/*` bench does.
//! Usage: `fanout-group SUBSCRIBERS CYCLES`. The work is deterministic, so its retired
//! instructions, less a run of zero cycles, are the per-group cost whatever the host does.
use std::task::Poll;

const FRAMES: usize = 512;

fn main() {
	let mut args = std::env::args().skip(1).map(|a| a.parse::<usize>().unwrap());
	let (subscribers, cycles) = (args.next().unwrap(), args.next().unwrap());
	let (broadcast, track, group) = moq_net::fuzz::object_group(FRAMES, false);
	let mut buffer = moq_net::fuzz::ObjectBatch::<16>::new();
	for _ in 0..cycles {
		for _ in 0..subscribers {
			let mut reader = group.consume();
			loop {
				let Poll::Ready(count) = buffer.poll_read(&kio::Waiter::noop(), &mut reader) else {
					panic!("finished group blocked")
				};
				if count.unwrap() == 0 {
					break;
				}
				buffer.inspect();
			}
		}
	}
	// Left standing, so teardown stays out of the count.
	std::mem::forget((broadcast, track, group));
	std::process::exit(0);
}
