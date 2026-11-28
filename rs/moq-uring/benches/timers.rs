//! Timer churn on a worker's queue: a QUIC driver re-arms its timer every
//! loop, so the cost of a re-arm, swept over how many other timers are armed,
//! shows whether the queue's cost grows with the table.

#[cfg(target_os = "linux")]
mod timers {
	use std::time::Duration;

	use moq_uring::{Config, Timer, Worker};
	use swole::bench;

	/// A timer that re-arms among `others`, all on one worker.
	struct Table {
		timer: Timer,
		/// Two instants a re-arm alternates between, so every set moves it.
		at: [kio::clock::Instant; 2],
		flip: usize,
		_others: Vec<Timer>,
		_worker: Worker,
	}

	fn table(armed: u32) -> Table {
		let worker = Worker::new(Config::default()).expect("io_uring worker");
		let handle = worker.handle();
		// The rest of the table, spread across a minute so the re-armed timer
		// lands among them.
		let now = kio::clock::now();
		let others = (0..armed)
			.map(|i| {
				let mut timer = handle.timer();
				timer.set(Some(now.saturating_add(Duration::from_secs(60) * i / armed)));
				timer
			})
			.collect();
		Table {
			timer: handle.timer(),
			at: [
				now.saturating_add(Duration::from_secs(30)),
				now.saturating_add(Duration::from_secs(31)),
			],
			flip: 0,
			_others: others,
			_worker: worker,
		}
	}

	#[bench(args = [1, 64, 1024, 16384], setup = table)]
	fn rearm(table: &mut Table) {
		table.flip ^= 1;
		table.timer.set(Some(table.at[table.flip]));
	}
}

fn main() {
	swole::main();
}
