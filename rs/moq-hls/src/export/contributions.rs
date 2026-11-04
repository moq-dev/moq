//! Catalog-only contribution discovery for the HTTP HLS master.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::task::Poll;
#[cfg(feature = "server")]
use std::task::ready;
use std::time::Duration;

use hang::catalog::{Contribution, Media};
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
}

#[derive(Default)]
struct State {
	media: Media,
	entries: BTreeMap<String, Entry>,
}

struct Entry {
	reference: Contribution,
	current: kio::Producer<crate::Result<Option<Arc<Instance>>>>,
	_owner: Option<driver::Owner>,
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
			},
		}
	}

	pub fn sync(&self, source: &moq_mux::Source, catalog: &Catalog) {
		let Ok(mut state) = self.state.write() else { return };
		state.media = catalog.media;
		state
			.entries
			.retain(|id, entry| catalog.contributions.get(id) == Some(&entry.reference));
		for (id, reference) in &catalog.contributions {
			if state.entries.contains_key(id) || !master::is_path_segment(id) {
				continue;
			}
			let current = kio::Producer::new(Ok(None));
			let owner = match source.follow(Some(&reference.broadcast), reference.instance.as_ref()) {
				Ok(instances) => Some(self.context.spawner.spawn(follow_instances(
					instances,
					current.clone(),
					self.context.clone(),
				))),
				Err(err) => {
					*current.write().unwrap_or_else(|_| unreachable!("new entry is open")) = Err(err.into());
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

	pub fn clear(&self) {
		if let Ok(mut state) = self.state.write() {
			state.entries.clear();
		}
	}

	pub fn poll_advertised(&self, waiter: &kio::Waiter) -> Poll<()> {
		self.state
			.poll_ref(waiter, |state| {
				for entry in state.entries.values() {
					let ready = entry.current.poll_ref(waiter, |current| {
						if let Ok(Some(instance)) = &**current {
							let listed = instance.renditions.poll_advertised(waiter);
							if listed.is_ready() && compatible(state.media, instance.renditions.media()) {
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
			let Ok(Some(instance)) = &*current else { continue };
			if !compatible(state.media, instance.renditions.media()) {
				continue;
			}
			for rendition in instance.renditions.snapshot() {
				if !rendition.is_enabled() || rendition.poll_refused(&kio::Waiter::noop()).is_ready() {
					continue;
				}
				let uri = master::contribution_uri(parent, id, &instance.id, &rendition, query);
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
	pub async fn rendition(
		&self,
		at: &super::route::Contribution,
		kind: Kind,
		name: &str,
	) -> crate::Result<Option<Arc<Rendition>>> {
		kio::wait(|waiter| self.poll_rendition(waiter, at, kind, name)).await
	}

	#[cfg(feature = "server")]
	fn poll_rendition(
		&self,
		waiter: &kio::Waiter,
		at: &super::route::Contribution,
		kind: Kind,
		name: &str,
	) -> Poll<crate::Result<Option<Arc<Rendition>>>> {
		let state = self.state.read();
		let Some(entry) = state.entries.get(&at.id) else {
			return Poll::Ready(Ok(None));
		};
		match entry.current.poll_ref(waiter, |current| {
			let instance = match &**current {
				Err(err) => return Poll::Ready(Err(err.clone())),
				Ok(None) => return Poll::Ready(Ok(None)),
				Ok(Some(instance)) if instance.id != at.instance => return Poll::Ready(Ok(None)),
				Ok(Some(instance)) => instance,
			};
			let rendition = ready!(instance.renditions.poll_get(waiter, kind, name));
			Poll::Ready(Ok(
				rendition.filter(|_| compatible(state.media, instance.renditions.media()))
			))
		}) {
			Poll::Ready(Ok(result)) => Poll::Ready(result),
			Poll::Ready(Err(_)) => Poll::Ready(Ok(None)),
			Poll::Pending => Poll::Pending,
		}
	}
}

fn compatible(parent: Media, contribution: Option<Media>) -> bool {
	contribution.is_some_and(|media| {
		parent.interval().is_some() && parent.interval() == media.interval() && parent.clock == media.clock
	})
}

async fn follow_instances(
	mut instances: follow::Instances,
	current: kio::Producer<crate::Result<Option<Arc<Instance>>>>,
	context: Context,
) {
	loop {
		let next = instances.next().await;
		let next = match next {
			Ok(follow::Event::Start(broadcast)) => {
				let id = broadcast
					.info()
					.instance
					.clone()
					.unwrap_or_else(moq_net::Instance::random)
					.to_string();
				let source = moq_mux::Source::new(context.origin.clone(), broadcast);
				let renditions = renditions::Producer::lazy(context.window, context.spawner.clone());
				let owner = context
					.spawner
					.spawn(super::watch_catalog(source, renditions.clone(), None));
				Ok(Some(Arc::new(Instance {
					id,
					renditions,
					_owner: owner,
				})))
			}
			Ok(follow::Event::End) => Ok(None),
			Err(err) => Err(err.into()),
		};
		let failed = next.is_err();
		let Ok(mut state) = current.write() else { return };
		*state = next;
		if failed {
			return;
		}
	}
}
