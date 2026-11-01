//! Every integration test, linked into one binary.
//!
//! Each `tests/*.rs` file is its own crate that links this package and its
//! dependencies again, so add new integration tests here as a module.

mod backend;
mod broadcast;
mod close_code;
mod keylog;
mod max_age;
mod migrate;
mod receive_credit;
mod reconnect;
mod reset_secret;
mod runtime;
mod stream_code;
mod subscription_end_integrity;
mod tokens;
mod unannounce;
mod worker;

/// Wait until some subscriber wants the track.
async fn wanted(mut demand: moq_net::track::Demand) -> moq_net::Result<()> {
	while demand.wanted_changed().await?.is_none() {}
	Ok(())
}

/// Wait until no subscriber wants the track.
async fn unwanted(mut demand: moq_net::track::Demand) -> moq_net::Result<()> {
	while demand.wanted_changed().await?.is_some() {}
	Ok(())
}

/// Every crate's log lines on this thread, captured until it drops.
///
/// `traced_test` keeps only this binary's own crate, never the library ones
/// these tests inspect. A current-thread runtime runs spawned tasks on this
/// thread, so their lines are captured too.
struct Logs {
	buf: LogBuf,
	_guard: tracing::subscriber::DefaultGuard,
	_second: tracing::Dispatch,
}

impl Logs {
	fn capture() -> Self {
		let buf = LogBuf::default();
		let writer = buf.clone();
		let subscriber = tracing_subscriber::fmt()
			.with_max_level(tracing::Level::TRACE)
			.with_ansi(false)
			.with_writer(move || writer.clone())
			.finish();
		let _guard = tracing::subscriber::set_default(subscriber);
		// With one dispatcher registered, tracing caches each callsite's interest from the
		// first thread to hit it, so a parallel test without a subscriber could disable a
		// span for good. A second one leaves every thread to ask its own subscriber.
		let _second = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
		Self { buf, _guard, _second }
	}

	/// The lines captured so far.
	fn text(&self) -> String {
		String::from_utf8(self.buf.0.lock().unwrap().clone()).unwrap()
	}
}

/// The shared buffer [`Logs`] formats into.
#[derive(Clone, Default)]
struct LogBuf(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for LogBuf {
	fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
		self.0.lock().unwrap().write(buf)
	}

	fn flush(&mut self) -> std::io::Result<()> {
		Ok(())
	}
}
