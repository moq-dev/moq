//! What a node's announcement reader sees, and the events it logs as that changes:
//! one implementation for every route rule.

use std::collections::{BTreeMap, BTreeSet};

use crate::Path;

use super::view::{ancestors, within_set};
use super::{Event, Id, Kind, Shown, within};

/// How the reader sees each claim, and the events not yet taken.
pub(crate) struct Reader<A> {
	shown: BTreeMap<(Id, Path), Shown<A>>,
	/// Claim paths indexed by publisher for scoped reader updates.
	claims: BTreeMap<Id, BTreeSet<Path>>,
	/// The identities recomputed since [`Self::changed`] last took them, each with
	/// the claims whose shown state changed.
	changed: BTreeMap<Id, BTreeSet<Path>>,
	events: Vec<Event<A>>,
	/// Per claim this node has shown, how many times it forgot that claim.
	#[cfg(test)]
	forgot: BTreeMap<(Id, Path), u32>,
}

impl<A> Default for Reader<A> {
	fn default() -> Self {
		Self {
			shown: BTreeMap::new(),
			claims: BTreeMap::new(),
			changed: BTreeMap::new(),
			events: Vec::new(),
			#[cfg(test)]
			forgot: BTreeMap::new(),
		}
	}
}

impl<A: Clone + Eq + std::fmt::Debug> Reader<A> {
	pub fn shown(&self) -> &BTreeMap<(Id, Path), Shown<A>> {
		&self.shown
	}

	/// How the reader sees `claim` of `id`.
	pub fn get(&self, id: Id, claim: &Path) -> Option<&Shown<A>> {
		self.shown.get(&(id, claim.clone()))
	}

	/// The claims of `id` the reader sees.
	pub fn claims(&self, id: Id) -> impl Iterator<Item = &Path> {
		self.claims.get(&id).into_iter().flatten()
	}

	pub fn claims_within(&self, id: Id, prefix: &Path) -> impl Iterator<Item = &Path> {
		self.claims
			.get(&id)
			.into_iter()
			.flat_map(move |claims| within_set(claims, prefix))
	}

	/// Take the identities recomputed since the last call, each with the claims whose
	/// shown state changed.
	pub fn changed(&mut self) -> BTreeMap<Id, BTreeSet<Path>> {
		std::mem::take(&mut self.changed)
	}

	/// Mark `id` recomputed, whether or not anything it shows changed.
	pub fn touch(&mut self, id: Id) {
		self.changed.entry(id).or_default();
	}

	/// Take the events, in order.
	pub fn events(&mut self) -> Vec<Event<A>> {
		std::mem::take(&mut self.events)
	}

	/// Count a forget against each claim of `id` this node has shown that `kept` does not keep.
	#[cfg_attr(not(test), allow(unused_variables, reason = "only fixtures count forgets"))]
	pub fn forget(&mut self, id: Id, kept: impl Fn(&Path) -> bool) {
		#[cfg(test)]
		for ((key, claim), count) in self.forgot.iter_mut() {
			if *key == id && !kept(claim) {
				*count += 1;
			}
		}
	}

	/// Assert that `now`, recomputed for some claims of `id`, agrees with `full`,
	/// recomputed for all of them.
	#[cfg(test)]
	pub fn check(&self, id: Id, now: &BTreeMap<Path, Option<Shown<A>>>, full: BTreeMap<Path, Option<Shown<A>>>) {
		for (claim, full) in full {
			let got = match now.get(&claim) {
				Some(got) => got.as_ref(),
				None => self.get(id, &claim),
			};
			assert_eq!(got, full.as_ref(), "incremental reader view of {id} {claim:?}");
		}
	}

	/// The reader now sees each claim of `id` in `now` as it says, inside `watch`. Log
	/// each change. A claim no longer shown ended if `ended` says the origin withdrew it
	/// as far as this node knows, and went offline if the node let go of it instead.
	pub fn apply(
		&mut self,
		id: Id,
		watch: &Path,
		now: BTreeMap<Path, Option<Shown<A>>>,
		ended: impl Fn(&Path) -> bool,
	) {
		let shown = |claim: &Path| match now.get(claim) {
			Some(now) => now.as_ref(),
			None => self.get(id, claim),
		};
		let mut events = Vec::new();
		for (claim, now) in &now {
			let Some(was) = self.get(id, claim) else {
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
					let moved = ancestors(claim)
						.filter(|above| above != claim)
						.any(|above| shown(&above).is_some_and(|now| was.presented || now.item == was.item));
					let kind = if !within(claim, watch) {
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
				&& self.get(id, claim).is_none()
			{
				events.push((claim, Kind::Start, &now.item));
			}
		}
		let events: Vec<(Path, Kind, A)> = (events.into_iter())
			.map(|(claim, kind, item)| (claim.clone(), kind, item.clone()))
			.collect();
		for (claim, kind, item) in events {
			self.log(id, claim, kind, item);
		}
		let changed = self.changed.entry(id).or_default();
		for (claim, now) in now {
			let key = (id, claim);
			if self.shown.get(&key) == now.as_ref() {
				continue;
			}
			changed.insert(key.1.clone());
			match now {
				Some(now) => {
					self.claims.entry(id).or_default().insert(key.1.clone());
					self.shown.insert(key, now);
				}
				None => {
					if let Some(claims) = self.claims.get_mut(&id) {
						claims.remove(&key.1);
						if claims.is_empty() {
							self.claims.remove(&id);
						}
					}
					self.shown.remove(&key);
				}
			};
		}
		#[cfg(test)]
		assert_eq!(
			self.claims(id).cloned().collect::<BTreeSet<_>>(),
			self.shown
				.range((id, Path::empty())..)
				.take_while(|((key, _), _)| *key == id)
				.map(|((_, claim), _)| claim.clone())
				.collect(),
			"reader claim index"
		);
	}

	fn log(&mut self, id: Id, claim: Path, kind: Kind, item: A) {
		#[cfg(test)]
		let forgot = *self.forgot.entry((id, claim.clone())).or_insert(0);
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
