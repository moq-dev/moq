//! Windows.Graphics.Capture notification ownership and selector validation.
//! Native API calls live separately so the stop/wakeup contract runs in host CI.

use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use moq_net::Timestamp;

use crate::Error;

#[cfg(target_os = "windows")]
mod native;
#[cfg(target_os = "windows")]
pub(super) use native::{displays, open, windows};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Event {
	Stop,
	Closed,
	Borderless,
	Frame,
	Deadline,
}

#[derive(Default)]
struct Events {
	stop: bool,
	closed: bool,
	borderless: bool,
	frame: bool,
}

impl Events {
	fn next(&mut self) -> Option<Event> {
		if self.stop {
			Some(Event::Stop)
		} else if self.closed {
			Some(Event::Closed)
		} else if std::mem::take(&mut self.borderless) {
			Some(Event::Borderless)
		} else if std::mem::take(&mut self.frame) {
			Some(Event::Frame)
		} else {
			None
		}
	}
}

#[derive(Default)]
struct Signal {
	state: Mutex<Events>,
	wake: Condvar,
}

impl Signal {
	fn notify(&self, event: Event) {
		let mut state = self.state.lock().unwrap();
		match event {
			Event::Stop => state.stop = true,
			Event::Closed => state.closed = true,
			Event::Borderless => state.borderless = true,
			Event::Frame => state.frame = true,
			Event::Deadline => unreachable!("deadlines are derived from the clock"),
		}
		drop(state);
		self.wake.notify_one();
	}

	fn wait(&self, deadline: Option<Instant>) -> Event {
		let mut state = self.state.lock().unwrap();
		loop {
			if let Some(event) = state.next() {
				return event;
			}
			state = match deadline {
				Some(deadline) => {
					let now = Instant::now();
					if now >= deadline {
						return Event::Deadline;
					}
					self.wake.wait_timeout(state, deadline - now).unwrap().0
				}
				None => self.wake.wait(state).unwrap(),
			};
		}
	}
}

// WGC can omit unchanged frames. Retain only the owned output, keeping the
// capture stream paced without holding a frame-pool slot or converting again.
struct Delivery<T> {
	frame: Option<T>,
	clock: Option<(Timestamp, Instant)>,
	interval: Duration,
	next: Instant,
}

impl<T> Delivery<T> {
	fn new(interval: Duration, now: Instant) -> Self {
		Self {
			frame: None,
			clock: None,
			interval,
			next: now,
		}
	}

	fn replace(&mut self, frame: T, timestamp: Timestamp, now: Instant) {
		self.frame = Some(frame);
		// Changed frames can arrive after a repeated frame was already delivered.
		// Keep one presentation clock instead of rewinding to their acquisition time.
		self.clock.get_or_insert((timestamp, now));
	}

	fn clear(&mut self) {
		self.frame = None;
	}

	fn deadline(&self) -> Option<Instant> {
		self.frame.as_ref().map(|_| self.next)
	}

	fn next(&mut self, now: Instant) -> Result<Option<(&T, Timestamp)>, Error> {
		if now < self.next {
			return Ok(None);
		}
		let Some(frame) = self.frame.as_ref() else {
			return Ok(None);
		};
		let (timestamp, started) = self.clock.unwrap();
		let elapsed =
			u64::try_from(now.saturating_duration_since(started).as_micros()).map_err(|_| moq_net::TimeOverflow)?;
		let timestamp = timestamp.checked_add(Timestamp::from_micros(elapsed)?)?;
		self.next = now + self.interval;
		Ok(Some((frame, timestamp)))
	}
}

fn display_index(selector: Option<&str>) -> Result<usize, Error> {
	selector.map_or(Ok(0), |selector| {
		selector
			.strip_prefix("display:")
			.unwrap_or(selector)
			.parse()
			.map_err(|_| Error::SourceUnavailable(format!("invalid Windows display selector {selector:?}")))
	})
}

fn window_handle(selector: &str) -> Result<usize, Error> {
	let value = selector
		.strip_prefix("window:")
		.unwrap_or(selector)
		.parse::<usize>()
		.map_err(|_| Error::SourceUnavailable(format!("invalid Windows window selector {selector:?}")))?;
	if value == 0 {
		return Err(Error::SourceUnavailable("a window handle cannot be null".into()));
	}
	Ok(value)
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::sync::Arc;

	#[test]
	fn unchanged_frames_keep_the_delivery_deadline_and_advance_timestamps() {
		let now = Instant::now();
		let interval = Duration::from_millis(20);
		let mut delivery = Delivery::new(interval, now);
		let pixels = Arc::new([1, 2, 3]);
		delivery.replace(pixels.clone(), Timestamp::from_micros(1_000_000).unwrap(), now);

		for step in 0..10 {
			let now = now + interval * step;
			assert_eq!(delivery.deadline(), Some(now));
			let (frame, timestamp) = delivery.next(now).unwrap().unwrap();
			assert!(
				Arc::ptr_eq(frame, &pixels),
				"reuse the owned output without a conversion"
			);
			assert_eq!(timestamp.as_micros(), 1_000_000 + u128::from(step) * 20_000);
			assert!(delivery.next(now + interval / 2).unwrap().is_none());
		}
	}

	#[test]
	fn new_frames_and_resume_keep_the_same_presentation_clock() {
		let now = Instant::now();
		let interval = Duration::from_millis(20);
		let mut delivery = Delivery::new(interval, now);
		delivery.replace(1, Timestamp::from_micros(1_000_000).unwrap(), now);
		delivery.next(now).unwrap().unwrap();
		let repeated = now + interval * 5;
		assert_eq!(delivery.next(repeated).unwrap().unwrap().1.as_micros(), 1_100_000);
		// This changed frame was acquired before the most recent repeated delivery.
		delivery.replace(2, Timestamp::from_micros(1_090_000).unwrap(), repeated);
		let (frame, timestamp) = delivery.next(repeated + interval).unwrap().unwrap();
		assert_eq!(*frame, 2);
		assert_eq!(timestamp.as_micros(), 1_120_000);

		// Minimized, empty, or resizing sources stop repeating until new content arrives.
		delivery.clear();
		assert_eq!(delivery.deadline(), None);
		let resumed = now + Duration::from_secs(1);
		assert!(delivery.next(resumed).unwrap().is_none());
		delivery.replace(3, Timestamp::from_micros(1_990_000).unwrap(), resumed);
		let (frame, timestamp) = delivery.next(resumed).unwrap().unwrap();
		assert_eq!(*frame, 3);
		assert_eq!(timestamp.as_micros(), 2_000_000);
		assert_eq!(delivery.deadline(), Some(resumed + interval));
	}

	#[tokio::test]
	#[cfg(feature = "openh264")]
	async fn unchanged_frames_reach_an_active_subscriber() {
		use crate::encode::{Config, Encoder, Kind, Producer};

		let now = Instant::now();
		let interval = Duration::from_millis(20);
		let mut delivery = Delivery::new(interval, now);
		let surface = crate::frame::I420 {
			width: 320,
			height: 240,
			data: vec![0x80; 320 * 240 * 3 / 2],
			color: None,
		};
		delivery.replace(surface, Timestamp::from_micros(0).unwrap(), now);
		let mut config = Config::new(320, 240, crate::Rate::integer(50));
		config.kind = Kind::Software;
		let mut encoder = Encoder::new(&config).unwrap();
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
		let track = broadcast
			.create_track("video", catalog.track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let subscriber = track.subscribe(None);
		let rendition = config.probe().await.unwrap();
		let container = moq_mux::catalog::hang::Container::try_from(&rendition).unwrap();
		let mut subscriber = moq_mux::container::Consumer::new(subscriber, container);
		let mut producer = Producer::with_track(track, catalog.clone(), rendition).unwrap();
		assert!(producer.demand().is_used());

		// Only one captured picture arrives; more than three intervals of paced
		// output must still reach a real video subscriber with advancing timestamps.
		for step in 0..10 {
			let now = now + interval * step;
			assert_eq!(delivery.deadline(), Some(now));
			let (surface, timestamp) = delivery.next(now).unwrap().unwrap();
			let frame = crate::Frame::new(crate::Surface::I420(surface.clone()), timestamp);
			producer.publish(&encoder.encode(&frame).unwrap()).unwrap();
			let received = subscriber.read().await.unwrap().unwrap();
			assert_eq!(received.timestamp.as_micros(), u128::from(step) * 20_000);
		}
	}

	#[test]
	fn selectors_preserve_native_ids_and_refuse_malformed_input() {
		assert_eq!(display_index(None).unwrap(), 0);
		assert_eq!(display_index(Some("display:2")).unwrap(), 2);
		assert_eq!(display_index(Some("2")).unwrap(), 2);
		assert_eq!(window_handle("window:1234").unwrap(), 1234);
		assert_eq!(window_handle("1234").unwrap(), 1234);
		for invalid in ["", "-1", "window:2", "display:"] {
			assert!(display_index(Some(invalid)).is_err());
		}
		for invalid in ["0", "window:0", "-1", "display:1"] {
			assert!(window_handle(invalid).is_err());
		}
	}

	#[test]
	fn stop_wakes_without_a_frame_or_polling_timeout() {
		let signal = Arc::new(Signal::default());
		let barrier = Arc::new(std::sync::Barrier::new(2));
		let worker = std::thread::spawn({
			let signal = signal.clone();
			let barrier = barrier.clone();
			move || {
				barrier.wait();
				signal.wait(None)
			}
		});
		barrier.wait();
		signal.notify(Event::Stop);
		assert_eq!(worker.join().unwrap(), Event::Stop);
	}

	#[test]
	fn terminal_events_win_over_queued_frames_and_access_callbacks() {
		let signal = Signal::default();
		signal.notify(Event::Frame);
		signal.notify(Event::Borderless);
		signal.notify(Event::Closed);
		assert_eq!(signal.wait(None), Event::Closed);
		signal.notify(Event::Stop);
		assert_eq!(signal.wait(None), Event::Stop);
	}

	#[test]
	fn notifications_are_retained_and_frames_coalesce() {
		let signal = Signal::default();
		signal.notify(Event::Frame);
		signal.notify(Event::Frame);
		signal.notify(Event::Borderless);
		assert_eq!(signal.wait(None), Event::Borderless);
		assert_eq!(signal.wait(None), Event::Frame);
		assert_eq!(signal.wait(Some(Instant::now())), Event::Deadline);
	}
}
