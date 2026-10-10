#[derive(Default)]
pub(crate) struct Dropped {
	pub(crate) count: u64,
}

impl Dropped {
	pub(crate) fn record(&mut self) {
		self.count += 1;
		if self.count == 1 {
			tracing::warn!(count = self.count, "discarding RTMP stream metadata");
		}
	}
}

impl Drop for Dropped {
	fn drop(&mut self) {
		if self.count > 1 {
			tracing::warn!(count = self.count, "RTMP metadata discard summary");
		}
	}
}
