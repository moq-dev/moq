use std::sync::LazyLock;

pub static HANDLE: LazyLock<tokio::runtime::Handle> = LazyLock::new(|| {
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.unwrap();
	let handle = runtime.handle().clone();

	std::thread::Builder::new()
		.name("moq-ffi".into())
		.spawn(move || {
			runtime.block_on(std::future::pending::<()>());
		})
		.expect("failed to spawn runtime thread");

	handle
});

/// Reusable close signal for cancelling async operations from sync context.
pub(crate) struct CloseSignal {
	tx: tokio::sync::watch::Sender<bool>,
	rx: tokio::sync::watch::Receiver<bool>,
}

impl CloseSignal {
	pub fn new() -> Self {
		let (tx, rx) = tokio::sync::watch::channel(false);
		Self { tx, rx }
	}

	/// Signal close. Idempotent.
	pub fn close(&self) {
		let _ = self.tx.send(true);
	}

	/// Resolves when `close()` has been called (or the signal is dropped).
	pub async fn closed(&self) {
		let mut rx = self.rx.clone();
		let _ = rx.wait_for(|v| *v).await;
	}
}
