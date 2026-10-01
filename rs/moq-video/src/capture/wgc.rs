//! Windows.Graphics.Capture notification ownership and selector validation.
//! Native API calls live separately so the stop/wakeup contract runs in host CI.

use std::sync::{Condvar, Mutex};
use std::time::Instant;

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
