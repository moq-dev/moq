//! Which prefixes a session asks its peer to announce.
//!
//! A client session fed into an origin asks only about the prefixes the origin's
//! readers want discovered, as [`origin::Producer`] reports them, and stops asking
//! [`LINGER`] after nothing at or beneath a prefix is wanted. A relay or a server asks
//! about everything its scope may reach, once, for the session's lifetime.
//!
//! moq-lite lets requests overlap. moq-transport forbids it, and a peer that enforces
//! that refuses an overlapping request, so a session to one runs [`Discovery::exclusive`].

use std::{
	collections::{BTreeSet, HashMap, HashSet, VecDeque},
	task::{Poll, ready},
	time::Duration,
};

use crate::{
	Path,
	model::{InterestChange, Interests, Snapshot},
	origin,
	time::{Deadline, Deadlines, Instant},
};

/// How long an announce request outlives the last reader at or beneath its prefix,
/// so a reader torn down and rebuilt reuses it.
pub(crate) const LINGER: Duration = Duration::from_secs(2);

/// What the session does with its announce requests next.
pub(crate) enum Change {
	/// Ask the peer about the prefix, holding the snapshot until the request's
	/// initial set is in.
	Open(Path, Snapshot),
	/// Stop asking about the prefix: end its request, retracting what it reported.
	Close(Path),
}

/// The announce requests a session keeps open, as a covering set of what is wanted.
///
/// A request stays open while anything at or beneath its prefix is wanted, so a
/// narrower want rides it rather than opening a request that would later have to take
/// its routes over. A narrower request already open when a broader one opens, wanted or
/// lingering, stays open until the broader one goes, so neither takes the other's routes
/// over before it has synced. An [exclusive](Self::exclusive) session closes the narrower
/// requests instead. Nothing here ever reopens a request that ended on its own, nor one a
/// broader request replaced: the next want that nothing covers does.
pub(crate) struct Discovery {
	/// `None` for a session that asks about everything once, or once the origin closed.
	interests: Option<Interests>,
	/// Never hold two requests where one covers the other.
	exclusive: bool,
	runtime: Deadlines,
	/// Decided but not yet handed to the session, in order.
	changes: VecDeque<Change>,
	/// How many wants name each prefix: overlapping scopes can name one twice.
	wanted: HashMap<Path, usize>,
	/// How many wanted prefixes sit at or beneath each prefix, so a request learns in a
	/// walk up its path whether anything beneath it is still wanted.
	beneath: HashMap<Path, usize>,
	/// The requests the session holds open, by prefix.
	requests: HashMap<Path, Request>,
	/// The requests nothing beneath wants any more, by when they close.
	lingering: BTreeSet<(Instant, Path)>,
	/// Fires for the first of `lingering`.
	timer: Deadline,
}

#[derive(Default)]
struct Request {
	/// When it closes, while nothing at or beneath its prefix is wanted.
	linger: Option<Instant>,
	/// Narrower requests nothing wants any more, kept open until this one goes.
	held: HashSet<Path>,
}

impl Discovery {
	/// Decide for a session feeding `origin`: a client on a local link asks only about
	/// what is wanted, while a server, or any session on a link between relays, asks
	/// about everything it may, so discovery never narrows what a relay learns.
	pub fn new(origin: &origin::Producer, client: bool, runtime: Deadlines) -> Self {
		let mut discovery = Self {
			interests: None,
			exclusive: false,
			timer: Deadline::new(&runtime),
			runtime,
			changes: VecDeque::new(),
			wanted: HashMap::new(),
			beneath: HashMap::new(),
			requests: HashMap::new(),
			lingering: BTreeSet::new(),
		};
		match client && !origin.relays() {
			true => discovery.interests = Some(origin.interests()),
			// The wire speaks announce interest by prefix: ask for each granted pattern's
			// literal head and let the origin's scope filter what arrives. Each snapshot is
			// taken now, so a reader never reads as synced before the request goes out.
			false => {
				for prefix in crate::model::interest_prefixes(&origin.allowed()) {
					let snapshot = origin.snapshot(&prefix);
					discovery.requests.insert(prefix.clone(), Request::default());
					discovery.changes.push_back(Change::Open(prefix, snapshot));
				}
			}
		}
		discovery
	}

	/// Never hold two requests where one covers the other: a broader request closes the
	/// narrower ones beneath it before it opens, so the session can keep it off the wire
	/// until they are. The covering set of wants and the granted heads never overlap, so
	/// nothing else does.
	pub fn exclusive(mut self) -> Self {
		self.exclusive = true;
		self
	}

	/// The next change to apply, registering `waiter` for the next one otherwise.
	pub fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Change> {
		loop {
			if let Some(change) = self.changes.pop_front() {
				return Poll::Ready(change);
			}
			if let Some(interests) = &self.interests {
				match interests.poll_next(waiter) {
					Poll::Ready(Some(change)) => {
						self.interest(change);
						continue;
					}
					// The origin closed: its readers want nothing more.
					Poll::Ready(None) => {
						self.interests = None;
						continue;
					}
					Poll::Pending => {}
				}
			}
			ready!(self.timer.poll(waiter));
			let now = self.runtime.now();
			while let Some((at, prefix)) = self.lingering.first().cloned()
				&& at <= now
			{
				self.lingering.pop_first();
				self.expire(prefix);
			}
			self.timer.set(self.lingering.first().map(|(at, _)| *at));
		}
	}

	/// The request at `prefix` ended on its own: the peer finished it.
	pub fn ended(&mut self, prefix: &Path) {
		if let Some(request) = self.requests.remove(prefix) {
			self.forget(prefix, request);
		}
	}

	fn interest(&mut self, change: InterestChange) {
		let InterestChange {
			prefix,
			wanted: want,
			snapshot,
		} = change;

		let count = self.wanted.get(&prefix).copied().unwrap_or(0);
		let count = match want {
			true => count + 1,
			false => count
				.checked_sub(1)
				.expect("an interest released more often than gained"),
		};
		match count {
			0 => self.wanted.remove(&prefix),
			count => self.wanted.insert(prefix.clone(), count),
		};
		// Another want already named the prefix, or one still does.
		if count != usize::from(want) {
			return;
		}

		for above in covering(&prefix) {
			let held = self.beneath.get(&above).copied().unwrap_or(0);
			match want {
				true => self.beneath.insert(above, held + 1),
				false if held > 1 => self.beneath.insert(above, held - 1),
				false => self.beneath.remove(&above),
			};
		}

		let mut covered = false;
		for above in covering(&prefix) {
			if !self.requests.contains_key(&above) {
				continue;
			}
			covered = true;
			match self.beneath.contains_key(&above) {
				true => self.stay(&above),
				false => self.release(above),
			}
		}

		if want && !covered {
			let snapshot = snapshot.expect("a wanted prefix comes with its snapshot");
			let request = match self.exclusive {
				true => self.replace_beneath(&prefix),
				false => self.hold_lingering(&prefix),
			};
			self.requests.insert(prefix.clone(), request);
			self.changes.push_back(Change::Open(prefix, snapshot));
		}
	}

	/// A new request at `prefix` replaces every request open beneath it, wanted or
	/// lingering, which closes first.
	fn replace_beneath(&mut self, prefix: &Path) -> Request {
		let beneath: Vec<_> = self
			.requests
			.keys()
			.filter(|other| *other != prefix && other.has_prefix(prefix))
			.cloned()
			.collect();
		for other in beneath {
			if let Some(Request { linger: Some(at), .. }) = self.requests.remove(&other) {
				self.lingering.remove(&(at, other.clone()));
			}
			self.changes.push_back(Change::Close(other));
		}
		self.timer.set(self.lingering.first().map(|(at, _)| *at));
		Request::default()
	}

	/// A new request at `prefix` holds the narrower ones still lingering beneath it, as it
	/// would had they been released after it opened, so none closes before it has synced.
	fn hold_lingering(&mut self, prefix: &Path) -> Request {
		let mut request = Request::default();
		let beneath: Vec<_> = self
			.lingering
			.iter()
			.filter(|(_, other)| other.has_prefix(prefix))
			.cloned()
			.collect();
		for (at, other) in beneath {
			self.lingering.remove(&(at, other.clone()));
			if let Some(narrower) = self.requests.get_mut(&other) {
				narrower.linger = None;
			}
			request.held.insert(other);
		}
		self.timer.set(self.lingering.first().map(|(at, _)| *at));
		request
	}

	/// Keep the request at `prefix` open: something beneath it is wanted again.
	fn stay(&mut self, prefix: &Path) {
		let Some(request) = self.requests.get_mut(prefix) else {
			return;
		};
		if let Some(at) = request.linger.take() {
			self.lingering.remove(&(at, prefix.clone()));
			self.timer.set(self.lingering.first().map(|(at, _)| *at));
		}
	}

	/// Settle a request nothing beneath it wants any more. A broader open request that
	/// is still wanted holds it until that one goes; otherwise it lingers out.
	fn release(&mut self, prefix: Path) {
		for above in covering(&prefix).into_iter().skip(1) {
			if !self.beneath.contains_key(&above) {
				continue;
			}
			if let Some(holder) = self.requests.get_mut(&above) {
				holder.held.insert(prefix);
				return;
			}
		}
		let Some(request) = self.requests.get_mut(&prefix) else {
			return;
		};
		if request.linger.is_some() {
			return;
		}
		let at = self.runtime.now() + LINGER;
		request.linger = Some(at);
		self.lingering.insert((at, prefix));
		self.timer.set(self.lingering.first().map(|(at, _)| *at));
	}

	/// Close a request that lingered out.
	fn expire(&mut self, prefix: Path) {
		let Some(request) = self.requests.remove(&prefix) else {
			return;
		};
		self.forget(&prefix, request);
		self.changes.push_back(Change::Close(prefix));
	}

	/// What a request that went held now settles on its own, unless it is wanted again.
	fn forget(&mut self, prefix: &Path, request: Request) {
		if let Some(at) = request.linger {
			self.lingering.remove(&(at, prefix.clone()));
		}
		for held in request.held {
			if self.requests.contains_key(&held) && !self.beneath.contains_key(&held) {
				self.release(held);
			}
		}
	}
}

/// `path` and every prefix above it, up to the root, narrowest first.
fn covering(path: &Path) -> Vec<Path> {
	std::iter::successors(Some(path.as_str()), |path| {
		(!path.is_empty()).then(|| path.rfind('/').map_or("", |slash| &path[..slash]))
	})
	.map(|prefix| Path::new(prefix).expect("a prefix of a path fits"))
	.collect()
}
