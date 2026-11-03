/// Relay log output as text, waking every waiter on each write.
#[derive(Clone, Default)]
pub struct Logs {
	text: std::sync::Arc<std::sync::Mutex<String>>,
	written: std::sync::Arc<tokio::sync::Notify>,
}

impl Logs {
	/// Capture this test's connection logs without installing a shared writer.
	pub fn capture(&self) -> tracing::dispatcher::DefaultGuard {
		use tracing_subscriber::layer::SubscriberExt;

		// With just one registered dispatcher, tracing-core registers new callsites
		// against the registering thread's default. An explicit process fallback
		// keeps a different test's first event from disabling this scoped subscriber.
		static FALLBACK: std::sync::Once = std::sync::Once::new();
		FALLBACK.call_once(|| {
			tracing::subscriber::set_global_default(tracing::subscriber::NoSubscriber::default())
				.expect("integration tests own the no-op process subscriber");
		});
		let subscriber = tracing_subscriber::fmt()
			.with_ansi(false)
			.with_writer(self.clone())
			.finish()
			.with(
				tracing_subscriber::filter::Targets::new().with_target("moq_relay::connection", tracing::Level::INFO),
			);
		tracing::subscriber::set_default(subscriber)
	}

	pub fn text(&self) -> String {
		self.text.lock().unwrap().clone()
	}

	/// Wait until the output contains `needle`.
	pub async fn wait_for(&self, needle: &str) {
		loop {
			// Created before the check, so a write in between still wakes it.
			let written = self.written.notified();
			if self.text.lock().unwrap().contains(needle) {
				return;
			}
			written.await;
		}
	}
}

impl std::io::Write for Logs {
	fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
		self.text.lock().unwrap().push_str(&String::from_utf8_lossy(data));
		self.written.notify_waiters();
		Ok(data.len())
	}

	fn flush(&mut self) -> std::io::Result<()> {
		Ok(())
	}
}

impl tracing_subscriber::fmt::MakeWriter<'_> for Logs {
	type Writer = Self;

	fn make_writer(&self) -> Self {
		self.clone()
	}
}

/// Registering a callsite on another test thread must not disable a scoped capture.
#[test]
fn scoped_capture_survives_another_threads_first_event() {
	fn event() {
		tracing::info!(target: "moq_relay::connection", "scoped capture probe");
	}

	let logs = Logs::default();
	let _logging = logs.capture();
	std::thread::spawn(event).join().unwrap();
	event();
	assert!(logs.text().contains("scoped capture probe"));
}

/// Scoped writers must not collect sessions from another runtime's test thread.
#[test]
fn simultaneous_captures_keep_their_own_events() {
	let ready = std::sync::Arc::new(std::sync::Barrier::new(2));
	let threads: Vec<_> = ["first", "second"]
		.into_iter()
		.map(|owner| {
			let ready = ready.clone();
			std::thread::spawn(move || {
				let logs = Logs::default();
				let _logging = logs.capture();
				ready.wait();
				tracing::info!(target: "moq_relay::connection", owner, "scoped owner");
				ready.wait();
				let text = logs.text();
				assert_eq!(text.lines().count(), 1, "{text}");
				assert!(text.contains(owner), "{text}");
			})
		})
		.collect();
	for thread in threads {
		thread.join().unwrap();
	}
}
