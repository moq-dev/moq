//! Tokio lifecycle helpers for origins.

/// Construct an origin and spawn its driver on the current Tokio runtime.
///
/// The returned producer does not keep the driver alive after the producer and
/// its lifecycle work drain. This function panics when called outside a Tokio
/// runtime.
pub fn spawn(info: moq_net::origin::Info) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(info);
	tokio::spawn(driver);
	producer
}

#[cfg(test)]
mod tests {
	use futures::FutureExt;

	use super::*;

	#[tokio::test]
	async fn progresses_and_tears_down() {
		let origin = spawn(moq_net::origin::Info::new(moq_net::Origin::random()));
		let consumer = origin.consume();
		let mut announced = consumer.announced();
		let source = origin
			.create_broadcast("test", moq_net::broadcast::Route::new().with_announce(true))
			.unwrap();

		assert!(consumer.request_broadcast("test").await.is_ok());
		assert!(announced.next().await.unwrap().broadcast.is_some());

		drop(origin);
		drop(source);
		tokio::task::yield_now().await;
		assert!(announced.next().await.unwrap().broadcast.is_none());
		assert!(announced.next().await.is_none());
	}

	#[test]
	fn panics_without_tokio_runtime() {
		let result = std::panic::catch_unwind(|| spawn(moq_net::origin::Info::new(moq_net::Origin::random())));
		assert!(result.is_err());
	}

	#[test]
	fn driver_does_not_retain_origin() {
		let runtime = tokio::runtime::Runtime::new().unwrap();
		runtime.block_on(async {
			let origin = spawn(moq_net::origin::Info::new(moq_net::Origin::random()));
			let consumer = origin.consume();
			drop(origin);
			tokio::task::yield_now().await;
			assert!(consumer.request_broadcast("test").now_or_never().unwrap().is_err());
		});
	}
}
