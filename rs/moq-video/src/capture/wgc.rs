//! Windows.Graphics.Capture notification ownership and selector validation.
//! Native API calls live separately so the stop/wakeup contract runs in host CI.

use std::sync::{Condvar, Mutex};
use std::time::Duration;

use kio::clock::Instant;

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

// A source can stop producing events before its first frame, for example if
// a window is minimized just after capture starts. Startup must still finish.
struct Startup {
	deadline: Option<Instant>,
}

impl Startup {
	fn new(now: Instant) -> Self {
		Self {
			deadline: Some(now.saturating_add(Duration::from_secs(5))),
		}
	}

	fn check(&self, now: Instant) -> Result<(), Error> {
		if self.deadline.is_some_and(|deadline| now >= deadline) {
			return Err(Error::SourceUnavailable(
				"WGC did not deliver its first frame within 5 seconds".into(),
			));
		}
		Ok(())
	}

	fn delivered(&mut self) {
		self.deadline = None;
	}
}

// WGC delivers a changed frame as often as the display refreshes. The newest
// waits for its slot, so changes reach the encoder at the configured rate. A
// still source delivers nothing, and the channel repeats its last picture.
struct Pace {
	interval: Duration,
	next: Instant,
}

impl Pace {
	fn new(interval: Duration, now: Instant) -> Self {
		Self { interval, next: now }
	}

	/// When a waiting frame may go out.
	fn deadline(&self) -> Instant {
		self.next
	}

	/// Whether a frame may go out at `now`, taking the slot if so.
	fn take(&mut self, now: Instant) -> bool {
		if now < self.next {
			return false;
		}
		self.next = now.saturating_add(self.interval);
		true
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
	fn startup_without_frame_events_expires() {
		let now = Instant::now();
		let startup = Startup::new(now);
		let deadline = now.saturating_add(Duration::from_secs(5));
		assert_eq!(startup.deadline, Some(deadline));
		assert!(startup.check(deadline.saturating_sub(Duration::from_nanos(1))).is_ok());
		// Model WGC silence after minimizing between StartCapture and its first
		// frame. No callback is required to reach the failure boundary.
		assert!(matches!(startup.check(deadline), Err(Error::SourceUnavailable(_))));
	}

	#[test]
	fn startup_callbacks_without_usable_frames_do_not_extend_the_deadline() {
		let now = Instant::now();
		let startup = Startup::new(now);
		let signal = Signal::default();
		for second in 1..=5 {
			signal.notify(Event::Frame);
			signal.notify(Event::Borderless);
			assert_eq!(signal.wait(startup.deadline), Event::Borderless);
			assert_eq!(signal.wait(startup.deadline), Event::Frame);
			// Empty pools and invalid content sizes do not deliver a frame.
			assert_eq!(startup.deadline, Some(now.saturating_add(Duration::from_secs(5))));
			assert_eq!(
				startup.check(now.saturating_add(Duration::from_secs(second))).is_err(),
				second == 5
			);
		}
	}

	#[test]
	fn first_delivery_removes_the_startup_deadline() {
		let now = Instant::now();
		let mut startup = Startup::new(now);
		assert!(startup.check(now.saturating_add(Duration::from_secs(1))).is_ok());
		startup.delivered();
		assert_eq!(startup.deadline, None);
		assert!(startup.check(now.saturating_add(Duration::from_secs(10))).is_ok());
	}

	#[test]
	fn terminal_events_take_precedence_over_expired_startup() {
		let startup = Startup::new(Instant::now().saturating_sub(Duration::from_secs(5)));
		let signal = Signal::default();
		assert_eq!(signal.wait(startup.deadline), Event::Deadline);
		signal.notify(Event::Frame);
		signal.notify(Event::Borderless);
		signal.notify(Event::Closed);
		assert_eq!(signal.wait(startup.deadline), Event::Closed);
		signal.notify(Event::Stop);
		assert_eq!(signal.wait(startup.deadline), Event::Stop);
	}

	#[test]
	fn changes_go_out_at_most_once_per_interval() {
		let now = Instant::now();
		let interval = Duration::from_millis(20);
		let mut pace = Pace::new(interval, now);
		assert!(pace.take(now));
		// A display refreshing at 240 Hz offers a change every ~4 ms.
		let sent = (1..=50)
			.map(|step| now.saturating_add(Duration::from_millis(4 * step)))
			.filter(|&at| pace.take(at))
			.count();
		assert_eq!(sent, 10, "200 ms of changes at 50 fps");
		assert_eq!(pace.deadline(), now.saturating_add(Duration::from_millis(220)));
	}

	#[test]
	fn a_change_after_a_pause_goes_out_at_once() {
		let now = Instant::now();
		let interval = Duration::from_millis(20);
		let mut pace = Pace::new(interval, now);
		assert!(pace.take(now));
		let later = now.saturating_add(Duration::from_secs(3));
		assert!(pace.take(later));
		assert!(!pace.take(later.saturating_add(Duration::from_millis(19))));
		assert!(pace.take(later.saturating_add(interval)));
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
