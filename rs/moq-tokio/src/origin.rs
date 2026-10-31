//! Tokio construction helpers for [`moq_net::origin`].

/// Build an origin producer and spawn its [`moq_net::origin::Driver`] on the
/// ambient tokio runtime, else moq-tokio's fallback runtime.
///
/// The tokio shorthand for [`moq_net::origin::Producer::new`]: the driver runs
/// as its own task, finishing once every producer handle drops and the
/// remaining lifecycle work drains. Keep the pair explicit instead when you
/// want to poll the driver yourself or tear the origin down by dropping it.
///
/// A panic in the driver aborts the
/// process: every session on the origin depends on it, so serving on without it
/// would look healthy while delivering nothing.
pub fn spawn() -> moq_net::origin::Producer {
	spawn_config(moq_net::origin::Config::default())
}

/// Build and spawn an origin producer with an explicit configuration.
pub fn spawn_config(config: moq_net::origin::Config) -> moq_net::origin::Producer {
	use futures::FutureExt;

	let (producer, driver) = moq_net::origin::Producer::new(config);
	crate::runtime::spawn(async move {
		if let Err(payload) = std::panic::AssertUnwindSafe(moq_net::time::run(driver))
			.catch_unwind()
			.await
		{
			tracing::error!(
				panic = crate::panic::message(payload.as_ref()),
				"origin driver panicked"
			);
			std::process::abort();
		}
	});
	producer
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The next route and whether it is active.
	async fn next_update(announced: &mut moq_net::announce::Consumer) -> Option<(moq_net::announce::Announce, bool)> {
		match announced.next().await? {
			moq_net::announce::Event::Start(route)
			| moq_net::announce::Event::Update(route)
			| moq_net::announce::Event::Restart(route) => Some((route, true)),
			moq_net::announce::Event::End(route) => Some((route, false)),
		}
	}

	/// The spawned driver runs the origin's lifecycle: an announced route
	/// reaches a consumer, and dropping the announcement retracts it.
	#[tokio::test]
	async fn spawn_drives_the_origin() {
		let origin = spawn();
		let mut announced = origin.consume().announced();

		let broadcast = origin.create_broadcast("cam").expect("create broadcast");
		broadcast.announce(Default::default()).expect("create broadcast");

		let (update, active) = next_update(&mut announced).await.expect("announce");
		assert_eq!(update.prefix.as_str(), "cam");
		assert!(active);

		broadcast.close();
		let (_, active) = next_update(&mut announced).await.expect("retraction");
		assert!(!active);
	}
}
