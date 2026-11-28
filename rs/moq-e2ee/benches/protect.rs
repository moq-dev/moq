//! AES-128-GCM throughput for grouped frames and datagrams through the public API.
//!
//! Run with `cargo bench -p moq-e2ee --bench protect`.

use moq_e2ee::Credential;
use moq_e2ee::credential::Config;

const GROUPED: [u8; 1024] = [0; 1024];
const OPUS: [u8; 160] = [0; 160];

fn credential() -> Credential {
	Credential::new(Config {
		context: "bench".into(),
		kid: 0,
		secret: [0x11; 32],
	})
	.unwrap()
}

/// Each producer is its own publisher instance, so it gets its own epoch.
fn broadcast() -> moq_e2ee::broadcast::Producer {
	moq_e2ee::broadcast::Producer::new(moq_net::broadcast::Info::new().produce(), credential().produce())
}

mod protect {
	use std::time::Duration;

	use moq_net::Timestamp;
	use swole::bench;

	use super::{GROUPED, OPUS, broadcast};

	/// A track producer and the millisecond its next group starts at.
	struct Track {
		producer: moq_e2ee::track::Producer,
		ms: u64,
	}

	fn track() -> Track {
		// Advancing timestamps plus a zero max age leave only the newest group fresh.
		let info = moq_net::track::Info::default()
			.with_timescale(moq_net::Timescale::MILLI)
			.with_max_age(Duration::ZERO);
		let producer = broadcast().create_track("video", info).unwrap().unwrap();
		Track { producer, ms: 0 }
	}

	#[bench(setup = track, bytes = GROUPED.len())]
	fn grouped_1k(track: &mut Track) {
		track.ms += 1;
		let at = Timestamp::from_millis(track.ms).unwrap();
		let mut group = track.producer.append_group(at).unwrap();
		group.write_frame(at, &GROUPED[..]).unwrap();
		group.finish().unwrap();
	}

	/// A datagram producer and the millisecond its next datagram carries.
	struct Datagrams {
		producer: moq_e2ee::datagram::Producer,
		ms: u64,
	}

	fn datagrams() -> Datagrams {
		let info = moq_net::datagram::Info::default().with_timescale(moq_net::Timescale::MILLI);
		let producer = broadcast().create_datagrams("audio", info).unwrap().unwrap();
		Datagrams { producer, ms: 0 }
	}

	#[bench(setup = datagrams, bytes = OPUS.len())]
	fn datagram_opus160(datagrams: &mut Datagrams) -> u64 {
		datagrams.ms += 1;
		datagrams
			.producer
			.append(Timestamp::from_millis(datagrams.ms).unwrap(), &OPUS[..])
			.unwrap()
	}
}

fn main() {
	swole::main();
}
