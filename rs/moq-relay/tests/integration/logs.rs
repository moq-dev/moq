/// Relay log output as text, waking every waiter on each write.
#[derive(Clone, Default)]
pub struct Logs {
	text: std::sync::Arc<std::sync::Mutex<String>>,
	written: std::sync::Arc<tokio::sync::Notify>,
}

impl Logs {
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
