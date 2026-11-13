//! Catalog-only include discovery for the HTTP HLS master.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::task::{Poll, ready};
use std::time::Duration;

use hang::catalog::{Include, Media};
use moq_mux::{catalog::hang::Catalog, follow};

#[cfg(feature = "server")]
use super::Rendition;
use super::{Kind, driver, master, renditions};

#[derive(Clone)]
pub(super) struct Producer {
	state: kio::Producer<State>,
	context: Context,
}

#[derive(Clone)]
struct Context {
	origin: moq_net::origin::Consumer,
	window: Duration,
	spawner: driver::Spawner,
	/// The root catalog's `media`. An include is part of the root broadcast, so its
	/// renditions share the root's timeline and segment on the root's grid.
	root: kio::Producer<Media>,
}

#[derive(Default)]
struct State {
	entries: BTreeMap<String, Entry>,
}

struct Entry {
	reference: Include,
	current: kio::Producer<Current>,
	_owner: Option<driver::Owner>,
}

enum Current {
	Pending,
	Offline,
	Online(Arc<Instance>),
	Failed(
		#[cfg_attr(
			not(feature = "server"),
			allow(dead_code, reason = "only the `server` feature answers a lookup")
		)]
		crate::Error,
	),
}

struct Instance {
	id: String,
	renditions: renditions::Producer,
	_owner: driver::Owner,
}

impl Drop for Instance {
	fn drop(&mut self) {
		self.renditions.cut();
		self.renditions.clear();
	}
}

impl Producer {
	pub fn new(origin: moq_net::origin::Consumer, window: Duration, spawner: driver::Spawner) -> Self {
		Self {
			state: kio::Producer::default(),
			context: Context {
				origin,
				window,
				spawner,
				root: kio::Producer::default(),
			},
		}
	}

	pub fn sync(&self, source: &moq_mux::Source, catalog: &Catalog) {
		let Ok(mut state) = self.state.write() else { return };
		// Republished estimates leave the root media as is, so only a change wakes the children.
		if *self.context.root.read() != catalog.media
			&& let Ok(mut root) = self.context.root.write()
		{
			*root = catalog.media;
		}
		state
			.entries
			.retain(|id, entry| catalog.include.get(id) == Some(&entry.reference));
		for (id, reference) in &catalog.include {
			if state.entries.contains_key(id) || !master::is_path_segment(id) {
				continue;
			}
			let current = kio::Producer::new(Current::Pending);
			let owner = match source.follow(Some(&reference.broadcast), reference.instance.as_ref()) {
				Ok(instances) => Some(self.context.spawner.spawn(FollowInstances {
					instances,
					current: current.clone(),
					context: self.context.clone(),
				})),
				Err(err) => {
					*current.write().unwrap_or_else(|_| unreachable!("new entry is open")) =
						Current::Failed(err.into());
					None
				}
			};
			state.entries.insert(
				id.clone(),
				Entry {
					reference: reference.clone(),
					current,
					_owner: owner,
				},
			);
		}
	}

	/// Stop following every include for good. Closing the state under the same lock
	/// makes this terminal: a [`sync`](Self::sync) racing past its catalog await finds the
	/// state closed and cannot re-add entries or spawn follow tasks.
	pub fn clear(&self) {
		if let Ok(mut state) = self.state.write() {
			state.entries.clear();
			state.close();
		}
		let _ = self.context.root.close();
	}

	pub fn poll_advertised(&self, waiter: &kio::Waiter) -> Poll<()> {
		self.state
			.poll_ref(waiter, |state| {
				for entry in state.entries.values() {
					let ready = entry.current.poll_ref(waiter, |current| {
						if let Current::Online(instance) = &**current {
							// The set's changes wake this; each rendition's binding wakes it as it resolves.
							if instance.renditions.poll_advertised(waiter).is_pending() {
								return Poll::Pending;
							}
							let listable = instance.renditions.snapshot().iter().any(|rendition| {
								rendition.is_enabled()
									&& rendition.poll_resolved(waiter) == Poll::Ready(true)
									&& rendition.poll_refused(waiter).is_pending()
							});
							if listable {
								return Poll::Ready(());
							}
						}
						Poll::Pending
					});
					if matches!(ready, Poll::Ready(Ok(()))) {
						return Poll::Ready(());
					}
				}
				Poll::Pending
			})
			.map(|_| ())
	}

	pub fn variants(
		&self,
		parent: &str,
		query: Option<&str>,
	) -> (Vec<master::VideoVariant>, Vec<master::AudioVariant>) {
		let state = self.state.read();
		let mut video = Vec::new();
		let mut audio = Vec::new();
		for (id, entry) in &state.entries {
			let current = entry.current.read();
			let Current::Online(instance) = &*current else { continue };
			for rendition in instance.renditions.snapshot() {
				// A rendition whose binding is still resolving may yet be denied, so it waits; one
				// whose binding failed has no media to serve.
				let noop = kio::Waiter::noop();
				if !rendition.is_enabled()
					|| rendition.poll_resolved(&noop) != Poll::Ready(true)
					|| rendition.poll_refused(&noop).is_ready()
				{
					continue;
				}
				let uri = master::include_uri(parent, id, &instance.id, &rendition, query);
				match rendition.kind {
					Kind::Video => video.push(master::VideoVariant {
						uri,
						bandwidth: rendition.bandwidth(),
						width: rendition.width,
						height: rendition.height,
						codec: rendition.codec.clone(),
					}),
					Kind::Audio => audio.push(master::AudioVariant {
						// Qualify the ID with its catalog; the renderer resolves any remaining NAME collision.
						name: format!("{id}/{}", rendition.name),
						uri,
						bandwidth: rendition.bandwidth(),
						codec: rendition.codec.clone(),
					}),
				}
			}
		}
		(video, audio)
	}

	#[cfg(feature = "server")]
	#[kio::wrapper]
	pub fn poll_rendition(
		&self,
		waiter: &kio::Waiter,
		at: &super::route::Include,
		kind: Kind,
		name: &str,
	) -> Poll<crate::Result<Option<Arc<Rendition>>>> {
		let state = self.state.read();
		let Some(entry) = state.entries.get(&at.id) else {
			return Poll::Ready(Ok(None));
		};
		match entry.current.poll_ref(waiter, |current| {
			let instance = match &**current {
				Current::Pending => return Poll::Pending,
				Current::Failed(err) => return Poll::Ready(Err(err.clone())),
				Current::Offline => return Poll::Ready(Ok(None)),
				Current::Online(instance) if instance.id != at.instance => return Poll::Ready(Ok(None)),
				Current::Online(instance) => instance,
			};
			Poll::Ready(Ok(ready!(instance.renditions.poll_get(waiter, kind, name))))
		}) {
			Poll::Ready(Ok(result)) => Poll::Ready(result),
			Poll::Ready(Err(_)) => Poll::Ready(Ok(None)),
			Poll::Pending => Poll::Pending,
		}
	}
}

/// Follows an include's instances into its [`Current`], watching each one's catalog.
struct FollowInstances {
	instances: follow::Instances,
	current: kio::Producer<Current>,
	context: Context,
}

impl kio::Task for FollowInstances {
	type Output = ();

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		loop {
			let next = match ready!(self.instances.poll_next(waiter)) {
				Ok(Some(broadcast)) => {
					let id = broadcast
						.info()
						.instance
						.clone()
						.unwrap_or_else(moq_net::Instance::random)
						.to_string();
					let source = moq_mux::Source::new(self.context.origin.clone(), broadcast);
					let renditions = renditions::Producer::lazy(self.context.window, self.context.spawner.clone());
					let owner = self.context.spawner.lead(super::WatchCatalog::new(
						source,
						renditions.clone(),
						super::Role::Include(self.context.root.consume()),
					));
					Current::Online(Arc::new(Instance {
						id,
						renditions,
						_owner: owner,
					}))
				}
				Ok(None) => Current::Offline,
				// An authorization refusal is final: the include stays out and its URLs answer 403.
				Err(err @ moq_mux::Error::Moq(moq_net::Error::Unauthorized)) => {
					tracing::debug!(%err, "include refused");
					if let Ok(mut state) = self.current.write() {
						*state = Current::Failed(err.into());
					}
					return Poll::Ready(());
				}
				Err(moq_mux::Error::Moq(moq_net::Error::Closed)) => {
					if let Ok(mut state) = self.current.write() {
						*state = Current::Offline;
					}
					return Poll::Ready(());
				}
				// Not routed yet, or refused otherwise: wait for the path's next announcement, which
				// the follower hands out, however long it takes. Until then nothing is listed.
				Err(err) => {
					tracing::debug!(%err, "include not routed; waiting for its next announcement");
					match &*self.current.read() {
						Current::Pending => Current::Offline,
						_ => continue,
					}
				}
			};
			let Ok(mut state) = self.current.write() else {
				return Poll::Ready(());
			};
			*state = next;
		}
	}
}

#[cfg(all(test, feature = "server"))]
mod tests {
	use super::*;

	#[tokio::test(start_paused = true)]
	async fn a_sync_after_clear_adds_nothing() {
		let origin = moq_tokio::origin::spawn();
		let _parent = origin.route("room/live", Default::default()).unwrap();
		let source = crate::resolved(&origin, "room/live").await;
		let mut catalog = Catalog::default();
		catalog.include.insert(
			"child".into(),
			Include::new(moq_net::path::Relative::new("./child").unwrap()),
		);
		let (spawner, _driver) = driver::Spawner::new();
		let include = Producer::new(origin.consume(), Duration::from_secs(16), spawner);
		include.clear();
		// A watcher already past its catalog await syncs after the clear.
		include.sync(&source, &catalog);
		assert!(include.state.read().entries.is_empty());
	}

	#[tokio::test(start_paused = true)]
	async fn a_cold_lookup_waits_for_the_first_follow_event() {
		let origin = moq_tokio::origin::spawn();
		let _parent = origin.route("room/live", Default::default()).unwrap();
		let child_id = moq_net::Instance::random();
		let child = origin
			.route(
				"room/child",
				moq_net::origin::RouteOptions::default().with_instance(child_id.clone(), 0),
			)
			.unwrap();
		let source = crate::resolved(&origin, "room/live").await;
		let mut catalog = Catalog::default();
		catalog.media.join = hang::catalog::Join::new(Duration::from_secs(2));
		let mut video = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
		video.coded_width = Some(320);
		video.coded_height = Some(240);
		catalog.video.renditions.insert("v".into(), video);
		let track = child
			.broadcast()
			.create_track(hang::Catalog::DEFAULT_NAME, hang::Catalog::default_track_info())
			.unwrap();
		let mut json = moq_json::snapshot::Producer::new(track, Default::default());
		json.update(moq_json::Timed::from(&catalog).at(moq_net::Timestamp::ZERO))
			.unwrap();
		catalog.video.renditions.clear();
		catalog.include.insert(
			"child".into(),
			Include::new(moq_net::path::Relative::new("./child").unwrap()),
		);
		let (spawner, driver) = driver::Spawner::new();
		let include = Producer::new(origin.consume(), Duration::from_secs(16), spawner);
		include.sync(&source, &catalog);
		let at = super::super::Include {
			id: "child".into(),
			instance: child_id.to_string(),
		};
		let mut lookup = Box::pin(include.rendition(&at, Kind::Video, "v"));
		// The catalog has been read, but the driver has not yet polled its follow task.
		assert!(futures::poll!(lookup.as_mut()).is_pending());
		let running = tokio::spawn(driver.run());
		let rendition = lookup.await.unwrap().unwrap();
		assert_eq!(rendition.name, "v");
		drop((rendition, include));
		running.await.unwrap();
	}
}
