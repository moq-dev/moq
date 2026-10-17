//! Reading typed catalog updates from a broadcast.

use std::task::{Poll, ready};

use moq_net::PathOwned;
use serde::de::DeserializeOwned;

use super::Catalog;
use crate::Result;

/// Reads typed catalog roots from an already subscribed catalog track.
pub struct Consumer<E = ()> {
	// Decoded in two steps, so a catalog needing a newer reader fails with a typed error.
	inner: moq_json::snapshot::Consumer<serde_json::Value>,
	base: PathOwned,
	_ext: std::marker::PhantomData<E>,
}

impl<E: DeserializeOwned + Default> Consumer<E> {
	/// Wrap an uncompressed catalog track subscription.
	pub fn new(track: moq_net::track::Subscriber) -> Self {
		Self::with_config(track, moq_json::snapshot::consumer::Config::default())
	}

	/// Wrap a DEFLATE-compressed catalog track subscription.
	pub fn compressed(track: moq_net::track::Subscriber) -> Self {
		let mut config = moq_json::snapshot::consumer::Config::default();
		config.compression = moq_json::Compression::Deflate;
		Self::with_config(track, config)
	}

	fn with_config(track: moq_net::track::Subscriber, config: moq_json::snapshot::consumer::Config) -> Self {
		Self {
			base: track.broadcast().path.clone(),
			inner: moq_json::snapshot::Consumer::new(track, config),
			_ext: std::marker::PhantomData,
		}
	}

	/// Poll for the next catalog update.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Catalog<E>>>> {
		let value = ready!(self.inner.poll_next(waiter)).map_err(|err| crate::Error::Json(err.to_string()))?;
		let catalog = value.map(Catalog::from_value).transpose()?;
		if let Some(catalog) = catalog.as_ref() {
			catalog.check_renditions()?;
			check_resolvable(&self.base, catalog)?;
		}
		Poll::Ready(Ok(catalog))
	}

	/// Wait for the next catalog update, or return `None` when the track ends.
	pub async fn next(&mut self) -> Result<Option<Catalog<E>>>
	where
		Catalog<E>: Unpin,
	{
		kio::wait(|waiter| self.poll_next(waiter)).await
	}
}

fn check_resolvable<E>(base: &PathOwned, catalog: &Catalog<E>) -> Result<()> {
	let video = catalog.video.renditions.iter();
	let audio = catalog.audio.renditions.iter();
	let text = catalog.text.renditions.iter();
	let json = catalog.json.tracks.iter();
	let binary = catalog.binary.tracks.iter();

	for rel in video
		.map(|(_, config)| config.broadcast.as_ref())
		.chain(audio.map(|(_, config)| config.broadcast.as_ref()))
		.chain(text.map(|(_, config)| config.broadcast.as_ref()))
		.chain(json.map(|(_, config)| config.broadcast.as_ref()))
		.chain(binary.map(|(_, config)| config.broadcast.as_ref()))
		.flatten()
	{
		if !rel.is_empty() && base.try_resolve(rel).is_none() {
			return Err(crate::Error::EscapingBroadcast(rel.to_string()));
		}
	}

	if let Some(echo) = &catalog.echo
		&& base.try_resolve(&echo.path).is_none()
	{
		return Err(crate::Error::EscapingBroadcast(echo.path.to_string()));
	}
	Ok(())
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn echo_prefix_cannot_escape_the_catalog_root() {
		let base = moq_net::PathOwned::from("room/live");
		let mut catalog = Catalog::<()>::from_str(include_str!("../../fixtures/catalog-stats-echo.json")).unwrap();
		assert!(check_resolvable(&base, &catalog).is_ok());
		let echo = catalog.echo.as_mut().unwrap();
		echo.path = moq_net::path::RelativeOwned::from("../viewers");
		assert!(check_resolvable(&base, &catalog).is_ok());
		catalog.echo.as_mut().unwrap().path = moq_net::path::RelativeOwned::from("../../viewers");
		assert!(matches!(
			check_resolvable(&base, &catalog),
			Err(crate::Error::EscapingBroadcast(_))
		));
	}

	#[tokio::test]
	async fn subscribe_validates_echo_prefix_before_yielding() {
		for (reference, accepted) in [("viewers", true), ("../../viewers", false)] {
			let mut info = moq_net::broadcast::Info::new();
			info.path = "room/live".into();
			let broadcast = info.produce();
			let track = broadcast
				.create_track(Catalog::DEFAULT_NAME, Catalog::default_track_info())
				.unwrap();
			let mut consumer = Catalog::<()>::subscribe(&broadcast.consume()).await.unwrap();
			let published = serde_json::json!({ "echo": { "path": reference } }).to_string();
			let mut group = track.append_group().unwrap();
			group.write_frame(moq_net::Timestamp::ZERO, published).unwrap();
			group.finish().unwrap();
			let result = consumer.next().await;
			if accepted {
				assert!(result.unwrap().unwrap().echo.is_some());
			} else {
				assert!(matches!(result, Err(crate::Error::EscapingBroadcast(_))));
			}
		}
	}

	#[tokio::test]
	async fn subscribe_reads_a_typed_root() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track(Catalog::DEFAULT_NAME, Catalog::default_track_info())
			.unwrap();
		let mut consumer = Catalog::<()>::subscribe(&broadcast.consume()).await.unwrap();
		let mut published = Catalog::default();
		published.audio.renditions.insert(
			"audio".into(),
			crate::catalog::AudioConfig::new(crate::catalog::AudioCodec::Opus, 48_000, 2),
		);
		let mut group = track.append_group().unwrap();
		group
			.write_frame(moq_net::Timestamp::ZERO, published.to_json().unwrap())
			.unwrap();
		group.finish().unwrap();
		assert_eq!(consumer.next().await.unwrap(), Some(published));
	}

	#[tokio::test]
	async fn a_newer_version_is_a_typed_refusal() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track(Catalog::DEFAULT_NAME, Catalog::default_track_info())
			.unwrap();
		let mut consumer = Catalog::<()>::subscribe(&broadcast.consume()).await.unwrap();
		let mut group = track.append_group().unwrap();
		group
			.write_frame(moq_net::Timestamp::ZERO, r#"{"version":5,"video":[]}"#)
			.unwrap();
		group.finish().unwrap();
		assert!(matches!(
			consumer.next().await,
			Err(crate::Error::UnsupportedVersion(5))
		));
	}
}
