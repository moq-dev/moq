//! Tokio construction helpers for [`moq_net::origin`].

/// Build an origin producer and spawn its [`moq_net::origin::Driver`] on the
/// ambient tokio runtime.
///
/// The tokio shorthand for [`moq_net::origin::Producer::new`]: the driver runs
/// as its own task, finishing once every producer handle drops and the
/// remaining lifecycle work drains. Keep the pair explicit instead when you
/// want to poll the driver yourself or tear the origin down by dropping it.
///
/// Panics if called outside a tokio runtime.
///
/// Accepts a [`Config`](moq_net::origin::Config) or a bare
/// [`Hop`](moq_net::Hop) id (which uses the default config).
pub fn spawn(config: impl Into<moq_net::origin::Config>) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(config.into());
	tokio::spawn(driver.run(crate::runtime::Runtime::<()>::new()));
	producer
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The spawned driver runs the origin's lifecycle: an announced route
	/// reaches a consumer, and dropping the announcement retracts it.
	#[tokio::test]
	async fn spawn_drives_the_origin() {
		let origin = spawn(moq_net::origin::Config::new(moq_net::Hop::random()));
		let mut announced = origin.consume().announced();

		let mut broadcast = origin.create_broadcast("cam").expect("create broadcast");
		broadcast.announce(Default::default()).expect("create broadcast");

		let update = announced.next().await.expect("announce");
		assert_eq!(update.path.as_str(), "cam");
		assert!(update.kind.is_active());

		broadcast.finish();
		let update = announced.next().await.expect("retraction");
		assert!(!update.kind.is_active());
	}

	/// Outside a runtime the spawn has nowhere to run the driver: it panics
	/// rather than silently returning an origin that never makes progress.
	#[test]
	#[should_panic(expected = "no reactor running")]
	fn spawn_outside_runtime_panics() {
		let _ = spawn(moq_net::origin::Config::new(moq_net::Hop::random()));
	}
}
