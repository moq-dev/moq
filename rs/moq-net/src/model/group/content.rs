//! Weak indexed identity; delivery records and streaming readers own the bytes.
use super::*;
use std::collections::hash_map::RandomState;
use std::hash::Hash;
use std::sync::atomic::AtomicU64;

type Map<K, V> = hashbrown::HashMap<K, V, RandomState>;

fn insert<K: Eq + Hash, V>(map: &mut Map<K, V>, key: K, value: V, bill: &cache::Extra) {
	let before = map.allocation_size();
	map.insert(key, value);
	bill.add((map.allocation_size() - before) as u64);
}

fn remove<K: Eq + Hash, V>(map: &mut Map<K, V>, key: &K, bill: &cache::Extra) {
	map.remove(key);
	if map.is_empty() {
		let bytes = map.allocation_size();
		*map = Map::default();
		bill.sub(bytes as u64);
	}
}

/// Object IDs are not frame ordinals on a sparse IETF stream.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(in crate::model) enum Key {
	Frame(u64),
	Object(u64),
}

pub(super) enum Input {
	Whole(Payload),
	Stream(Option<FrameBuf>),
	Adopt(frame::Shared),
}

pub(super) struct Spec {
	pub key: Key,
	pub size: u64,
	pub inflated: Option<u64>,
}

struct Row {
	inflated: Option<u64>,
	content: frame::SharedWeak,
}

pub(in crate::model) struct Content {
	state: Mutex<Map<Key, Row>>,
	frame_charge: AtomicU64,
	bill: cache::Extra,
	plain: Mutex<Option<Arc<Plain>>>,
	views: AtomicUsize,
	index: Mutex<Option<(Arc<Index>, u64)>>,
}

impl Content {
	pub(super) fn new(charge: &cache::Charge, plain: Option<Arc<Plain>>) -> Arc<Self> {
		let bill = charge.extra();
		bill.add((2 * size_of::<usize>() + size_of::<Self>()) as u64);
		Arc::new(Self {
			state: Mutex::default(),
			frame_charge: AtomicU64::new(0),
			bill,
			plain: Mutex::new(plain),
			views: AtomicUsize::new(0),
			index: Mutex::default(),
		})
	}

	pub(super) fn key(index: usize, fields: Option<&super::super::object::Fields>, wire: bool) -> Key {
		if wire || fields.is_some() {
			Key::Object(fields.map_or(index as u64, |fields| fields.id))
		} else {
			Key::Frame(index as u64)
		}
	}

	pub(super) fn row(
		self: &Arc<Self>,
		spec: Spec,
		input: Input,
		charge: &cache::Charge,
		transferred: Option<cache::Extra>,
	) -> Result<frame::Shared> {
		let Spec { key, size, inflated } = spec;
		let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
		if let Some(row) = state.get(&key) {
			if let Some(shared) = row.content.upgrade() {
				let matches = shared.size() as u64 == size && row.inflated == inflated;
				// A temporary strong owner must never drop under this mutex: its last
				// release removes its weak record through `forget_frame`.
				drop(state);
				if !matches {
					return Err(Error::ProtocolViolation);
				}
				let payload = match input {
					Input::Whole(payload) => Some(payload),
					Input::Adopt(shared) => Some(shared.freeze()),
					Input::Stream(_) => None,
				};
				if let Some(payload) = payload {
					let delivery = shared.fork();
					let mut at = 0;
					while at < payload.len() {
						let chunk = payload.chunk_at(at);
						delivery.append(&chunk)?;
						at += chunk.len();
					}
				}
				return Ok(shared);
			}
			state.remove(&key);
		}
		// Retained bytes/frame limits remain per delivery. Combining valid disjoint
		// ranges must not introduce a stricter aggregate admission policy.
		if size > MAX_CACHE_BYTES || inflated.is_some_and(|size| size > MAX_CACHE_BYTES) {
			return Err(Error::FrameTooLarge);
		}
		let shared = match input {
			Input::Whole(payload) => frame::Shared::from_payload(payload),
			Input::Stream(Some(buf)) => buf.shared(),
			Input::Stream(None) => FrameBuf::new(size as usize).shared(),
			Input::Adopt(shared) => frame::Shared::from_payload(shared.freeze()),
		};
		if !shared.identify(self.clone(), key) {
			drop(state);
			return Err(Error::ProtocolViolation);
		}
		shared.bill_content(transferred.unwrap_or_else(|| charge.extra()));
		insert(
			&mut state,
			key,
			Row {
				inflated,
				content: shared.downgrade(),
			},
			&self.bill,
		);
		drop(state);
		Ok(shared)
	}

	pub(in crate::model) fn charge_frame(&self, bytes: u64) {
		self.frame_charge.fetch_add(bytes, Ordering::Relaxed);
	}

	pub(in crate::model) fn forget_frame(&self, key: Key, address: usize, charge: u64) {
		self.frame_charge
			.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |total| total.checked_sub(charge))
			.expect("canonical frame charge underflow");
		let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
		if state.get(&key).is_some_and(|row| row.content.address() == address) {
			remove(&mut state, &key, &self.bill);
		}
	}

	pub(super) fn attach(&self, charge: &cache::Charge) -> Option<Arc<Plain>> {
		let mut plain = self.plain.lock().unwrap_or_else(PoisonError::into_inner);
		self.views.fetch_add(1, Ordering::Relaxed);
		if plain.as_ref().is_some_and(|plain| plain.lock().released.is_some()) {
			*plain = Some(Arc::new(Plain::new(charge.extra())));
		}
		plain.clone()
	}

	pub(super) fn detach(&self, err: Error) {
		let plain = self.plain.lock().unwrap_or_else(PoisonError::into_inner);
		if self.views.fetch_sub(1, Ordering::AcqRel) == 1
			&& let Some(plain) = plain.as_ref()
		{
			plain.release(err);
		}
	}

	#[cfg(test)]
	fn frames(&self) -> Vec<frame::Shared> {
		self.state
			.lock()
			.unwrap_or_else(PoisonError::into_inner)
			.values()
			.filter_map(|row| row.content.upgrade())
			.collect()
	}

	#[cfg(test)]
	pub(in crate::model) fn cost(&self) -> (usize, usize, u64) {
		let frames = self.frames();
		let bytes = frames.iter().map(frame::Shared::written).sum();
		assert_eq!(
			frames.iter().map(frame::Shared::content_size).sum::<u64>(),
			self.frame_charge.load(Ordering::Relaxed)
		);
		(frames.len(), bytes, self.size())
	}

	pub(super) fn size(&self) -> u64 {
		self.bill.size()
			+ self
				.plain
				.lock()
				.unwrap_or_else(PoisonError::into_inner)
				.as_ref()
				.map_or(0, |plain| plain.bill.size())
			+ self.frame_charge.load(Ordering::Relaxed)
	}
}

impl Drop for Content {
	fn drop(&mut self) {
		if let Some(plain) = self.plain.get_mut().unwrap_or_else(PoisonError::into_inner)
			&& plain.lock().released.is_none()
		{
			plain.release(Error::Dropped);
		}
		if let Some((index, sequence)) = self.index.get_mut().unwrap_or_else(PoisonError::into_inner).take() {
			let mut state = index.state.lock().unwrap_or_else(PoisonError::into_inner);
			if state
				.get(&sequence)
				.is_some_and(|owner| std::ptr::eq(owner.as_ptr(), self))
			{
				remove(
					&mut state,
					&sequence,
					index.bill.get().expect("registered index is billed"),
				);
			}
		}
	}
}

/// Only live content owners occupy the index; a dead owner removes its weak entry.
/// A new publisher incarnation replaces the entry instead of inheriting old bytes.
#[derive(Default)]
pub(in crate::model) struct Contents(Mutex<Weak<Index>>);

#[derive(Default)]
struct Index {
	state: Mutex<Map<u64, Weak<Content>>>,
	bill: OnceLock<cache::Extra>,
}

impl Contents {
	pub(in crate::model) fn get(&self, sequence: u64) -> Option<Arc<Content>> {
		let index = self.0.lock().unwrap_or_else(PoisonError::into_inner).upgrade()?;
		let state = index.state.lock().unwrap_or_else(PoisonError::into_inner);
		state.get(&sequence).and_then(Weak::upgrade)
	}

	pub(in crate::model) fn remove(&self, sequence: u64) {
		let Some(index) = self.0.lock().unwrap_or_else(PoisonError::into_inner).upgrade() else {
			return;
		};
		let mut state = index.state.lock().unwrap_or_else(PoisonError::into_inner);
		remove(
			&mut state,
			&sequence,
			index.bill.get().expect("registered index is billed"),
		);
	}

	pub(in crate::model) fn insert(&self, sequence: u64, content: &Arc<Content>, charge: &cache::Charge) {
		let index = {
			let mut current = self.0.lock().unwrap_or_else(PoisonError::into_inner);
			match current.upgrade() {
				Some(index) => index,
				None => {
					let index = Arc::new(Index::default());
					*current = Arc::downgrade(&index);
					index
				}
			}
		};
		let mut state = index.state.lock().unwrap_or_else(PoisonError::into_inner);
		let bill = index.bill.get_or_init(|| {
			let bill = charge.extra();
			bill.add((2 * size_of::<usize>() + size_of::<Index>()) as u64);
			bill
		});
		insert(&mut state, sequence, Arc::downgrade(content), bill);
		drop(state);
		*content.index.lock().unwrap_or_else(PoisonError::into_inner) = Some((index, sequence));
	}
}

/// The actual weak-index/strong-frame ownership used by canonical cache accounting.
#[cfg(all(feature = "async", any(test, feature = "fuzz")))]
pub(crate) struct StorageBench {
	content: Arc<Content>,
	_owners: Vec<Vec<frame::Shared>>,
	_charge: cache::Charge,
}

#[cfg(all(feature = "async", any(test, feature = "fuzz")))]
impl StorageBench {
	pub(crate) fn new(frames: usize, views: usize) -> Self {
		let account = cache::Track::new(cache::Pool::unbounded(), Default::default(), kio::Weak::new());
		let charge = account.charge();
		let content = Content::new(&charge, None);
		let mut owners = Vec::with_capacity(frames);
		for index in 0..frames {
			owners.push(
				content
					.row(
						Spec {
							key: Key::Frame(index as u64),
							size: 64,
							inflated: None,
						},
						Input::Whole(Bytes::from(vec![0; 64]).into()),
						&charge,
						None,
					)
					.unwrap(),
			);
		}
		let held = (0..views).map(|_| owners.clone()).collect();
		Self {
			content,
			_owners: held,
			_charge: charge,
		}
	}

	pub(crate) fn size(&self) -> u64 {
		self.content.size()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn weak_indexes_remove_final_owners_without_losing_replacements() {
		let pool = cache::Pool::unbounded();
		let account = cache::Track::new(pool.clone(), Default::default(), kio::Weak::new());
		let charge = account.charge();
		let baseline = pool.used();
		let contents = Contents::default();
		let old = Content::new(&charge, None);
		contents.insert(7, &old, &charge);
		let current = Content::new(&charge, None);
		contents.insert(7, &current, &charge);
		drop(old);
		assert!(Arc::ptr_eq(&contents.get(7).unwrap(), &current));
		let mut frames = Vec::new();
		for index in 0..1024 {
			frames.push(
				current
					.row(
						Spec {
							key: Key::Frame(index),
							size: 0,
							inflated: None,
						},
						Input::Whole(Bytes::new().into()),
						&charge,
						None,
					)
					.unwrap(),
			);
		}
		assert_eq!(current.cost().0, 1024);
		// Unordered final releases exercise removal while other weak rows remain.
		for index in (0..512).rev() {
			frames.swap_remove(index);
		}
		assert_eq!(current.cost().0, 512);
		drop(frames);
		assert_eq!(current.cost().0, 0);
		assert_eq!(current.state.lock().unwrap().allocation_size(), 0);
		drop(current);
		assert!(contents.get(7).is_none());
		assert_eq!(
			pool.used(),
			baseline,
			"frame and group weak indexes own no dead history"
		);
	}
}
