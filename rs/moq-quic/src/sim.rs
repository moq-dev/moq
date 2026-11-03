//! A seeded, impaired network path: a rate-limited bottleneck queue, then random loss,
//! duplication, jitter, and reordering
//!
//! Not part of the public API. The crate's own tests drive two endpoints across a [`Link`]
//! each way, and the `sim` feature exposes it for drivers that run QUIC on kio's
//! simulator with no sockets.

use rand::{RngExt, rngs::StdRng};

use crate::{Duration, Instant};

/// A rate-limited FIFO queue, as at a bottleneck router
#[derive(Debug, Clone)]
pub struct Bottleneck {
	time_per_byte: Duration,
	/// The bytes the queue holds before it tail-drops
	buffer_size: u32,
	/// The queueing delay past which packets are tail-dropped
	max_queue: Duration,
	/// Whether packets are marked CE once the queue is half full, as a classic AQM does
	marks_ce: bool,
	/// Whether the queue is bounded in time rather than bytes, as moq-shaper's is, so a rate
	/// change keeps `max_queue`
	timed: bool,
	/// When the link finishes serving everything queued so far
	busy_until: Option<Instant>,
	pub stats: QueueStats,
	/// Each queued packet's arrival and queueing delay, once set to `Some`
	pub trace: Option<Vec<(Instant, Duration)>>,
}

impl Bottleneck {
	/// A link serving `bytes_per_second`, queueing up to `buffer_size` bytes
	pub fn new(bytes_per_second: u64, buffer_size: u32, marks_ce: bool) -> Self {
		let time_per_byte = Duration::from_nanos(1_000_000_000 / bytes_per_second);
		Self {
			time_per_byte,
			buffer_size,
			max_queue: buffer_size * time_per_byte,
			marks_ce,
			timed: false,
			busy_until: None,
			stats: QueueStats::default(),
			trace: None,
		}
	}

	/// A link serving `bytes_per_second`, dropping what would wait in its queue past `max_queue`
	/// at whatever rate it serves, as a moq-shaper rate limit does
	pub fn timed(bytes_per_second: u64, max_queue: Duration) -> Self {
		let mut bottleneck = Self::new(bytes_per_second, 0, false);
		bottleneck.max_queue = max_queue;
		bottleneck.timed = true;
		bottleneck
	}

	/// Serve at `bytes_per_second` from now on, keeping the queue's size in bytes, or in time
	/// for a [`Self::timed`] queue
	///
	/// Packets already queued still leave when they were due, as if the link stepped once they
	/// had been served.
	pub fn set_rate(&mut self, bytes_per_second: u64) {
		self.time_per_byte = Duration::from_nanos(1_000_000_000 / bytes_per_second);
		if !self.timed {
			self.max_queue = self.buffer_size * self.time_per_byte;
		}
	}

	/// Queues `bytes` arriving at `now`, returning when they leave the link and whether they
	/// were marked CE, or `None` if the full queue dropped them
	fn enqueue(&mut self, now: Instant, bytes: usize) -> Option<(Instant, bool)> {
		let busy_until = self.busy_until.unwrap_or(now).max(now);
		let delay = busy_until - now;
		if delay > self.max_queue {
			self.stats.dropped += 1;
			return None;
		}
		let service = bytes as u32 * self.time_per_byte;
		if let Some(trace) = &mut self.trace {
			trace.push((now, delay));
		}
		self.stats.queued += 1;
		self.stats.total_delay += delay;
		self.stats.max_delay = self.stats.max_delay.max(delay);
		if delay > self.max_queue / 2 {
			self.stats.congested += service;
		}

		let done = busy_until + service;
		self.busy_until = Some(done);
		let marked = self.marks_ce && done > now + self.max_queue / 2;
		self.stats.marked += marked as u64;
		Some((done, marked))
	}
}

/// One direction of the path: an optional bottleneck, then seeded loss, duplication, jitter,
/// and reordering
#[derive(Debug, Clone, Default)]
pub struct Link {
	/// Queues datagrams at a rate-limited bottleneck, if set
	pub bottleneck: Option<Bottleneck>,
	/// The probability a datagram leaving the bottleneck is lost
	pub loss: f64,
	/// The probability a datagram arrives twice
	pub duplicate: f64,
	/// Extra delay drawn uniformly up to this for each arrival, keeping arrivals in order
	pub jitter: Duration,
	/// The probability an arrival is held back by up to `reorder_delay` more, so datagrams sent
	/// after it can overtake it
	pub reorder: f64,
	/// The most a reordered arrival is held back
	pub reorder_delay: Duration,
	/// Releases in-order arrivals only at the end of each interval this long, as a cellular
	/// uplink's scheduler grants them, bunching what arrived within it
	pub grant: Option<Duration>,
	/// The latest in-order arrival, which jitter never moves another before
	last_arrival: Option<Instant>,
	/// When the first grant interval began
	grant_epoch: Option<Instant>,
}

impl Link {
	/// Sends `bytes` at `now`, returning whether the bottleneck marked them CE and when each copy
	/// that survives arrives
	pub fn transmit(
		&mut self,
		rng: &mut StdRng,
		now: Instant,
		latency: Duration,
		bytes: usize,
	) -> (bool, Vec<Instant>) {
		let (sent, marked) = match &mut self.bottleneck {
			Some(bottleneck) => match bottleneck.enqueue(now, bytes) {
				Some(dequeued) => dequeued,
				None => return (false, Vec::new()),
			},
			None => (now, false),
		};
		if rng.random_bool(self.loss) {
			return (marked, Vec::new());
		}

		let copies = 1 + rng.random_bool(self.duplicate) as usize;
		let arrivals = (0..copies)
			.map(|_| {
				let arrival = sent + latency + rng.random_range(Duration::ZERO..=self.jitter);
				if rng.random_bool(self.reorder) {
					return arrival + rng.random_range(Duration::ZERO..=self.reorder_delay);
				}
				let arrival = self.last_arrival.map_or(arrival, |last| last.max(arrival));
				let arrival = self.granted(arrival);
				self.last_arrival = Some(arrival);
				arrival
			})
			.collect();
		(marked, arrivals)
	}

	/// When the grant covering `arrival` releases it: the end of its interval
	fn granted(&mut self, arrival: Instant) -> Instant {
		let Some(grant) = self.grant else {
			return arrival;
		};
		let epoch = *self.grant_epoch.get_or_insert(arrival);
		let intervals = (arrival - epoch).as_nanos().div_ceil(grant.as_nanos());
		epoch + Duration::from_nanos((intervals * grant.as_nanos()) as u64)
	}
}

/// What a [`Bottleneck`] queue has seen
#[derive(Debug, Default, Clone)]
pub struct QueueStats {
	/// Packets tail-dropped
	pub dropped: u64,
	/// Packets marked CE
	pub marked: u64,
	/// Packets queued for delivery
	pub queued: u32,
	/// The queueing delay delivered packets met, summed
	pub total_delay: Duration,
	/// The longest queueing delay a delivered packet met
	pub max_delay: Duration,
	/// How long the link spent serving packets that queued past half full, the marking
	/// threshold, so how long the sender took to answer congestion, summed over the run
	pub congested: Duration,
}

impl QueueStats {
	/// The mean queueing delay delivered packets met
	pub fn mean_delay(&self) -> Duration {
		self.total_delay / self.queued.max(1)
	}
}
