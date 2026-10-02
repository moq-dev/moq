//! Forward the serving route's copy of a track into the logical track an origin front
//! serves.
//!
//! A front serves each track through one route at a time and switches when that route
//! dies, withdraws, or is beaten. Readers never see the switch because they read a plain
//! logical track that outlives every route, and a [`Pump`] is that track's only writer.
//! It reads whichever copy the front feeds it and writes each group and frame into the
//! logical track by index, skipping what is already there.
//!
//! So a switch needs no seam bookkeeping. The replacement is asked to start where the
//! logical track stops ([`track::TrackWeak::resume_floor`]). A group the old route left
//! open, even mid-frame, stays open and is continued in place by the replacement's copy.
//! Anything delivered twice (an older peer resending a whole group, a late frame from the
//! route being replaced) is dropped by index, and since a name always means the same
//! content, the duplicate is byte-for-byte what is already cached.

use std::collections::{BTreeMap, btree_map};
use std::task::Poll;

use crate::{Error, Result, StreamError, frame, group, track};

use super::subscription::{Position, Subscription, max_some};

/// How many groups the routes may leave unfinished before the oldest is given up. Each
/// one waits for a replacement route to continue it, so this bounds what a churning set
/// of routes can pin.
const MAX_ORPHANS: usize = 4;

/// What the front tells its pump to do.
pub(crate) enum Command {
	/// Read this subscription from now on, replacing the current input. Sent through
	/// [`Handle::feed`], which numbers it.
	Feed(u64, Feed),
	/// Nobody reads the track: drop the input, cancelling its upstream subscription.
	/// What it delivered stays cached but parked: a returning reader gets it back only
	/// once the next input's start shows it is not stale.
	Detach,
	/// The track ended cleanly.
	Finish,
	/// The track failed for good.
	Abort(Error),
}

/// A route's copy of the track, subscribed from where the logical track stops.
pub(crate) struct Feed {
	/// The copy itself, for fetches and the start it declares.
	pub copy: track::Consumer,
	/// What the copy said the track is, checked by the front against earlier copies.
	/// `None` while the copy has yet to say, for a front that ended before it did.
	pub info: Option<track::Info>,
	/// A subscription to `copy`, starting at `floor`, made by the front when it asked the
	/// route for the track so the start rides the initial request. `None` when nobody
	/// subscribed yet (a fetch alone asked): the pump subscribes once someone does.
	pub sub: Option<kio::Pending<track::Subscribing>>,
	/// Where the subscription was asked to start. Demand updates never ask for less,
	/// so a reader at the live edge cannot pull the replacement past what it owes.
	pub floor: Option<Position>,
}

/// An input that ran out, once everything it delivered has been written.
#[derive(Clone, Debug)]
pub(crate) struct Ended {
	/// The feed, as [`Handle::feed`] numbered it.
	pub id: u64,
	/// `Ok` for a clean end of the track, otherwise why the copy failed.
	pub result: Result<()>,
	/// Whether the input wrote anything into the logical track.
	pub delivered: bool,
}

/// What the pump reports back to the front.
#[derive(Default)]
pub(crate) struct Status {
	/// The newest input to run out; see [`Ended`].
	pub ended: Option<Ended>,
}

/// The front's side of a pump: commands in, status out.
///
/// Dropping it concludes the pump: no more inputs. It drains the one it has, ending
/// the logical track the way that input ends, so readers mid-track carry on after
/// the front itself is gone.
pub(crate) struct Handle {
	commands: kio::Queue<Command>,
	status: kio::Consumer<Status>,
	weak: track::TrackWeak,
	/// The id of the next feed: the same source can be fed again, so its id would not
	/// tell one feed's end from the next.
	feeds: u64,
	/// Detached since the last feed: the cache is parked; see [`Command::Detach`].
	parked: bool,
}

impl Handle {
	/// Queue a command. A pump that has exited ignores it.
	pub(crate) fn send(&self, command: Command) {
		let _ = self.commands.try_push(command);
	}

	/// Feed the pump a new input, returning the id its [`Ended`] will carry.
	pub(crate) fn feed(&mut self, feed: Feed) -> u64 {
		let id = self.feeds;
		self.feeds += 1;
		self.parked = false;
		self.send(Command::Feed(id, feed));
		id
	}

	/// Stop reading for a track nobody reads; see [`Command::Detach`].
	pub(crate) fn detach(&mut self) {
		self.parked = true;
		self.send(Command::Detach);
	}

	/// Where the next route feeding the track should start.
	pub(crate) fn resume_floor(&self) -> Option<Position> {
		match self.parked {
			true => self.weak.park_floor(),
			false => self.weak.resume_floor(),
		}
	}

	/// What the track's readers want, aggregated.
	pub(crate) fn subscription(&self) -> Option<Subscription> {
		self.weak.subscription()
	}

	/// Whether anyone reads the track.
	pub(crate) fn is_used(&self) -> bool {
		self.weak.is_used()
	}

	/// Poll for a reader arriving.
	pub(crate) fn poll_used(&self, waiter: &kio::Waiter) -> Poll<()> {
		self.weak.poll_used(waiter);
		match self.weak.is_used() {
			true => Poll::Ready(()),
			false => Poll::Pending,
		}
	}

	/// Poll for the last reader leaving.
	pub(crate) fn poll_unused(&self, waiter: &kio::Waiter) -> Poll<()> {
		self.weak.poll_unused(waiter);
		match self.weak.is_used() {
			true => Poll::Pending,
			false => Poll::Ready(()),
		}
	}

	/// End the track unless a reader holds it; see [`track::TrackWeak::abort_unused`].
	pub(crate) fn abort_unused(&self, err: Error) -> bool {
		self.weak.abort_unused(err)
	}

	/// Poll for input `id` running out, once its delivery has been written.
	pub(crate) fn poll_ended(&self, id: u64, waiter: &kio::Waiter) -> Poll<Ended> {
		match self.status.poll(waiter, |status| match &status.ended {
			Some(ended) if ended.id == id => Poll::Ready(ended.clone()),
			_ => Poll::Pending,
		}) {
			Poll::Ready(Ok(ended)) => Poll::Ready(ended),
			// The pump exited; nothing more will be written.
			Poll::Ready(Err(_)) => Poll::Ready(Ended {
				id,
				result: Err(Error::Dropped),
				delivered: false,
			}),
			Poll::Pending => Poll::Pending,
		}
	}
}

impl Drop for Handle {
	fn drop(&mut self) {
		self.commands.close();
	}
}

/// The logical track: pending until the first input says what it is.
enum Logical {
	Pending(track::Request),
	Live(track::Producer),
	/// Finished or aborted: nothing more is written, but the track stays readable for as
	/// long as the front keeps the pump.
	Done(Option<track::Producer>),
}

impl Logical {
	fn producer(&mut self) -> Option<&mut track::Producer> {
		match self {
			Self::Live(producer) => Some(producer),
			_ => None,
		}
	}

	fn subscription(&self) -> Option<Subscription> {
		match self {
			Self::Pending(request) => request.subscription(),
			Self::Live(producer) => producer.subscription(),
			Self::Done(_) => None,
		}
	}
}

/// The subscription being read.
enum Sub {
	/// Nobody subscribed to the logical track yet.
	None,
	/// Waiting on the copy's info.
	Pending(kio::Pending<track::Subscribing>),
	Ready(track::Subscriber),
}

/// The subscription being read.
struct Input {
	id: u64,
	copy: track::Consumer,
	sub: Sub,
	floor: Option<Position>,
	/// The copy ran out: `Ok` for a clean end. Reported once its groups drained.
	end: Option<Result<()>>,
	/// The copy's live edge reached its declared end. Groups below it may still be in
	/// flight, so the input only ends once the copy closes.
	complete: bool,
	/// Whether anything this input delivered was written.
	delivered: bool,
}

/// A frame being written into the logical track.
struct Partial {
	frame: frame::ProducerOwned,
	/// Its index in the group.
	index: u64,
}

/// An input's copy of a group, and the frame of it being read.
struct Source {
	input: u64,
	group: group::Consumer,
	/// The frame being copied, and how many of its leading bytes the logical frame
	/// already holds (a route died partway through it).
	frame: Option<(frame::Consumer, usize)>,
}

/// A logical group still being written.
struct Open {
	dst: group::Producer,
	/// Fetched for a reader rather than delivered live: once a reader has picked it up,
	/// nobody wanting it any more cancels it, all the way upstream.
	fetched: bool,
	/// A fetch of the rest of a fetched group whose copy failed partway: nothing live
	/// will ever continue it, so it is asked for again from where it stopped.
	refetch: Option<(u64, kio::Pending<track::Fetching>)>,
	/// The in-flight frame, kept across inputs so a replacement can finish it.
	partial: Option<Partial>,
	/// The copy being read, or `None` while waiting for an input to continue it.
	src: Option<Source>,
}

impl Open {
	/// The index of the first frame the logical group does not have in full.
	fn next(&self) -> u64 {
		match &self.partial {
			Some(partial) => partial.index,
			None => self.dst.frame_count() as u64,
		}
	}
}

/// A fetch of a group the logical track does not cache, forwarded to the input.
struct Fetch {
	request: group::Request,
	/// The input it was forwarded to, and the fetch in flight there.
	pending: Option<(u64, kio::Pending<track::Fetching>)>,
}

/// What forwarding one group came to.
enum Step {
	/// Keep the group open.
	Open,
	/// The group is done (finished or given up): stop tracking it.
	Close,
}

/// The only writer of a front's logical track; see the module docs.
pub(crate) struct Pump {
	logical: Logical,
	/// Serves fetches of groups the logical track does not cache.
	dynamic: track::Dynamic,
	commands: kio::Queue<Command>,
	status: kio::Producer<Status>,
	input: Option<Input>,
	groups: BTreeMap<u64, Open>,
	fetches: Vec<Fetch>,
	budget: frame::Budget,
	/// The aggregate demand last forwarded to the input.
	demand: Option<Subscription>,
	/// The front let go: drain the input, then end the track as it ends.
	concluding: bool,
	/// The cache is parked until the next input resolves its start.
	parked: bool,
	/// Held while a fetched group is still being written: a fetch in progress reads the
	/// track, so the front keeps an input for it.
	fetching: Option<track::Consumer>,
}

impl Pump {
	/// A pump serving `request`, and the front's handle on it.
	pub(crate) fn new(request: track::Request) -> (Self, Handle) {
		let commands = kio::Queue::new();
		let status = kio::Producer::<Status>::default();
		let handle = Handle {
			commands: commands.clone(),
			status: status.consume(),
			weak: request.weak(),
			feeds: 0,
			parked: false,
		};
		let pump = Self {
			dynamic: request.dynamic(),
			logical: Logical::Pending(request),
			commands,
			status,
			input: None,
			groups: BTreeMap::new(),
			fetches: Vec::new(),
			budget: frame::Budget::default(),
			demand: None,
			concluding: false,
			parked: false,
			fetching: None,
		};
		(pump, handle)
	}

	/// Run until the front drops its [`Handle`].
	pub(crate) async fn run(mut self) {
		kio::wait(|waiter| self.poll(waiter)).await
	}

	/// Do every piece of work available, then park on everything that could bring more.
	/// Ready once the front let go.
	pub(crate) fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		loop {
			if !self.concluding {
				match self.commands.poll_pop(waiter) {
					Poll::Ready(Ok(command)) => {
						self.apply(command);
						continue;
					}
					Poll::Ready(Err(kio::Closed)) => {
						self.concluding = true;
						continue;
					}
					Poll::Pending => {}
				}
			}
			// The front forgot the track (it went unread): nothing is written anymore.
			if let Logical::Live(producer) = &self.logical
				&& producer.poll_closed(waiter).is_ready()
			{
				self.detach();
				self.logical = Logical::Done(None);
			}
			if self.concluding && self.conclude() {
				return Poll::Ready(());
			}

			// Demand first: a fresh input reads with what the readers want, not its
			// initial guess.
			let mut progress = self.poll_demand(waiter);
			progress |= self.poll_input(waiter);
			progress |= self.poll_groups(waiter);
			progress |= self.poll_fetches(waiter);
			self.prune_orphans(waiter);
			self.report();
			let fetching = self.groups.values().any(|open| open.fetched);
			match (fetching, &self.logical) {
				(true, Logical::Live(producer)) if self.fetching.is_none() => self.fetching = Some(producer.consume()),
				(false, _) => self.fetching = None,
				_ => {}
			}
			if !progress {
				return Poll::Pending;
			}
		}
	}

	fn apply(&mut self, command: Command) {
		match command {
			Command::Feed(id, feed) => self.feed(id, feed),
			Command::Detach => {
				self.detach();
				if let Some(producer) = self.logical.producer() {
					let _ = producer.park_cache();
					self.parked = true;
				}
			}
			Command::Finish => self.finish(),
			Command::Abort(err) => self.abort(err),
		}
	}

	fn finish(&mut self) {
		self.detach();
		self.give_up_orphans(|_| true, Error::Closed);
		self.logical = Logical::Done(match std::mem::replace(&mut self.logical, Logical::Done(None)) {
			Logical::Live(producer) => {
				// Declared at the live edge unless the copy already declared its own, then
				// sealed: a group below the end that never arrived is not coming now.
				let _ = producer.finish();
				let _ = producer.clone().close();
				Some(producer)
			}
			// Finished before any route served it: nothing to read.
			Logical::Pending(request) => {
				request.reject(Error::NotFound);
				None
			}
			Logical::Done(producer) => producer,
		});
	}

	fn abort(&mut self, err: Error) {
		self.detach();
		self.give_up_orphans(|_| true, err.clone());
		for fetch in self.fetches.drain(..) {
			fetch.request.reject(err.clone());
		}
		match std::mem::replace(&mut self.logical, Logical::Done(None)) {
			Logical::Live(producer) => {
				let _ = producer.abort(err);
			}
			Logical::Pending(request) => request.reject(err),
			Logical::Done(producer) => self.logical = Logical::Done(producer),
		}
	}

	/// With the front gone, end the track once the input does. True when there is
	/// nothing left to do.
	fn conclude(&mut self) -> bool {
		if let Logical::Done(_) = self.logical {
			return true;
		}
		match &self.input {
			// Nothing in flight: nothing more will ever be written.
			None => {
				self.abort(Error::Dropped);
				true
			}
			Some(input) => match &input.end {
				Some(_) if self.draining(input.id) => false,
				Some(Ok(())) => {
					self.finish();
					true
				}
				Some(Err(err)) => {
					let err = err.clone();
					self.abort(err);
					true
				}
				None => false,
			},
		}
	}

	/// Whether groups from input `id` are still being written.
	fn draining(&self, id: u64) -> bool {
		self.groups
			.values()
			.any(|open| open.src.as_ref().is_some_and(|src| src.input == id))
	}

	fn feed(&mut self, id: u64, feed: Feed) {
		self.detach();
		if let Logical::Done(_) = self.logical {
			return;
		}
		if let Some(info) = &feed.info {
			self.accept(info);
		}
		self.demand = None;
		self.input = Some(Input {
			id,
			copy: feed.copy,
			sub: match feed.sub {
				Some(sub) => Sub::Pending(sub),
				None => Sub::None,
			},
			floor: feed.floor,
			end: None,
			complete: false,
			delivered: false,
		});
	}

	/// The first copy says what the logical track is. The front checked every later one
	/// against it.
	fn accept(&mut self, info: &track::Info) {
		if let Logical::Pending(_) = &self.logical {
			let Logical::Pending(request) = std::mem::replace(&mut self.logical, Logical::Done(None)) else {
				unreachable!()
			};
			self.logical = Logical::Live(request.accept(info.clone()));
		}
	}

	/// Drop the input; its groups wait for the next one.
	fn detach(&mut self) {
		if self.input.take().is_none() {
			return;
		}
		for open in self.groups.values_mut() {
			open.src = None;
		}
			}

	/// Read new groups and datagrams off the input.
	fn poll_input(&mut self, waiter: &kio::Waiter) -> bool {
		let Some(input) = self.input.as_mut() else {
			return false;
		};
		if input.end.is_some() {
			return false;
		}
		let mut progress = false;

		// The copy's end is the logical track's, declared as soon as it is known so
		// readers see it on time; a group below it can still be written.
		if let Some(fin) = input.copy.final_sequence()
			&& let Logical::Live(producer) = &mut self.logical
			&& producer.final_sequence().is_none()
		{
			let _ = producer.finish_at(fin);
		}

		match &input.sub {
			// Nothing to read, but the copy's end still has to reach fetching readers.
			Sub::None => {
				let _ = input.copy.poll_complete(waiter);
				return false;
			}
			Sub::Pending(pending) => match pending.poll_ok(waiter) {
				Poll::Pending => return false,
				Poll::Ready(Err(err)) => {
					input.end = Some(Err(err));
					return true;
				}
				Poll::Ready(Ok(sub)) => {
					let info = sub.info().clone();
					input.sub = Sub::Ready(sub);
					self.accept(&info);
					progress = true;
				}
			},
			Sub::Ready(_) => {}
		}
		let Some(input) = self.input.as_mut() else {
			return progress;
		};
		// A parked cache waits on where this input starts, and so do its groups:
		// revealed, the cache comes back ahead of them.
		if self.parked {
			let Poll::Ready(start) = input.copy.poll_start(waiter) else {
				return progress;
			};
			if let Some(producer) = self.logical.producer() {
				let _ = producer.unpark_cache(start);
			}
			self.parked = false;
			progress = true;
		}
		let Some(input) = self.input.as_mut() else {
			return progress;
		};
		let Sub::Ready(sub) = &mut input.sub else {
			unreachable!("resolved above")
		};

		while let Poll::Ready(res) = sub.poll_recv_datagram(waiter) {
			let Ok(Some(datagram)) = res else { break };
			if let Some(producer) = self.logical.producer() {
				let _ = producer.insert_datagram(datagram.sequence, datagram.timestamp, datagram.payload);
			}
			progress = true;
		}

		loop {
			let input = self.input.as_mut().expect("checked above");
			let Sub::Ready(sub) = &mut input.sub else {
				unreachable!("resolved above")
			};
			let group = match sub.poll_recv_group(waiter) {
				Poll::Ready(Ok(Some(group))) => group,
				Poll::Ready(Ok(None)) => {
					// Settled once the copy closes: a group still owed below the end
					// arrives in the meantime.
					input.complete = true;
					if input.copy.poll_closed(waiter).is_ready() {
						input.end = Some(Ok(()));
						return true;
					}
					return progress;
				}
				Poll::Ready(Err(err)) => {
					input.end = Some(Err(err));
					return true;
				}
				Poll::Pending => return progress,
			};
			progress = true;
			let id = input.id;
			self.attach(id, group);
		}
	}

	/// Start writing an input's copy of a group, continuing the logical group if it is
	/// already open.
	fn attach(&mut self, input: u64, group: group::Consumer) {
		let sequence = group.sequence;
		let open = match self.groups.entry(sequence) {
			btree_map::Entry::Occupied(open) => open.into_mut(),
			btree_map::Entry::Vacant(vacant) => {
				let Some(producer) = self.logical.producer() else { return };
				// A group the logical track already has in full (or still caches from an
				// earlier route) is a duplicate.
				let Ok(dst) = producer.create_group(group::Info { sequence }) else {
					return;
				};
				vacant.insert(Open {
					dst,
					fetched: false,
					refetch: None,
					partial: None,
					src: None,
				})
			}
		};
		open.src = Some(Source {
			input,
			group,
			frame: None,
		});
	}

	/// Copy whatever the open groups' sources have.
	fn poll_groups(&mut self, waiter: &kio::Waiter) -> bool {
		let mut progress = false;
		let mut closed = Vec::new();
		let mut delivered = false;
		for (sequence, open) in self.groups.iter_mut() {
			// A fetched group nobody reads any more is cut short, never cached as whole.
			if open.fetched
				&& open.dst.poll_handed_out(waiter).is_ready()
				&& open.dst.poll_unused(waiter).is_ready()
			{
				let _ = open.dst.clone().abort(Error::Cancel);
				closed.push(*sequence);
				progress = true;
				continue;
			}
			if open.fetched && open.src.is_none() {
				match Self::poll_refetch(open, &self.input, waiter) {
					Step::Open if open.src.is_some() => progress = true,
					Step::Open => continue,
					Step::Close => {
						closed.push(*sequence);
						progress = true;
						continue;
					}
				}
			}
			if open.src.is_none() {
				continue;
			}
			let before = open.next();
			let step = Self::poll_forward(open, &self.budget, &self.input, waiter);
			if open.next() != before {
				delivered = true;
				progress = true;
			}
			if let Step::Close = step {
				closed.push(*sequence);
				progress = true;
			}
		}
		for sequence in closed {
			self.groups.remove(&sequence);
		}
		if delivered && let Some(input) = self.input.as_mut() {
			input.delivered = true;
		}
		if progress {
					}
		progress
	}

	/// Copy frames from `open`'s source into the logical group until the source has
	/// nothing more for now.
	fn poll_forward(
		open: &mut Open,
		budget: &frame::Budget,
		input: &Option<Input>,
		waiter: &kio::Waiter,
	) -> Step {
		loop {
			let next = open.next();
			let src = open.src.as_mut().expect("polled with a source");

			if src.frame.is_none() {
				// Position the copy at the first frame the logical group still needs.
				src.group.start_at(next);
				if src.group.index() != next {
					// The copy starts past what the group needs, so it can't continue it.
					// A group nothing was written to yet just starts there instead.
					if open.partial.is_none() && open.dst.frame_count() == 0 {
						let _ = open.dst.start_at(src.group.index());
						continue;
					}
					open.src = None;
					return Step::Open;
				}

				match src.group.poll_next_frame(waiter) {
					Poll::Pending => return Step::Open,
					Poll::Ready(Ok(Some(frame))) => {
						let skip = match &open.partial {
							Some(partial) if partial.frame.size == frame.size => partial.frame.written(),
							// The same frame, sized differently: the routes disagree on the
							// content, so neither can be trusted to finish it.
							Some(_) => {
								tracing::warn!(group = open.dst.sequence, frame = next, "routes disagree on a frame");
								let _ = open.dst.clone().abort(Error::ProtocolViolation);
								return Step::Close;
							}
							None => {
								let info = frame::Info {
									size: frame.size,
									timestamp: frame.timestamp,
								};
								match open.dst.create_frame_owned(info, budget) {
									Ok(owned) => open.partial = Some(Partial { frame: owned, index: next }),
									Err(_) => return Step::Close,
								}
								0
							}
						};
						src.frame = Some((frame, skip));
					}
					Poll::Ready(Ok(None)) => {
						// The copy ended where the logical group does: so does the group.
						if open.partial.is_none() && src.group.frame_count() as u64 == next {
							let _ = open.dst.finish();
							return Step::Close;
						}
						// It ended below what the logical group already holds.
						tracing::warn!(group = open.dst.sequence, frame = next, "routes disagree on a group's length");
						let _ = open.dst.clone().abort(Error::ProtocolViolation);
						return Step::Close;
					}
					Poll::Ready(Err(err)) => return Self::lost(open, err, input),
				}
			}

			let (frame, skip) = src.frame.as_mut().expect("set above");
			let partial = open.partial.as_mut().expect("a frame is open whenever one is read");
			match frame.poll_read_chunk(waiter) {
				Poll::Pending => return Step::Open,
				Poll::Ready(Ok(Some(mut chunk))) => {
					// Bytes the logical frame already holds, from a route that died
					// partway through it.
					let dup = (*skip).min(chunk.len());
					*skip -= dup;
					let chunk = chunk.split_off(dup);
					if !chunk.is_empty() {
						if partial.frame.write(chunk).is_err() {
							return Step::Close;
						}
						partial.frame.notify();
					}
				}
				Poll::Ready(Ok(None)) => {
					let partial = open.partial.take().expect("checked above");
					src.frame = None;
					if partial.frame.finish().is_err() {
						return Step::Close;
					}
				}
				Poll::Ready(Err(err)) => return Self::lost(open, err, input),
			}
		}
	}

	/// Ask the input for the rest of a fetched group whose copy failed partway, and read
	/// it once it answers. A refusal is the group's end: it is lost.
	fn poll_refetch(open: &mut Open, input: &Option<Input>, waiter: &kio::Waiter) -> Step {
		let Some(input) = input else {
			return Step::Open;
		};
		if open.refetch.as_ref().is_none_or(|(id, _)| *id != input.id) {
			let options = group::Fetch::default().with_frame_start(open.next());
			open.refetch = Some((input.id, input.copy.fetch_group(open.dst.sequence, options)));
		}
		let (id, pending) = open.refetch.as_ref().expect("issued above");
		match kio::Pollable::poll(&**pending, waiter) {
			Poll::Pending => Step::Open,
			Poll::Ready(Ok(group)) => {
				open.src = Some(Source {
					input: *id,
					group,
					frame: None,
				});
				open.refetch = None;
				Step::Open
			}
			Poll::Ready(Err(err)) => {
				let _ = open.dst.clone().abort(err);
				Step::Close
			}
		}
	}

	/// The source failed partway through the group. A group its route dropped on purpose
	/// is lost everywhere; otherwise the route itself is failing, and the group waits for
	/// a replacement to continue it.
	fn lost(open: &mut Open, err: Error, input: &Option<Input>) -> Step {
		let src = open.src.take().expect("lost a source");
		let deliberate = matches!(
			StreamError::from(&err),
			StreamError::Old | StreamError::Evicted | StreamError::App(_)
		);
		let current = input
			.as_ref()
			.is_some_and(|input| input.id == src.input && input.end.is_none());
		if deliberate && current {
			let _ = open.dst.clone().abort(err);
			return Step::Close;
		}
		Step::Open
	}

	/// Give up groups no route can continue: past [`MAX_ORPHANS`], below a start the
	/// input declared, or anything left once the input ended cleanly.
	fn prune_orphans(&mut self, waiter: &kio::Waiter) {
		if self.groups.values().all(|open| open.src.is_some()) {
			return;
		}
		// Below a start the input declared, nothing is coming. An input that declares
		// none promises nothing below its live edge either.
		let start = self.input.as_ref().and_then(|input| match (&input.sub, input.copy.poll_start(waiter)) {
			(Sub::Ready(_), Poll::Ready(Some(start))) => Some(start),
			(Sub::Ready(_), Poll::Ready(None)) => input.copy.latest(),
			_ => None,
		});
		if let Some(start) = start {
			self.give_up_orphans(|sequence| sequence < start, Error::NotFound);
		}
		if let Some(input) = &self.input
			&& matches!(input.end, Some(Ok(())))
			&& !self.groups.values().any(|open| open.src.is_some())
		{
			self.give_up_orphans(|_| true, Error::NotFound);
		}
		let orphans: Vec<u64> = self
			.groups
			.iter()
			.filter(|(_, open)| open.src.is_none())
			.map(|(sequence, _)| *sequence)
			.collect();
		if orphans.len() > MAX_ORPHANS {
			let excess = orphans.len() - MAX_ORPHANS;
			let oldest: Vec<u64> = orphans.into_iter().take(excess).collect();
			self.give_up_orphans(|sequence| oldest.contains(&sequence), Error::NotFound);
		}
	}

	fn give_up_orphans(&mut self, which: impl Fn(u64) -> bool, err: Error) {
		self.groups.retain(|sequence, open| {
			if open.src.is_some() || !which(*sequence) {
				return true;
			}
			tracing::debug!(group = sequence, frame = open.next(), %err, "no route can continue this group");
			let _ = open.dst.clone().abort(err.clone());
			false
		});
	}

	/// Forward fetches of uncached groups to the input.
	fn poll_fetches(&mut self, waiter: &kio::Waiter) -> bool {
		let mut progress = false;
		while let Poll::Ready(res) = self.dynamic.poll_requested_group(waiter) {
			let Ok(request) = res else { break };
			self.fetches.push(Fetch { request, pending: None });
			progress = true;
		}

		let mut i = 0;
		while i < self.fetches.len() {
			let fetch = &mut self.fetches[i];
			if fetch.request.poll_unused(waiter).is_ready() {
				self.fetches.swap_remove(i);
				progress = true;
				continue;
			}
			// (Re)issue on the current input; without one, wait for the next.
			let Some(input) = &self.input else {
				i += 1;
				continue;
			};
			let id = input.id;
			if fetch.pending.as_ref().is_none_or(|(pending, _)| *pending != id) {
				let options = group::Fetch::default()
					.with_priority(fetch.request.priority())
					.with_frame_start(fetch.request.frame_start());
				fetch.pending = Some((id, input.copy.fetch_group(fetch.request.sequence(), options)));
			}
			let (_, pending) = fetch.pending.as_ref().expect("issued above");
			let result = match kio::Pollable::poll(&**pending, waiter) {
				Poll::Pending => {
					i += 1;
					continue;
				}
				Poll::Ready(result) => result,
			};
			// A dead route's failure is the route's: retry on the next input.
			let route_dead = matches!(input.copy.poll_complete(&kio::Waiter::noop()), Poll::Ready(Err(_)));
			match result {
				Ok(group) => {
					let fetch = self.fetches.swap_remove(i);
					let sequence = fetch.request.sequence();
					if let Ok(dst) = fetch.request.accept(None) {
						self.groups.entry(sequence).or_insert(Open {
							dst,
							fetched: true,
							refetch: None,
							partial: None,
							src: None,
						});
						self.attach(id, group);
					}
				}
				Err(_) if route_dead => {
					i += 1;
					continue;
				}
				Err(err) => self.fetches.swap_remove(i).request.reject(err),
			}
			progress = true;
		}
		progress
	}

	/// Forward the readers' aggregate demand to the input, never below its floor.
	fn poll_demand(&mut self, waiter: &kio::Waiter) -> bool {
		// Drain the change notifications so the waiter stays registered for the next
		// one; the value itself is read below.
		match &mut self.logical {
			Logical::Live(producer) => while let Poll::Ready(Ok(_)) = producer.poll_subscription_changed(waiter) {},
			Logical::Pending(request) => while request.poll_subscription_changed(waiter).is_ready() {},
			Logical::Done(_) => return false,
		}
		let Some(input) = self.input.as_mut() else {
			return false;
		};
		let Some(demand) = self.logical.subscription() else {
			return false;
		};
		if self.demand.as_ref() == Some(&demand) {
			return false;
		}
		let wire = Subscription {
			start: max_some(demand.start, input.floor),
			..demand.clone()
		};
		let _ = match &mut input.sub {
			// The first reader to subscribe: subscribe the copy for it.
			Sub::None => {
				input.sub = Sub::Pending(input.copy.subscribe(wire));
				Ok(())
			}
			Sub::Pending(pending) => pending.update(wire),
			Sub::Ready(sub) => sub.update(wire),
		};
		self.demand = Some(demand);
		true
	}

	/// Tell the front an input ran out, once nothing it delivered is still being written.
	fn report(&mut self) {
		let Some(input) = &self.input else { return };
		let Some(result) = &input.end else { return };
		if self.groups.values().any(|open| open.src.as_ref().is_some_and(|src| src.input == input.id)) {
			return;
		}
		let id = input.id;
		if self.status.read().ended.as_ref().is_some_and(|ended| ended.id == id) {
			return;
		}
		let ended = Ended {
			id,
			result: result.clone(),
			delivered: input.delivered,
		};
		if let Ok(mut status) = self.status.write() {
			status.ended = Some(ended);
		}
			}
}

#[cfg(test)]
mod test {
	use super::*;
	use crate::{Timestamp, broadcast};
	use futures::FutureExt;
	use std::sync::Arc;
	use std::time::Duration;

	fn copy(name: &str) -> (track::Producer, track::Consumer) {
		let producer = track::Producer::new(Arc::new(broadcast::Info::default()), name, None);
		let consumer = producer.consume();
		(producer, consumer)
	}

	/// A pump over a fresh logical track, and a consumer of that track.
	fn logical() -> (Pump, Handle, track::Consumer) {
		let request = track::Request::new(Arc::new(broadcast::Info::default()), "video");
		let consumer = request.consume();
		let (pump, handle) = Pump::new(request);
		(pump, handle, consumer)
	}

	fn replay() -> Subscription {
		Subscription::default().with_max_age(Duration::from_secs(30))
	}

	/// Feed `copy` as input `id`, subscribed from where the logical track stops.
	fn feed(pump: &mut Pump, handle: &mut Handle, copy: &track::Consumer) {
		let floor = handle.resume_floor();
		let sub = copy.subscribe(Subscription {
			start: floor,
			..replay()
		});
		handle.feed(Feed {
			copy: copy.clone(),
			info: Some(track::Info::default()),
			sub: Some(sub),
			floor,
		});
		step(pump);
	}

	/// Run the pump until it has nothing left to do.
	fn step(pump: &mut Pump) {
		assert!(pump.poll(&kio::Waiter::noop()).is_pending(), "pump exited");
	}

	fn subscribe(consumer: &track::Consumer) -> track::Subscriber {
		consumer
			.subscribe(replay())
			.now_or_never()
			.expect("logical track accepted")
			.expect("logical track live")
	}

	fn recv(sub: &mut track::Subscriber) -> group::Consumer {
		sub.recv_group()
			.now_or_never()
			.expect("should not block")
			.expect("should not error")
			.expect("should not end")
	}

	/// Every frame the group has right now, then whether it ended (`Some(Ok)`), failed
	/// (`Some(Err)`), or is still open (`None`).
	fn drain(group: &mut group::Consumer) -> (Vec<String>, Option<std::result::Result<(), String>>) {
		let mut frames = Vec::new();
		loop {
			match group.read_frame().now_or_never() {
				Some(Ok(Some(frame))) => frames.push(String::from_utf8(frame.payload.to_vec()).unwrap()),
				Some(Ok(None)) => return (frames, Some(Ok(()))),
				Some(Err(err)) => return (frames, Some(Err(err.to_string()))),
				None => return (frames, None),
			}
		}
	}

	fn write(group: &mut group::Producer, payload: &str) {
		group.write_frame(Timestamp::ZERO, payload.as_bytes().to_vec()).unwrap();
	}

	/// The route dies the way a session does: the track first, then its open groups.
	fn kill(track: track::Producer, groups: impl IntoIterator<Item = group::Producer>) {
		let _ = track.abort_session(Error::Transport("gone".into()));
		for group in groups {
			let _ = group.abort(Error::Transport("gone".into()));
		}
	}

	#[test]
	fn forwards_groups_and_reports_a_clean_end() {
		let (mut pump, mut handle, logical) = logical();
		let (a, a_copy) = copy("a");
		feed(&mut pump, &mut handle, &a_copy);
		let mut sub = subscribe(&logical);

		let mut group = a.append_group().unwrap();
		write(&mut group, "0.0");
		write(&mut group, "0.1");
		group.finish().unwrap();
		a.finish().unwrap();
		step(&mut pump);
		assert!(handle.poll_ended(0, &kio::Waiter::noop()).is_pending(), "a group may still be owed");
		drop(a);
		step(&mut pump);

		let mut got = recv(&mut sub);
		assert_eq!(drain(&mut got), (vec!["0.0".into(), "0.1".into()], Some(Ok(()))));
		let ended = handle.poll_ended(0, &kio::Waiter::noop());
		let Poll::Ready(ended) = ended else { panic!("not reported") };
		assert!(ended.result.is_ok());
		assert!(ended.delivered);
	}

	/// The headline case: A delivered (0,0) and (0,1) and died; B is asked from (0,2)
	/// and continues the same logical group, which readers see once.
	#[test]
	fn a_replacement_continues_the_open_group() {
		let (mut pump, mut handle, logical) = logical();
		let (a, a_copy) = copy("a");
		feed(&mut pump, &mut handle, &a_copy);
		let mut sub = subscribe(&logical);

		let mut group = a.create_group(group::Info { sequence: 0 }).unwrap();
		write(&mut group, "0.0");
		write(&mut group, "0.1");
		step(&mut pump);
		let mut reading = recv(&mut sub);
		assert_eq!(drain(&mut reading), (vec!["0.0".into(), "0.1".into()], None));

		kill(a, [group]);
		step(&mut pump);
		assert_eq!(drain(&mut reading), (vec![], None), "a dead route must not end the group");
		assert_eq!(handle.resume_floor(), Some(Position { group: 0, frame: 2 }));

		let (b, b_copy) = copy("b");
		feed(&mut pump, &mut handle, &b_copy);
		assert_eq!(
			b.subscription().unwrap().start,
			Some(Position { group: 0, frame: 2 }),
			"the replacement is asked for exactly what is missing"
		);
		let mut group = b.create_group(group::Info { sequence: 0 }).unwrap();
		group.start_at(2).unwrap();
		write(&mut group, "0.2");
		group.finish().unwrap();
		let mut next = b.create_group(group::Info { sequence: 1 }).unwrap();
		write(&mut next, "1.0");
		next.finish().unwrap();
		step(&mut pump);

		assert_eq!(drain(&mut reading), (vec!["0.2".into()], Some(Ok(()))));
		assert_eq!(recv(&mut sub).sequence, 1);
		assert!(sub.recv_group().now_or_never().is_none(), "no group is delivered twice");
	}

	/// An older peer can't start mid-group, so it resends the whole group: the frames
	/// already written are dropped by index.
	#[test]
	fn a_resent_group_is_deduplicated() {
		let (mut pump, mut handle, logical) = logical();
		let (a, a_copy) = copy("a");
		feed(&mut pump, &mut handle, &a_copy);
		let mut sub = subscribe(&logical);

		let mut group = a.create_group(group::Info { sequence: 0 }).unwrap();
		write(&mut group, "0.0");
		write(&mut group, "0.1");
		step(&mut pump);
		kill(a, [group]);
		step(&mut pump);

		let (b, b_copy) = copy("b");
		feed(&mut pump, &mut handle, &b_copy);
		let mut group = b.create_group(group::Info { sequence: 0 }).unwrap();
		for frame in ["0.0", "0.1", "0.2"] {
			write(&mut group, frame);
		}
		group.finish().unwrap();
		step(&mut pump);

		let mut reading = recv(&mut sub);
		assert_eq!(
			drain(&mut reading),
			(vec!["0.0".into(), "0.1".into(), "0.2".into()], Some(Ok(())))
		);
		assert!(sub.recv_group().now_or_never().is_none());
	}

	/// A route dying partway through a frame leaves the logical frame open; the
	/// replacement's copy of that frame finishes it from the byte where it stopped.
	#[test]
	fn a_replacement_finishes_a_half_written_frame() {
		let (mut pump, mut handle, logical) = logical();
		let (a, a_copy) = copy("a");
		feed(&mut pump, &mut handle, &a_copy);
		let mut sub = subscribe(&logical);

		let mut group = a.create_group(group::Info { sequence: 0 }).unwrap();
		write(&mut group, "0.0");
		let mut frame = group
			.create_frame(frame::Info {
				size: 6,
				timestamp: Timestamp::ZERO,
			})
			.unwrap();
		frame.write(b"foo".to_vec()).unwrap();
		std::mem::forget(frame);
		step(&mut pump);

		let mut reading = recv(&mut sub);
		assert_eq!(drain(&mut reading).0, vec!["0.0".to_string()]);
		let mut streaming = reading.next_frame().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(streaming.read_chunk().now_or_never().unwrap().unwrap().unwrap(), b"foo".as_ref());

		kill(a, [group]);
		step(&mut pump);
		assert_eq!(
			handle.resume_floor(),
			Some(Position { group: 0, frame: 1 }),
			"the half-written frame is asked for again"
		);

		let (b, b_copy) = copy("b");
		feed(&mut pump, &mut handle, &b_copy);
		let mut group = b.create_group(group::Info { sequence: 0 }).unwrap();
		group.start_at(1).unwrap();
		write(&mut group, "foobar");
		group.finish().unwrap();
		step(&mut pump);

		assert_eq!(streaming.read_chunk().now_or_never().unwrap().unwrap().unwrap(), b"bar".as_ref());
		assert_eq!(streaming.read_chunk().now_or_never().unwrap().unwrap(), None);
		assert_eq!(drain(&mut reading), (vec![], Some(Ok(()))));
	}

	/// The old route's frames after the switch are ignored: its subscription is gone.
	#[test]
	fn the_replaced_route_is_cancelled() {
		let (mut pump, mut handle, logical) = logical();
		let (a, a_copy) = copy("a");
		feed(&mut pump, &mut handle, &a_copy);
		let mut sub = subscribe(&logical);

		let mut group = a.create_group(group::Info { sequence: 0 }).unwrap();
		write(&mut group, "0.0");
		step(&mut pump);
		assert!(a.subscription().is_some());

		let (b, b_copy) = copy("b");
		feed(&mut pump, &mut handle, &b_copy);
		drop(a_copy);
		assert!(a.subscription().is_none(), "the replaced route is unsubscribed at once");

		// A frame racing the switch never lands.
		write(&mut group, "a.late");
		step(&mut pump);
		let mut reading = recv(&mut sub);
		assert_eq!(drain(&mut reading), (vec!["0.0".into()], None));

		let mut group = b.create_group(group::Info { sequence: 0 }).unwrap();
		group.start_at(1).unwrap();
		write(&mut group, "0.1");
		group.finish().unwrap();
		step(&mut pump);
		assert_eq!(drain(&mut reading), (vec!["0.1".into()], Some(Ok(()))));
	}

	/// The old route had every frame but its FIN was still in flight: the replacement
	/// is asked for the empty tail, and its FIN ends the group cleanly.
	#[test]
	fn a_group_complete_at_the_switch_ends_cleanly() {
		let (mut pump, mut handle, logical) = logical();
		let (a, a_copy) = copy("a");
		feed(&mut pump, &mut handle, &a_copy);
		let mut sub = subscribe(&logical);

		let mut group = a.create_group(group::Info { sequence: 0 }).unwrap();
		write(&mut group, "0.0");
		write(&mut group, "0.1");
		step(&mut pump);
		kill(a, [group]);
		step(&mut pump);

		let (b, b_copy) = copy("b");
		feed(&mut pump, &mut handle, &b_copy);
		let group = b.create_group(group::Info { sequence: 0 }).unwrap();
		let mut group = group;
		group.start_at(2).unwrap();
		group.finish().unwrap();
		step(&mut pump);

		let mut reading = recv(&mut sub);
		assert_eq!(drain(&mut reading), (vec!["0.0".into(), "0.1".into()], Some(Ok(()))));
	}

	/// A group the old route opened but sent no frames of is asked for from its head,
	/// not skipped.
	#[test]
	fn an_empty_open_group_is_asked_from_its_head() {
		let (mut pump, mut handle, logical) = logical();
		let (a, a_copy) = copy("a");
		feed(&mut pump, &mut handle, &a_copy);
		let mut sub = subscribe(&logical);

		let group = a.create_group(group::Info { sequence: 0 }).unwrap();
		step(&mut pump);
		kill(a, [group]);
		step(&mut pump);
		assert_eq!(handle.resume_floor(), Some(Position { group: 0, frame: 0 }));

		let (b, b_copy) = copy("b");
		feed(&mut pump, &mut handle, &b_copy);
		let mut group = b.create_group(group::Info { sequence: 0 }).unwrap();
		write(&mut group, "0.0");
		group.finish().unwrap();
		step(&mut pump);

		let mut reading = recv(&mut sub);
		assert_eq!(drain(&mut reading), (vec!["0.0".into()], Some(Ok(()))));
	}

	/// A reader that sleeps through several switches still gets every group once.
	#[test]
	fn an_idle_reader_survives_many_switches() {
		let (mut pump, mut handle, logical) = logical();
		let mut sub = None;
		let mut routes = Vec::new();
		for sequence in 0..6u64 {
			let (route, route_copy) = copy("route");
			feed(&mut pump, &mut handle, &route_copy);
			sub.get_or_insert_with(|| subscribe(&logical));
			let mut group = route.create_group(group::Info { sequence }).unwrap();
			write(&mut group, &format!("{sequence}"));
			group.finish().unwrap();
			step(&mut pump);
			routes.push((route, route_copy));
		}
		let sub = sub.as_mut().unwrap();
		let got: Vec<u64> = (0..6).map(|_| recv(sub).sequence).collect();
		assert_eq!(got, (0..6).collect::<Vec<_>>());
	}

	/// A group the live route dropped on purpose is lost for readers too, at once.
	#[test]
	fn a_group_dropped_upstream_is_lost() {
		let (mut pump, mut handle, logical) = logical();
		let (a, a_copy) = copy("a");
		feed(&mut pump, &mut handle, &a_copy);
		let mut sub = subscribe(&logical);

		let mut group = a.create_group(group::Info { sequence: 0 }).unwrap();
		write(&mut group, "0.0");
		step(&mut pump);
		let mut reading = recv(&mut sub);
		group.abort(Error::Old).unwrap();
		step(&mut pump);

		assert_eq!(drain(&mut reading).1, Some(Err(Error::Old.to_string())));
		drop(a);
	}

	/// A replacement declaring it starts past an open group gives that group up.
	#[test]
	fn a_declared_start_past_an_open_group_gives_it_up() {
		let (mut pump, mut handle, logical) = logical();
		let (a, a_copy) = copy("a");
		feed(&mut pump, &mut handle, &a_copy);
		let mut sub = subscribe(&logical);

		let mut group = a.create_group(group::Info { sequence: 0 }).unwrap();
		write(&mut group, "0.0");
		step(&mut pump);
		let mut reading = recv(&mut sub);
		kill(a, [group]);
		step(&mut pump);

		let (mut b, b_copy) = copy("b");
		b.start_at(1).unwrap();
		feed(&mut pump, &mut handle, &b_copy);

		assert_eq!(drain(&mut reading).1, Some(Err(Error::NotFound.to_string())));
	}

	/// Readers' demand reaches the input, never below where it was asked to start.
	#[test]
	fn demand_is_forwarded_above_the_floor() {
		let (mut pump, mut handle, logical) = logical();
		let (a, a_copy) = copy("a");
		let mut group = a.create_group(group::Info { sequence: 0 }).unwrap();
		write(&mut group, "0.0");
		feed(&mut pump, &mut handle, &a_copy);
		let _sub = subscribe(&logical);
		step(&mut pump);
		group.finish().unwrap();
		step(&mut pump);

		let (b, b_copy) = copy("b");
		feed(&mut pump, &mut handle, &b_copy);
		let wanted = Subscription::default().with_max_age(Duration::from_secs(7));
		let _late = logical.subscribe(wanted).now_or_never().unwrap().unwrap();
		step(&mut pump);
		let demand = b.subscription().unwrap();
		assert_eq!(demand.start, Some(Position::group(1)));
		assert_eq!(
			demand.max_age,
			pump.logical.subscription().unwrap().max_age,
			"the readers' aggregate is what goes upstream"
		);
	}

	/// A fetch for a group the logical track never cached goes to the input.
	#[test]
	fn fetches_go_to_the_input() {
		let (mut pump, mut handle, logical) = logical();
		let (a, a_copy) = copy("a");
		let mut old = a.create_group(group::Info { sequence: 3 }).unwrap();
		write(&mut old, "3.0");
		old.finish().unwrap();
		let mut live = a.create_group(group::Info { sequence: 5 }).unwrap();
		write(&mut live, "5.0");
		feed(&mut pump, &mut handle, &a_copy);

		let fetching = logical.fetch_group(3, None);
		step(&mut pump);
		step(&mut pump);
		let mut group = fetching.now_or_never().expect("resolved").expect("served");
		assert_eq!(drain(&mut group), (vec!["3.0".into()], Some(Ok(()))));
	}

	/// A fetched group the reader abandons partway is cut short, all the way upstream,
	/// and never cached as whole.
	#[tokio::test(start_paused = true)]
	async fn an_abandoned_fetch_is_cancelled_upstream() {
		let (pump, mut handle, logical) = logical();
		tokio::spawn(pump.run());
		let request = track::Request::new(Arc::new(broadcast::Info::default()), "a");
		let a_copy = request.consume();
		let groups = request.dynamic();
		let _a = request.accept(None);
		handle.feed(Feed {
			copy: a_copy.clone(),
			info: Some(track::Info::default()),
			sub: None,
			floor: None,
		});

		let fetching = tokio::spawn({
			let logical = logical.clone();
			async move { logical.fetch_group(0, None).await }
		});
		let upstream = tokio::time::timeout(Duration::from_secs(1), groups.requested_group())
			.await
			.expect("the fetch reaches the copy")
			.unwrap();
		let mut group = upstream.accept(None).unwrap();
		write(&mut group, "head");
		let mut fetched = fetching.await.unwrap().unwrap();
		assert_eq!(fetched.read_frame().await.unwrap().unwrap().payload, b"head".as_ref());
		drop(fetched);

		tokio::time::timeout(Duration::from_secs(1), kio::wait(|waiter| group.poll_unused(waiter)))
			.await
			.expect("the copy's group is still wanted");
	}

	#[test]
	fn datagrams_are_forwarded() {
		let (mut pump, mut handle, logical) = logical();
		let (mut a, a_copy) = copy("a");
		feed(&mut pump, &mut handle, &a_copy);
		let mut sub = subscribe(&logical);
		a.insert_datagram(4, Timestamp::ZERO, b"d".to_vec()).unwrap();
		step(&mut pump);
		let datagram = sub.recv_datagram().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(datagram.sequence, 4);
	}

	/// A reader parked mid-group is woken by the replacement's frame, with the old
	/// route still connected but silent (audit S1).
	#[tokio::test(start_paused = true)]
	async fn a_parked_reader_wakes_on_the_replacement() {
		let (pump, mut handle, logical) = logical();
		tokio::spawn(pump.run());
		let (a, a_copy) = copy("a");
		let floor = handle.resume_floor();
		let sub = a_copy.subscribe(replay());
		handle.feed(Feed {
			copy: a_copy.clone(),
			info: Some(track::Info::default()),
			sub: Some(sub),
			floor,
		});
		let mut sub = logical.subscribe(replay()).await.unwrap();

		let mut group = a.create_group(group::Info { sequence: 0 }).unwrap();
		write(&mut group, "0.0");
		let mut reading = sub.recv_group().await.unwrap().unwrap();
		assert_eq!(reading.read_frame().await.unwrap().unwrap().payload, b"0.0".as_ref());

		let (b, b_copy) = copy("b");
		let floor = handle.resume_floor();
		let sub_b = b_copy.subscribe(Subscription {
			start: floor,
			..replay()
		});
		handle.feed(Feed {
			copy: b_copy.clone(),
			info: Some(track::Info::default()),
			sub: Some(sub_b),
			floor,
		});
		let read = tokio::spawn(async move { reading.read_frame().await });
		tokio::time::sleep(Duration::from_millis(10)).await;
		let mut tail = b.create_group(group::Info { sequence: 0 }).unwrap();
		tail.start_at(1).unwrap();
		write(&mut tail, "0.1");
		let frame = tokio::time::timeout(Duration::from_secs(1), read)
			.await
			.expect("reader never woke")
			.unwrap()
			.unwrap()
			.unwrap();
		assert_eq!(frame.payload, b"0.1".as_ref());
		drop((a, group));
	}
}
