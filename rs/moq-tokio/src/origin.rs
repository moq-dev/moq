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
pub fn spawn() -> moq_net::origin::Producer {
	spawn_config(moq_net::origin::Config::default())
}

/// Build and spawn an origin producer with an explicit configuration.
pub fn spawn_config(config: moq_net::origin::Config) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(config);
	tokio::spawn(moq_net::time::run(driver));
	producer
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The next route update, skipping the caught-up marker.
	async fn next_update(announced: &mut moq_net::announce::Consumer) -> Option<moq_net::announce::Update> {
		loop {
			match announced.next().await? {
				moq_net::announce::Event::Update(update) => return Some(update),
				moq_net::announce::Event::Live => continue,
			}
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

		let update = next_update(&mut announced).await.expect("announce");
		assert_eq!(update.prefix.as_str(), "cam");
		assert!(update.kind.is_active());

		broadcast.finish();
		let update = next_update(&mut announced).await.expect("retraction");
		assert!(!update.kind.is_active());
	}

	/// Outside a runtime the spawn has nowhere to run the driver: it panics
	/// rather than silently returning an origin that never makes progress.
	#[test]
	#[should_panic(expected = "no reactor running")]
	fn spawn_outside_runtime_panics() {
		let _ = spawn();
	}
}
