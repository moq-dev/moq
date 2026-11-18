//! What a node's announcement reader sees, and the events it logs as that changes:
//! one implementation for every route rule, keyed by whatever names a claim's owner
//! there (a publisher under the path rule, an Instance under the relay rule).

use std::collections::{BTreeMap, BTreeSet};

use crate::Path;

use super::prefix::ancestors;
use super::{Event, Id, Kind, Shown};

/// How the reader sees each claim, and the events not yet taken.
#[derive(Clone, Debug)]
pub(crate) struct Reader<A, K = Id> {
	shown: BTreeMap<(K, Path), Shown<A>>,
	/// The identities recomputed since [`Self::changed`] last took them, each with
	/// the claims whose shown state changed.
	changed: BTreeMap<K, BTreeSet<Path>>,
	events: Vec<Event<A, K>>,
	/// Per claim this node has shown, how many times it forgot that claim.
	#[cfg(test)]
	forgot: BTreeMap<(K, Path), u32>,
}

impl<A, K> Default for Reader<A, K> {
	fn default() -> Self {
		Self {
			shown: BTreeMap::new(),
			changed: BTreeMap::new(),
			events: Vec::new(),
			#[cfg(test)]
			forgot: BTreeMap::new(),
		}
	}
}

impl<A: Clone + Eq + std::fmt::Debug, K: Clone + Ord + std::fmt::Debug> Reader<A, K> {
	pub fn shown(&self) -> &BTreeMap<(K, Path), Shown<A>> {
		&self.shown
	}

	/// How the reader sees `claim` of `id`.
	pub fn get(&self, id: K, claim: &Path) -> Option<&Shown<A>> {
		self.shown.get(&(id, claim.clone()))
	}

	/// Take the identities recomputed since the last call, each with the claims whose
	/// shown state changed.
	pub fn changed(&mut self) -> BTreeMap<K, BTreeSet<Path>> {
		std::mem::take(&mut self.changed)
	}

	/// Take the events, in order.
	pub fn events(&mut self) -> Vec<Event<A, K>> {
		std::mem::take(&mut self.events)
	}

	/// The reader now sees each claim of `id` in `now` as it says, where `watched`. Log
	/// each change. A claim no longer shown ended if `ended` says the origin withdrew it
	/// as far as this node knows, and went offline if the node let go of it instead.
	pub fn apply(
		&mut self,
		id: K,
		watched: impl Fn(&Path) -> bool,
		now: BTreeMap<Path, Option<Shown<A>>>,
		ended: impl Fn(&Path) -> bool,
	) {
		let shown = |claim: &Path| match now.get(claim) {
			Some(now) => now.as_ref(),
			None => self.get(id.clone(), claim),
		};
		let mut events = Vec::new();
		for (claim, now) in &now {
			let Some(was) = self.get(id.clone(), claim) else {
				continue;
			};
			match now {
				Some(now) => {
					if was.item != now.item {
						events.push((claim, Kind::Update, &now.item));
					}
					if was.available != now.available {
						let kind = match now.available {
							true => Kind::Available,
							false => Kind::Unavailable,
						};
						events.push((claim, kind, &now.item));
					}
				}
				None => {
					// A claim presented at a narrower region moved up to the claim it
					// stood for, which is still shown. That claim may have changed
					// meanwhile, so a presented claim needs only something shown above it.
					// A claim its origin announced there is its own, whatever is above it.
					let moved = was.presented
						&& ancestors(claim)
							.filter(|above| above != claim)
							.any(|above| shown(&above).is_some());
					let kind = if !watched(claim) {
						Kind::Unwatched
					} else if !moved && ended(claim) {
						Kind::End
					} else {
						Kind::Offline
					};
					events.push((claim, kind, &was.item));
				}
			}
		}
		for (claim, now) in &now {
			if let Some(now) = now
				&& self.get(id.clone(), claim).is_none()
			{
				events.push((claim, Kind::Start, &now.item));
			}
		}
		let events: Vec<(Path, Kind, A)> = (events.into_iter())
			.map(|(claim, kind, item)| (claim.clone(), kind, item.clone()))
			.collect();
		for (claim, kind, item) in events {
			self.log(id.clone(), claim, kind, item);
		}
		// Named as recomputed whether or not anything it shows changed, so a driver
		// serves its claims through the routes it has now.
		let changed = self.changed.entry(id.clone()).or_default();
		for (claim, now) in now {
			let key = (id.clone(), claim);
			if self.shown.get(&key) == now.as_ref() {
				continue;
			}
			changed.insert(key.1.clone());
			match now {
				Some(now) => {
					self.shown.insert(key, now);
				}
				None => {
					self.shown.remove(&key);
				}
			};
		}
	}

	fn log(&mut self, id: K, claim: Path, kind: Kind, item: A) {
		#[cfg(test)]
		let forgot = *self.forgot.entry((id.clone(), claim.clone())).or_insert(0);
		self.events.push(Event {
			id,
			claim,
			kind,
			item,
			#[cfg(test)]
			forgot,
		});
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn p(path: &str) -> Path {
		Path::new(path).expect("a path")
	}

	fn shown(presented: bool) -> Option<Shown<u64>> {
		Some(Shown {
			item: 0,
			available: true,
			presented,
		})
	}

	fn kinds(reader: &mut Reader<u64, u64>) -> Vec<(String, Kind)> {
		(reader.events().into_iter())
			.map(|event| (event.claim.to_string(), event.kind))
			.collect()
	}

	/// A claim presented at a narrower region that goes while the claim it stood for is
	/// still shown moved up to it, so it went offline there, never ended, even when its
	/// origin withdrew it. One its origin announced there ends.
	#[test]
	fn a_presented_claim_that_moved_up_goes_offline() {
		let mut reader = Reader::<u64, u64>::default();
		let now = BTreeMap::from([(p("a"), shown(false)), (p("a/b"), shown(true)), (p("c"), shown(false))]);
		reader.apply(1, |_| true, now, |_| false);
		reader.events();
		reader.apply(1, |_| true, BTreeMap::from([(p("a/b"), None)]), |_| true);
		assert_eq!(kinds(&mut reader), [("a/b".to_string(), Kind::Offline)]);
		reader.apply(1, |_| true, BTreeMap::from([(p("c"), None)]), |_| true);
		assert_eq!(kinds(&mut reader), [("c".to_string(), Kind::End)]);
	}
}
