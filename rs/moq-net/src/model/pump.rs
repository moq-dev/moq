//! Forward the routes' copies of a track into the logical track an origin front serves.
//!
//! A front serves each track through one route at a time and switches when that route
//! dies, withdraws, or is beaten. Readers never see the switch because they read a plain
//! logical track that outlives every route, and a [`Pump`] is that track's only writer.
//!
//! A switch is a hint, not a seam. The replacement is asked to start where the logical
//! track stops ([`track::TrackWeak::resume_floor`]), but it may start earlier or later,
//! and the route it replaces may still deliver past that point. So every copy of a group
//! that reaches the pump, from any route's subscription or from a fetch, is a candidate:
//! each frame is written once, by index, from whichever candidate has it first. A name
//! always means the same content, so a duplicate is byte-for-byte what is already cached.
//!
//! The pump never guesses that a group is lost. A group ends when a copy finishes it, when
//! the serving route says it will not send it (it aborted the group, or refused a fetch
//! for it while its subscription is past it), or once the readers' own budget convicts
//! it as stale. A group closed once is never created again.

use std::collections::{BTreeMap, BTreeSet, btree_map};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::Poll;

use crate::{Error, Result, StreamError, frame, group, track};

use super::subscription::{Position, Subscription, max_some};

/// How many groups may sit with no copy to continue them, while nobody subscribes, before
/// the oldest is given up. Each waits for a route to deliver it, so this bounds what a
/// churning set of routes can pin when no reader's budget convicts them.
const MAX_ORPHANS: usize = 4;

/// How many closed groups the pump remembers, so a route delivering one late cannot
/// create it again after the cache let it go.
const MAX_CLOSED: usize = 64;

/// What the front tells its pump to do.
pub(crate) enum Command {
	/// Read this subscription from now on. Sent through [`Handle::feed`], which numbers it.
	/// The input it replaces keeps feeding the groups it has open, then is let go.
	Feed(u64, Box<Feed>),
	/// Nobody reads the track: stop reading new groups. A group a reader still holds keeps
	/// its copies until they run out. What was delivered stays cached.
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
	/// Whether the input's subscription delivered anything, new or not.
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
	/// Shared with the pump; see [`Pump::idle`].
	idle: Arc<AtomicBool>,
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
		self.send(Command::Feed(id, Box::new(feed)));
		id
	}

	/// Stop reading for a track nobody reads; see [`Command::Detach`].
	pub(crate) fn detach(&mut self) {
		// Set here rather than when the pump applies it, so a route queried right after
		// already joins at its live edge.
		self.idle.store(true, Ordering::Relaxed);
		self.send(Command::Detach);
	}

	/// Where the next route feeding the track should start.
	pub(crate) fn resume_floor(&self) -> Option<Position> {
		self.weak.resume_floor(self.idle.load(Ordering::Relaxed))
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

/// A route's copy of the track, and the subscription to it.
struct Input {
	id: u64,
	copy: track::Consumer,
	sub: Sub,
	floor: Option<Position>,
	/// The subscription ran out: `Ok` for a clean end. Reported once its groups drained.
	end: Option<Result<()>>,
	/// Whether its subscription delivered anything, new or not.
	delivered: bool,
	/// The newest group its subscription delivered.
	newest: Option<u64>,
	/// The newest datagram written before it was fed: a copy that buffers datagrams
	/// replays them to a new subscriber, and those are already written.
	datagrams: Option<u64>,
}

impl Input {
	/// Whether this input's subscription will not deliver group `sequence` (any more):
	/// it ended, it already delivered it, it starts past it, or there is none.
	fn forgoes(&self, sequence: u64, open: &Open, waiter: &kio::Waiter) -> bool {
		let Sub::Ready(_) = &self.sub else {
			return matches!(self.sub, Sub::None);
		};
		self.end.is_some()
			|| open.delivered.contains(&self.id)
			|| matches!(self.copy.poll_start(waiter), Poll::Ready(Some(start)) if sequence < start)
	}
}

/// Whether `err`, from a fetch or group of `input`, is its route failing rather than a
/// refusal: another route may still deliver what it failed.
fn route_failed(input: &Input, err: &Error) -> bool {
	matches!(
		err,
		Error::Transport(_)
			| Error::Session(_)
			| Error::SessionClosed
			| Error::GoingAway
			| Error::Stream(StreamError::Session(_) | StreamError::GoingAway)
	) || matches!(input.copy.poll_complete(&kio::Waiter::noop()), Poll::Ready(Err(_)))
}

/// A frame being written into the logical track.
struct Partial {
	frame: frame::ProducerOwned,
	/// Its index in the group.
	index: u64,
}

/// One route's copy of a group: a candidate for the frames the logical group lacks.
struct Copy {
	input: u64,
	group: group::Consumer,
	/// Answered a fetch rather than delivered by the subscription.
	fetched: bool,
	/// The next frame, while this copy reads it, and how many of its bytes it has read.
	frame: Option<(frame::Consumer, usize)>,
}

/// What the serving input was asked for the rest of a group.
struct Asked {
	input: u64,
	state: Asking,
}

enum Asking {
	/// Boxed: a fetch dwarfs the rest.
	Pending(Box<kio::Pending<track::Fetching>>),
	/// The answer is one of the group's copies.
	Answered,
	/// The route is failing: the next input is asked instead.
	RouteFailed,
	/// The route will not serve it.
	Refused(Error),
}

/// A logical group still being written.
struct Open {
	dst: group::Producer,
	/// Fetched for a reader rather than delivered live: the fetches still waiting for it.
	/// Once neither they nor any reader want it, it is cut short, all the way upstream.
	waiting: Option<track::FetchWaiting>,
	/// The in-flight frame, kept across copies so another can finish it.
	partial: Option<Partial>,
	copies: Vec<Copy>,
	asked: Option<Asked>,
	/// The inputs whose subscription delivered the group: none sends it twice.
	delivered: Vec<u64>,
}

impl Open {
	fn new(dst: group::Producer, waiting: Option<track::FetchWaiting>) -> Self {
		Self {
			dst,
			waiting,
			partial: None,
			copies: Vec::new(),
			asked: None,
			delivered: Vec::new(),
		}
	}

	/// The index of the first frame the logical group does not have in full.
	fn next(&self) -> u64 {
		match &self.partial {
			Some(partial) => partial.index,
			None => self.dst.frame_count() as u64,
		}
	}

	/// Whether no copy can supply the next frame: none is reading it or holds it.
	fn blocked(&mut self) -> bool {
		let next = self.next();
		!self.copies.iter_mut().any(|copy| {
			copy.frame.is_some() || {
				copy.group.start_at(next);
				copy.group.index() == next
			}
		})
	}

	/// Whether `input` may be asked for the rest: it was not asked already.
	fn askable(&self, input: &Input) -> bool {
		self.asked.as_ref().is_none_or(|asked| asked.input != input.id)
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
	/// The input read for new groups, datagrams, demand and fetches.
	input: Option<Input>,
	/// Replaced inputs, kept subscribed while they feed a group open when they were
	/// replaced, so its tail is not cut off. Nothing newer is taken from them.
	draining: Vec<(Input, u64)>,
	/// The groups the live feed delivered.
	groups: BTreeMap<u64, Open>,
	/// The groups fetched for readers, kept apart so a fetch never displaces a live group.
	fetched: BTreeMap<u64, Open>,
	/// Live groups already closed; see [`MAX_CLOSED`].
	closed: BTreeSet<u64>,
	fetches: Vec<Fetch>,
	budget: frame::Budget,
	/// The aggregate demand last forwarded to the input.
	demand: Option<Subscription>,
	/// The front let go: drain the input, then end the track as it ends.
	concluding: bool,
	/// The track went unread and no input has delivered since: what is cached may be
	/// stale, so the next route joins at its live edge unless a group is still owed.
	idle: Arc<AtomicBool>,
	/// The track went unread: withhold the cache from readers once nobody reads it.
	hiding: bool,
	/// The newest group withheld: how stale the cache is cannot be told until a live route
	/// says where the track is now. The next one to answer its subscription settles it.
	hidden: Option<u64>,
	/// Held while a fetched group is still being written: a fetch in progress reads the
	/// track, so the front keeps an input for it.
	fetching: Option<track::Consumer>,
	/// The logical track's start is the first copy's to declare, mirrored for as long as
	/// that copy feeds it: a lite-05 SUBSCRIBE_START lands after its subscription resolves.
	starting: bool,
	/// The start last mirrored, so re-reading an unchanged one wakes no reader. `None`
	/// until a copy declared one: until then, the next copy fed is still the first.
	mirrored: Option<Option<u64>>,
	/// The newest datagram written.
	datagrams: Option<u64>,
}

impl Pump {
	/// A pump serving `request`, and the front's handle on it.
	pub(crate) fn new(request: track::Request) -> (Self, Handle) {
		let commands = kio::Queue::new();
		let status = kio::Producer::<Status>::default();
		let idle = Arc::new(AtomicBool::new(false));
		let handle = Handle {
			commands: commands.clone(),
			status: status.consume(),
			weak: request.weak(),
			feeds: 0,
			idle: idle.clone(),
		};
		let pump = Self {
			dynamic: request.dynamic(),
			logical: Logical::Pending(request),
			commands,
			status,
			input: None,
			draining: Vec::new(),
			groups: BTreeMap::new(),
			fetched: BTreeMap::new(),
			closed: BTreeSet::new(),
			fetches: Vec::new(),
			budget: frame::Budget::default(),
			demand: None,
			concluding: false,
			idle,
			hiding: false,
			hidden: None,
			fetching: None,
			starting: false,
			mirrored: None,
			datagrams: None,
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
			// The front forgot the track (it went unread): stop reading new groups, but a
			// reader already holding a group still gets the rest of it.
			if let Logical::Live(producer) = &self.logical
				&& producer.poll_closed(waiter).is_ready()
			{
				self.logical = Logical::Done(None);
				self.retire();
				self.give_up_orphans(Error::Dropped);
			}
			if self.concluding && self.conclude(waiter) {
				return Poll::Ready(());
			}
			if self.hiding {
				self.poll_hide(waiter);
			}

			// Demand first: a fresh input reads with what the readers want, not its
			// initial guess.
			let mut progress = self.poll_demand(waiter);
			progress |= self.poll_input(waiter);
			progress |= self.poll_draining(waiter);
			progress |= self.poll_groups(waiter);
			progress |= self.poll_fetches(waiter);
			self.prune_orphans();
			self.let_go();
			self.report();
			let fetching = self.fetched.values().any(|open| open.waiting.is_some());
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
			Command::Feed(id, feed) => self.feed(id, *feed),
			Command::Detach => {
				self.retire();
				self.hiding = true;
			}
			Command::Finish => self.finish(),
			Command::Abort(err) => self.abort(err),
		}
	}

	fn finish(&mut self) {
		self.retire();
		self.give_up_orphans(Error::Closed);
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
		self.retire();
		self.give_up_orphans(err.clone());
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

	/// With the front gone, end the track once the input does, then let the groups
	/// readers hold run out. True when there is nothing left to do.
	fn conclude(&mut self, waiter: &kio::Waiter) -> bool {
		// Nobody reads it, and with the front gone nobody will: let go of the input now
		// rather than when it ends, which a live one never does.
		if let Logical::Live(producer) = &self.logical {
			let weak = producer.weak();
			weak.poll_unused(waiter);
			if !weak.is_used() {
				self.abort(Error::Dropped);
			}
		}
		if !matches!(self.logical, Logical::Done(_)) {
			match &self.input {
				// Nothing in flight: nothing more will ever be written.
				None => self.abort(Error::Dropped),
				Some(input) => match &input.end {
					Some(_) if self.feeding(input.id) => return false,
					// No front is left to fail over, and a local close is a close, not an
					// error.
					Some(Ok(()) | Err(Error::Closed)) => self.finish(),
					Some(Err(err)) => {
						let err = err.clone();
						self.abort(err);
					}
					None => return false,
				},
			}
		}
		// Nothing continues an orphan now; a group a reader holds runs until its copies do.
		self.give_up_orphans(Error::Dropped);
		!self.opens().any(|open| !open.copies.is_empty())
	}

	fn opens(&self) -> impl Iterator<Item = &Open> {
		self.groups.values().chain(self.fetched.values())
	}

	/// Whether a copy from input `id` is still being read.
	fn feeding(&self, id: u64) -> bool {
		self.opens().any(|open| open.copies.iter().any(|copy| copy.input == id))
	}

	fn feed(&mut self, id: u64, feed: Feed) {
		self.retire();
		// A reader is back, so whatever it saw stays as it is.
		self.hiding = false;
		if let Logical::Done(_) = self.logical {
			return;
		}
		// A replacement continues the track from where the declaring copy left it.
		if self.mirrored.is_some() {
			self.starting = false;
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
			delivered: false,
			newest: None,
			datagrams: self.datagrams,
		});
	}

	/// The first copy says what the logical track is. The front checked every later one
	/// against it.
	///
	/// Where the logical feed starts is the first copy's to say, which a relay publishing
	/// the track waits on: resolving it from whichever group lands first instead would
	/// drop an older one still in flight.
	fn accept(&mut self, info: &track::Info) {
		if let Logical::Pending(_) = &self.logical {
			let Logical::Pending(request) = std::mem::replace(&mut self.logical, Logical::Done(None)) else {
				unreachable!()
			};
			self.logical = Logical::Live(request.resolving_start().accept(info.clone()));
			self.starting = true;
		}
	}

	/// Stop reading new groups from the input. It keeps feeding the live groups it has
	/// open, through the newest of them, and is let go once it feeds none. A fetch still
	/// in flight there is dropped: the next input is asked instead.
	fn retire(&mut self) {
		let Some(input) = self.input.take() else { return };
		for open in self.groups.values_mut().chain(self.fetched.values_mut()) {
			if let Some(Asked {
				input: id,
				state: Asking::Pending(_),
			}) = &open.asked
				&& *id == input.id
			{
				open.asked = None;
			}
		}
		let until = self
			.groups
			.iter()
			.filter(|(_, open)| open.copies.iter().any(|copy| copy.input == input.id))
			.map(|(sequence, _)| *sequence)
			.next_back();
		if let Some(until) = until {
			self.draining.push((input, until));
		}
	}

	/// Withhold the cache from readers once the track is unread. A reader that arrived
	/// since the front saw it go unread may have read the cache already, so it stays as it
	/// is until that reader leaves too, or the front feeds the track again.
	fn poll_hide(&mut self, waiter: &kio::Waiter) {
		let Logical::Live(producer) = &mut self.logical else {
			self.hiding = false;
			return;
		};
		match producer.hide_cache() {
			Ok(newest) => {
				self.hiding = false;
				self.hidden = self.hidden.max(newest);
			}
			Err(()) => producer.weak().poll_unused(waiter),
		}
	}

	/// Drop the replaced inputs that feed no group any more, cancelling their
	/// subscriptions.
	fn let_go(&mut self) {
		let mut draining = std::mem::take(&mut self.draining);
		draining.retain(|(input, _)| self.feeding(input.id));
		self.draining = draining;
	}

	/// Read new groups and datagrams off the input.
	fn poll_input(&mut self, waiter: &kio::Waiter) -> bool {
		let Some(input) = self.input.as_mut() else {
			return false;
		};
		// A forgotten track takes no new groups.
		if input.end.is_some() || !matches!(self.logical, Logical::Live(_) | Logical::Pending(_)) {
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
			// Nothing to read, but the copy's end still has to reach fetching readers, and
			// the front, which fails over on it.
			Sub::None => {
				if let Poll::Ready(end) = input.copy.poll_complete(waiter) {
					input.end = Some(end);
					return true;
				}
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
					input.sub = Sub::Ready(sub.without_budget());
					self.accept(&info);
					progress = true;
				}
			},
			Sub::Ready(_) => {}
		}
		let Some(input) = self.input.as_mut() else {
			return progress;
		};
		if self.starting
			&& let Poll::Ready(start) = input.copy.poll_start(waiter)
			&& self.mirrored != Some(start)
			&& let Logical::Live(producer) = &mut self.logical
		{
			let _ = producer.start_at(start);
			self.mirrored = Some(start);
		}
		// The route's answer settles a hidden cache, without waiting on a group a sparse
		// track may not send for a while. Starting where the cache leaves off (or declaring
		// no start), the cache leads into its feed and comes back. Starting past a gap, it
		// stays with fetches: nothing bounds how old it is. Groups already racing in land
		// first either way.
		if let Some(newest) = self.hidden
			&& let Poll::Ready(start) = input.copy.poll_start(waiter)
			// A copy that died reads as having declared nothing, which is no answer.
			&& (start.is_some() || input.copy.poll_complete(&kio::Waiter::noop()).is_pending())
		{
			if start.is_none_or(|start| start <= newest.saturating_add(1))
				&& let Some(producer) = self.logical.producer()
			{
				producer.reveal_cache();
			}
			self.hidden = None;
		}
		let Sub::Ready(sub) = &mut input.sub else {
			unreachable!("resolved above")
		};

		while let Poll::Ready(res) = sub.poll_recv_datagram(waiter) {
			let Ok(Some(datagram)) = res else { break };
			progress = true;
			input.delivered = true;
			if input.datagrams.is_some_and(|written| datagram.sequence <= written) {
				continue;
			}
			if let Logical::Live(producer) = &mut self.logical
				&& producer
					.insert_datagram(datagram.sequence, datagram.timestamp, datagram.payload)
					.is_ok()
			{
				self.datagrams = self.datagrams.max(Some(datagram.sequence));
			}
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
					// arrives in the meantime. Closed without reaching a declared end, the
					// route went away rather than the track ending: `Closed` for a session
					// closed locally, which another route may still take over.
					if input.copy.poll_closed(waiter).is_ready() {
						let end = match input.copy.poll_complete(&kio::Waiter::noop()) {
							Poll::Ready(end) => end,
							Poll::Pending => Err(Error::Dropped),
						};
						input.end = Some(end);
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
			input.delivered = true;
			input.newest = input.newest.max(Some(group.sequence));
			let id = input.id;
			self.idle.store(false, Ordering::Relaxed);
			self.attach(id, group);
		}
	}

	/// Read the groups replaced inputs still owe: through the newest one each had open.
	fn poll_draining(&mut self, waiter: &kio::Waiter) -> bool {
		let mut groups = Vec::new();
		for (input, until) in &mut self.draining {
			if input.end.is_some() {
				continue;
			}
			if let Sub::Pending(pending) = &input.sub {
				match pending.poll_ok(waiter) {
					Poll::Pending => continue,
					Poll::Ready(Ok(sub)) => input.sub = Sub::Ready(sub.without_budget()),
					Poll::Ready(Err(err)) => {
						input.end = Some(Err(err));
						continue;
					}
				}
			}
			let Sub::Ready(sub) = &mut input.sub else { continue };
			loop {
				match sub.poll_recv_group(waiter) {
					Poll::Ready(Ok(Some(group))) if group.sequence <= *until => groups.push((input.id, group)),
					Poll::Ready(Ok(Some(_))) => {}
					Poll::Ready(Ok(None)) => {
						if input.copy.poll_closed(waiter).is_ready() {
							input.end = Some(Ok(()));
						}
						break;
					}
					Poll::Ready(Err(err)) => {
						input.end = Some(Err(err));
						break;
					}
					Poll::Pending => break,
				}
			}
		}
		let progress = !groups.is_empty();
		for (id, group) in groups {
			self.attach(id, group);
		}
		progress
	}

	/// Add a copy the live feed delivered to its logical group, creating the group unless
	/// it was closed already. One the cache holds but readers were not offered (fetched, or
	/// hidden while idle) is current, so it is offered now.
	fn attach(&mut self, input: u64, group: group::Consumer) {
		let sequence = group.sequence;
		if self.closed.contains(&sequence) {
			return;
		}
		let open = match self.groups.entry(sequence) {
			btree_map::Entry::Occupied(open) => open.into_mut(),
			btree_map::Entry::Vacant(vacant) => {
				let Some(producer) = self.logical.producer() else {
					return;
				};
				match producer.create_group(group::Info { sequence }) {
					Ok(dst) => vacant.insert(Open::new(dst, None)),
					// The cache holds it whole already, perhaps hidden from readers of the
					// live feed, which now delivers it.
					Err(_) => {
						producer.reveal_group(sequence);
						let Some(mut open) = self.fetched.remove(&sequence) else {
							return;
						};
						open.waiting = None;
						vacant.insert(open)
					}
				}
			}
		};
		if !open.delivered.contains(&input) {
			open.delivered.push(input);
		}
		open.copies.push(Copy {
			input,
			group,
			fetched: false,
			frame: None,
		});
	}

	/// Write whatever the open groups' copies have, and ask for what none of them has.
	fn poll_groups(&mut self, waiter: &kio::Waiter) -> bool {
		let mut progress = false;
		let done = matches!(self.logical, Logical::Done(_));
		let budget = self.logical.subscription().map(|demand| demand.max_age);
		let serving = self.input.as_ref().map(|input| input.id);
		for live in [true, false] {
			let groups = match live {
				true => &mut self.groups,
				false => &mut self.fetched,
			};
			let mut closed = Vec::new();
			for (sequence, open) in groups.iter_mut() {
				let mut progressed = false;
				let step = Self::poll_open(
					*sequence,
					open,
					&self.input,
					done,
					&self.budget,
					waiter,
					&mut progressed,
				);
				progress |= progressed;
				let step = match step {
					// Every reader would skip it, and the serving route is not delivering it:
					// nothing is left to wait for, whether no copy has its next frame or only a
					// replaced route that went quiet does. The serving route's own groups are
					// left to the readers, like any relay's.
					Step::Open
						if live
							&& (open.blocked() || !open.copies.iter().any(|copy| Some(copy.input) == serving))
							&& let (Some(budget), Logical::Live(producer)) = (budget, &self.logical)
							&& producer.is_stale(*sequence, budget) =>
					{
						tracing::debug!(group = sequence, frame = open.next(), "giving up a stale group");
						let _ = open.dst.clone().abort(Error::Old);
						Step::Close
					}
					step => step,
				};
				if let Step::Close = step {
					closed.push(*sequence);
					progress = true;
				}
			}
			for sequence in &closed {
				groups.remove(sequence);
			}
			if live {
				for sequence in closed {
					self.close(sequence);
				}
			}
		}
		progress
	}

	/// Remember that live group `sequence` closed; see [`MAX_CLOSED`].
	fn close(&mut self, sequence: u64) {
		self.closed.insert(sequence);
		if self.closed.len() > MAX_CLOSED {
			self.closed.pop_first();
		}
	}

	/// Copy what `open`'s copies have, then ask the input for what they lack.
	fn poll_open(
		sequence: u64,
		open: &mut Open,
		input: &Option<Input>,
		done: bool,
		budget: &frame::Budget,
		waiter: &kio::Waiter,
		progress: &mut bool,
	) -> Step {
		// Aborted under the pump, by the cache's expiry or eviction: nothing more can be
		// written to it.
		if open.dst.is_aborted() {
			return Step::Close;
		}
		// A fetched group nobody wants any more is cut short, never cached as whole. The
		// waiting fetches go first: one resolving takes the group before letting go.
		if let Some(waiting) = &open.waiting
			&& waiting.poll_unused(waiter).is_ready()
			&& open.dst.poll_unused(waiter).is_ready()
		{
			let _ = open.dst.clone().abort(Error::Cancel);
			return Step::Close;
		}
		// Without an input nothing new is read, so a group goes on only while a reader
		// holds it and its copies last. Otherwise it is given up once the track ended, or
		// left for the next input.
		if input.is_none() && (open.copies.is_empty() || open.dst.poll_unused(waiter).is_ready()) {
			if done {
				let _ = open.dst.clone().abort(Error::Dropped);
				return Step::Close;
			}
			open.copies.clear();
		}
		loop {
			let before = open.next();
			if let Step::Close = Self::poll_forward(open, input, budget, waiter) {
				return Step::Close;
			}
			if open.next() != before {
				*progress = true;
			}
			if !open.blocked() {
				return Step::Open;
			}
			let Some(input) = input else { return Step::Open };
			match Self::poll_ask(sequence, open, input, waiter) {
				// A copy arrived: write from it.
				Some(true) => *progress = true,
				Some(false) => return Step::Open,
				None => return Step::Close,
			}
		}
	}

	/// Copy frames from `open`'s copies into the logical group until none has more for now.
	///
	/// Every copy holding the next frame reads it, and whichever has bytes the logical
	/// frame lacks writes them, so a copy that stalls partway through a frame never holds
	/// it from another.
	fn poll_forward(open: &mut Open, input: &Option<Input>, budget: &frame::Budget, waiter: &kio::Waiter) -> Step {
		'frames: loop {
			let next = open.next();
			let mut i = 0;
			while i < open.copies.len() {
				let copy = &mut open.copies[i];
				if copy.frame.is_none() {
					copy.group.start_at(next);
					if copy.group.index() != next {
						// It starts past what the group needs. A fetch asked from `next`
						// answering that way cannot serve it; a live copy may serve later
						// frames.
						if copy.fetched {
							let copy = open.copies.remove(i);
							open.asked = Some(Asked {
								input: copy.input,
								state: Asking::Refused(Error::NotFound),
							});
							continue;
						}
						i += 1;
						continue;
					}
					match copy.group.poll_next_frame(waiter) {
						Poll::Pending => {
							i += 1;
							continue;
						}
						Poll::Ready(Ok(Some(frame))) => {
							match &open.partial {
								Some(partial) if partial.frame.size == frame.size => {}
								// The same frame, sized differently: the routes disagree on the
								// content, so neither can be trusted to finish it.
								Some(_) => {
									tracing::warn!(
										group = open.dst.sequence,
										frame = next,
										"routes disagree on a frame"
									);
									let _ = open.dst.clone().abort(Error::ProtocolViolation);
									return Step::Close;
								}
								None => {
									let info = frame::Info {
										size: frame.size,
										timestamp: frame.timestamp,
									};
									let Ok(owned) = open.dst.create_frame_owned(info, budget) else {
										let _ = open.dst.clone().abort(Error::ProtocolViolation);
										return Step::Close;
									};
									open.partial = Some(Partial {
										frame: owned,
										index: next,
									});
								}
							}
							copy.frame = Some((frame, 0));
						}
						Poll::Ready(Ok(None)) => {
							// The copy ended where the logical group does: so does the group.
							if open.partial.is_none() && copy.group.frame_count() as u64 == next {
								let _ = open.dst.finish();
								return Step::Close;
							}
							// It ended below what the logical group already holds.
							tracing::warn!(
								group = open.dst.sequence,
								frame = next,
								"routes disagree on a group's length"
							);
							let _ = open.dst.clone().abort(Error::ProtocolViolation);
							return Step::Close;
						}
						Poll::Ready(Err(err)) => {
							let copy = open.copies.remove(i);
							Self::lost(open, copy, err, input);
							continue;
						}
					}
				}

				let copy = &mut open.copies[i];
				let (frame, read) = copy.frame.as_mut().expect("opened above");
				let partial = open.partial.as_mut().expect("a frame is open whenever one is read");
				match frame.poll_read_chunk(waiter) {
					Poll::Pending => i += 1,
					Poll::Ready(Ok(Some(mut chunk))) => {
						// Only the bytes past what the logical frame holds are new: another
						// copy may have written further already.
						let written = partial.frame.written();
						let start = *read;
						*read += chunk.len();
						if *read > written {
							let chunk = chunk.split_off(written - start);
							if partial.frame.write(chunk).is_err() {
								let _ = open.dst.clone().abort(Error::ProtocolViolation);
								return Step::Close;
							}
							partial.frame.notify();
						}
					}
					Poll::Ready(Ok(None)) => {
						let partial = open.partial.take().expect("checked above");
						if partial.frame.finish().is_err() {
							let _ = open.dst.clone().abort(Error::ProtocolViolation);
							return Step::Close;
						}
						// Every copy reading the frame moves on to the next.
						for copy in &mut open.copies {
							copy.frame = None;
						}
						continue 'frames;
					}
					Poll::Ready(Err(err)) => {
						let copy = open.copies.remove(i);
						Self::lost(open, copy, err, input);
					}
				}
			}

			// Nothing written yet, nobody holds it, and every copy starts later: the group
			// starts there. A group a reader holds is owed from its head instead.
			if open.partial.is_none()
				&& open.dst.frame_count() == 0
				&& open.dst.poll_unused(&kio::Waiter::noop()).is_ready()
				&& let Some(first) = open.copies.iter().map(|copy| copy.group.index()).min()
				&& first > next
				&& open.dst.start_at(first).is_ok()
			{
				continue;
			}
			return Step::Open;
		}
	}

	/// A copy failed partway through the group. Only the serving input's answer counts:
	/// a failing route leaves the group to the next one, and a group the route aborted on
	/// purpose, or failed after serving it on request, is refused there.
	fn lost(open: &mut Open, copy: Copy, err: Error, input: &Option<Input>) {
		let Some(input) = input
			.as_ref()
			.filter(|input| input.id == copy.input && input.end.is_none())
		else {
			return;
		};
		let state = if route_failed(input, &err) {
			match copy.fetched {
				true => Asking::RouteFailed,
				false => return,
			}
		} else if copy.fetched
			|| matches!(
				StreamError::from(&err),
				StreamError::Old | StreamError::Evicted | StreamError::App(_)
			) {
			Asking::Refused(err)
		} else {
			// Its subscription will not deliver the group again: a fetch picks up from
			// where it stopped.
			return;
		};
		open.asked = Some(Asked { input: input.id, state });
	}

	/// Ask the input for the rest of a group no copy can continue. `Some(true)` once a
	/// fetch answered with a copy, `Some(false)` to wait, `None` once the input refused
	/// it and its subscription will not deliver it either.
	fn poll_ask(sequence: u64, open: &mut Open, input: &Input, waiter: &kio::Waiter) -> Option<bool> {
		if let Some(Asked {
			input: id,
			state: state @ Asking::Pending(_),
		}) = &mut open.asked
		{
			let Asking::Pending(pending) = state else {
				unreachable!()
			};
			match kio::Task::poll(&mut ***pending, waiter) {
				Poll::Pending => return Some(false),
				Poll::Ready(Ok(group)) => {
					open.copies.push(Copy {
						input: *id,
						group,
						fetched: true,
						frame: None,
					});
					*state = Asking::Answered;
					return Some(true);
				}
				Poll::Ready(Err(err)) if route_failed(input, &err) => *state = Asking::RouteFailed,
				Poll::Ready(Err(err)) => *state = Asking::Refused(err),
			}
		}

		// A group fetched for a reader has no subscription behind it.
		let forgoes = open.waiting.is_some() || input.forgoes(sequence, open, waiter);
		if let Some(Asked {
			input: id,
			state: Asking::Refused(err),
		}) = &open.asked
			&& *id == input.id
			&& forgoes
		{
			let _ = open.dst.clone().abort(err.clone());
			return None;
		}
		// Ask once the subscription will not deliver it, or went on past it: a stream
		// overtaken by a later one is only a hint, so the subscription may still win.
		if open.askable(input) && (forgoes || input.newest.is_some_and(|newest| newest > sequence)) {
			let options = group::Fetch::default().with_frame_start(open.next());
			open.asked = Some(Asked {
				input: input.id,
				state: Asking::Pending(Box::new(input.copy.fetch_group(sequence, options))),
			});
			return Some(true);
		}
		Some(false)
	}

	/// Give up live groups no route can continue: anything left once the input ended
	/// cleanly, or past [`MAX_ORPHANS`] while nobody subscribes (the readers' budget bounds
	/// them otherwise).
	fn prune_orphans(&mut self) {
		if self.groups.values().all(|open| !open.copies.is_empty()) {
			return;
		}
		if let Some(input) = &self.input
			&& matches!(input.end, Some(Ok(())))
			&& !self.feeding(input.id)
		{
			self.give_up_orphans(Error::NotFound);
		}
		let orphans: Vec<u64> = self
			.groups
			.iter()
			.filter(|(_, open)| open.copies.is_empty())
			.map(|(sequence, _)| *sequence)
			.collect();
		if self.logical.subscription().is_some() {
			return;
		}
		for sequence in orphans.iter().take(orphans.len().saturating_sub(MAX_ORPHANS)) {
			self.give_up(*sequence, Error::NotFound);
		}
	}

	/// Give up every group with no copy to continue it.
	fn give_up_orphans(&mut self, err: Error) {
		let orphans: Vec<u64> = self
			.groups
			.iter()
			.chain(self.fetched.iter())
			.filter(|(_, open)| open.copies.is_empty())
			.map(|(sequence, _)| *sequence)
			.collect();
		for sequence in orphans {
			self.give_up(sequence, err.clone());
		}
	}

	fn give_up(&mut self, sequence: u64, err: Error) {
		let open = match self.groups.remove(&sequence) {
			Some(open) => {
				self.close(sequence);
				open
			}
			None => match self.fetched.remove(&sequence) {
				Some(open) => open,
				None => return,
			},
		};
		tracing::debug!(group = sequence, frame = open.next(), %err, "no route can continue this group");
		let _ = open.dst.clone().abort(err);
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
			let sequence = fetch.request.sequence();
			// The cache slot is the group the pump is writing: a wider fetch answered now
			// would take it from under that group's readers. It waits for the group to end.
			if self.groups.contains_key(&sequence) || self.fetched.contains_key(&sequence) {
				fetch.pending = None;
				i += 1;
				continue;
			}
			if fetch.pending.as_ref().is_none_or(|(pending, _)| *pending != id) {
				let options = group::Fetch::default()
					.with_priority(fetch.request.priority())
					.with_frame_start(fetch.request.frame_start());
				fetch.pending = Some((id, input.copy.fetch_group(fetch.request.sequence(), options)));
			}
			let (_, pending) = fetch.pending.as_mut().expect("issued above");
			let result = match kio::Task::poll(&mut **pending, waiter) {
				Poll::Pending => {
					i += 1;
					continue;
				}
				Poll::Ready(result) => result,
			};
			match result {
				Ok(group) => {
					let fetch = self.fetches.swap_remove(i);
					let sequence = fetch.request.sequence();
					let waiting = fetch.request.waiting();
					// Accepted only where the cache holds nothing that covers the request,
					// which may still be a live group that started past it: that one goes on
					// beside this one.
					if let Ok(dst) = fetch.request.accept(None) {
						let mut open = Open::new(dst, Some(waiting));
						open.asked = Some(Asked {
							input: id,
							state: Asking::Answered,
						});
						open.copies.push(Copy {
							input: id,
							group,
							fetched: true,
							frame: None,
						});
						self.fetched.insert(sequence, open);
					}
				}
				// A failing route's error is the route's: retry on the next input.
				Err(err) if route_failed(input, &err) => {
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
		if self.feeding(input.id) {
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
		assert!(
			handle.poll_ended(0, &kio::Waiter::noop()).is_pending(),
			"a group may still be owed"
		);
		drop(a);
		step(&mut pump);

		let mut got = recv(&mut sub);
		assert_eq!(drain(&mut got), (vec!["0.0".into(), "0.1".into()], Some(Ok(()))));
		let ended = handle.poll_ended(0, &kio::Waiter::noop());
		let Poll::Ready(ended) = ended else {
			panic!("not reported")
		};
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
		assert_eq!(
			drain(&mut reading),
			(vec![], None),
			"a dead route must not end the group"
		);
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
		assert_eq!(
			streaming.read_chunk().now_or_never().unwrap().unwrap().unwrap(),
			b"foo".as_ref()
		);

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

		assert_eq!(
			streaming.read_chunk().now_or_never().unwrap().unwrap().unwrap(),
			b"bar".as_ref()
		);
		assert_eq!(streaming.read_chunk().now_or_never().unwrap().unwrap(), None);
		assert_eq!(drain(&mut reading), (vec![], Some(Ok(()))));
	}

	/// The replaced route keeps feeding the group it had open, overlapping the
	/// replacement: each frame lands once, from whichever copy has it first. Nothing newer
	/// is taken from it, and it is let go once that group ends.
	#[test]
	fn the_replaced_route_finishes_its_open_group() {
		let (mut pump, mut handle, logical) = logical();
		let (a, a_copy) = copy("a");
		feed(&mut pump, &mut handle, &a_copy);
		let mut sub = subscribe(&logical);

		let mut group = a.create_group(group::Info { sequence: 0 }).unwrap();
		write(&mut group, "0.0");
		step(&mut pump);
		let mut reading = recv(&mut sub);

		let (b, b_copy) = copy("b");
		feed(&mut pump, &mut handle, &b_copy);
		drop(a_copy);
		assert!(a.subscription().is_some(), "the replaced route still feeds group 0");

		// A's frame lands; B's copy of it is a duplicate.
		write(&mut group, "0.1");
		step(&mut pump);
		let mut tail = b.create_group(group::Info { sequence: 0 }).unwrap();
		tail.start_at(1).unwrap();
		write(&mut tail, "0.1");
		write(&mut tail, "0.2");
		step(&mut pump);
		assert_eq!(
			drain(&mut reading),
			(vec!["0.0".into(), "0.1".into(), "0.2".into()], None)
		);

		// A's next group is B's to deliver.
		let mut late = a.create_group(group::Info { sequence: 1 }).unwrap();
		write(&mut late, "a.1");
		step(&mut pump);
		assert!(
			sub.recv_group().now_or_never().is_none(),
			"a group past the switch came from A"
		);

		write(&mut group, "0.2");
		group.finish().unwrap();
		step(&mut pump);
		assert_eq!(drain(&mut reading), (vec![], Some(Ok(()))));
		assert!(
			a.subscription().is_none(),
			"the replaced route is let go once it feeds nothing"
		);
	}

	/// A replaced route that stays up but goes quiet on the group it had open holds it
	/// only until the readers' budget convicts it; the route is let go with it.
	#[test]
	fn a_quiet_replaced_route_holds_its_group_until_stale() {
		let (mut pump, mut handle, logical) = logical();
		let (a, a_copy) = copy("a");
		feed(&mut pump, &mut handle, &a_copy);
		let mut sub = subscribe(&logical);
		let mut group = a.create_group(group::Info { sequence: 0 }).unwrap();
		group.write_frame(Timestamp::ZERO, b"0.0".to_vec()).unwrap();
		step(&mut pump);
		let mut reading = recv(&mut sub);
		assert_eq!(drain(&mut reading), (vec!["0.0".into()], None));

		let (mut b, b_copy) = copy("b");
		b.start_at(1).unwrap();
		feed(&mut pump, &mut handle, &b_copy);
		for (sequence, secs) in [(1, 10), (2, 60)] {
			let mut next = b.create_group(group::Info { sequence }).unwrap();
			next.write_frame(Timestamp::from_secs(secs).unwrap(), b"x".to_vec())
				.unwrap();
			next.finish().unwrap();
			step(&mut pump);
		}
		assert_eq!(drain(&mut reading), (vec![], Some(Err(Error::Old.to_string()))));
		assert!(a.subscription().is_none(), "the quiet route is let go");
		drop(group);
	}

	/// A transcoder asked for (0, 1) starts at (1, 0) and refuses a mid-group fetch. The
	/// replaced route, still healthy, finishes group 0 itself.
	#[test]
	fn a_replacement_starting_at_the_next_group_leaves_the_open_one_to_its_route() {
		let (mut pump, mut handle, logical) = logical();
		let (a, a_copy) = copy("a");
		feed(&mut pump, &mut handle, &a_copy);
		let mut sub = subscribe(&logical);
		let mut group = a.create_group(group::Info { sequence: 0 }).unwrap();
		write(&mut group, "0.0");
		step(&mut pump);
		let mut reading = recv(&mut sub);

		let (mut b, b_copy) = copy("b");
		b.start_at(1).unwrap();
		feed(&mut pump, &mut handle, &b_copy);
		let mut next = b.create_group(group::Info { sequence: 1 }).unwrap();
		write(&mut next, "1.0");
		next.finish().unwrap();
		step(&mut pump);

		write(&mut group, "0.1");
		group.finish().unwrap();
		step(&mut pump);
		assert_eq!(drain(&mut reading), (vec!["0.0".into(), "0.1".into()], Some(Ok(()))));
		assert_eq!(recv(&mut sub).sequence, 1);
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

	/// A forgotten track reads no new group, but a reader holding one still gets the rest
	/// of it, and only for as long as it holds it. The source is let go with it.
	#[test]
	fn a_forgotten_track_finishes_only_held_groups() {
		let (mut pump, mut handle, logical) = logical();
		let (a, a_copy) = copy("a");
		feed(&mut pump, &mut handle, &a_copy);
		let mut sub = subscribe(&logical);
		let mut group = a.create_group(group::Info { sequence: 0 }).unwrap();
		write(&mut group, "0.0");
		step(&mut pump);
		let mut reading = recv(&mut sub);
		drop((sub, logical));

		assert!(handle.abort_unused(Error::Dropped), "only a group is held");
		step(&mut pump);
		let mut next = a.create_group(group::Info { sequence: 1 }).unwrap();
		write(&mut next, "1.0");
		write(&mut group, "0.1");
		step(&mut pump);
		assert_eq!(drain(&mut reading), (vec!["0.0".into(), "0.1".into()], None));
		assert!(
			next.poll_unused(&kio::Waiter::noop()).is_ready(),
			"a new group was read"
		);

		drop(reading);
		step(&mut pump);
		assert!(
			group.poll_unused(&kio::Waiter::noop()).is_ready(),
			"nothing copies a group nobody reads"
		);
		assert!(a.subscription().is_none(), "the source is let go");
	}

	/// A group the readers' budget would skip is still copied: skipping it, and counting
	/// it as stale, is each reader's own.
	#[test]
	fn stale_groups_are_left_to_the_readers() {
		let (mut pump, mut handle, logical) = logical();
		let (a, a_copy) = copy("a");
		feed(&mut pump, &mut handle, &a_copy);
		let mut sub = subscribe(&logical);

		// Group 0 reaches 60 s, a minute behind the newest frame: past the 30 s budget.
		for (sequence, secs) in [(0, 0), (1, 60), (2, 120)] {
			let mut group = a.create_group(group::Info { sequence }).unwrap();
			group
				.write_frame(Timestamp::from_secs(secs).unwrap(), b"frame".to_vec())
				.unwrap();
			group.finish().unwrap();
		}
		step(&mut pump);

		assert_eq!(logical.cached_sequences(), vec![0, 1, 2], "every group is copied");
		assert_eq!(recv(&mut sub).sequence, 1, "the reader skips the stale one itself");
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

	/// A fetch dropped before it picks up its group lets the group go: nothing keeps the
	/// track read, so the front can release the input.
	#[test]
	fn a_fetch_dropped_before_its_group_lands_releases_the_track() {
		let (mut pump, mut handle, logical) = logical();
		let (a, a_copy) = copy("a");
		let mut writing = a.create_group(group::Info { sequence: 3 }).unwrap();
		write(&mut writing, "head");
		handle.feed(Feed {
			copy: a_copy,
			info: Some(track::Info::default()),
			sub: None,
			floor: None,
		});
		step(&mut pump);

		let fetch = logical.fetch_group(3, None);
		step(&mut pump);
		drop((fetch, logical));
		step(&mut pump);
		assert!(!handle.is_used(), "the pump kept the track read for nobody");
	}

	/// A fetched group whose route dies partway waits, open, for the next input to
	/// continue it, rather than failing on the dead route.
	#[test]
	fn a_fetched_group_waits_for_the_next_input_when_its_route_dies() {
		let (mut pump, mut handle, logical) = logical();
		let (a, a_copy) = copy("a");
		let mut writing = a.create_group(group::Info { sequence: 3 }).unwrap();
		write(&mut writing, "head");
		handle.feed(Feed {
			copy: a_copy,
			info: Some(track::Info::default()),
			sub: None,
			floor: None,
		});
		step(&mut pump);
		let fetch = logical.fetch_group(3, None);
		step(&mut pump);
		let mut reading = fetch.now_or_never().unwrap().unwrap();
		assert_eq!(drain(&mut reading), (vec!["head".into()], None));

		kill(a, [writing]);
		step(&mut pump);
		assert_eq!(drain(&mut reading), (vec![], None), "a dead route failed the group");

		let (b, b_copy) = copy("b");
		let mut replacement = b.create_group(group::Info { sequence: 3 }).unwrap();
		write(&mut replacement, "head");
		write(&mut replacement, "tail");
		replacement.finish().unwrap();
		handle.feed(Feed {
			copy: b_copy,
			info: Some(track::Info::default()),
			sub: None,
			floor: None,
		});
		step(&mut pump);
		assert_eq!(drain(&mut reading), (vec!["tail".into()], Some(Ok(()))));
	}

	/// A detached track reads no new group, but a group a reader holds keeps its copy, and
	/// its source stays subscribed until that group ends. One nobody holds is left open for
	/// the next input to continue.
	#[test]
	fn a_detached_track_keeps_only_held_groups_flowing() {
		let (mut pump, mut handle, logical) = logical();
		let (a, a_copy) = copy("a");
		feed(&mut pump, &mut handle, &a_copy);
		let mut sub = subscribe(&logical);
		let mut held = a.create_group(group::Info { sequence: 0 }).unwrap();
		write(&mut held, "0.0");
		let mut unheld = a.create_group(group::Info { sequence: 1 }).unwrap();
		write(&mut unheld, "1.0");
		step(&mut pump);
		let mut reading = recv(&mut sub);
		assert_eq!(reading.sequence, 0);
		drop((sub, logical));

		handle.detach();
		step(&mut pump);
		assert!(
			unheld.poll_unused(&kio::Waiter::noop()).is_ready(),
			"nothing copies a group nobody holds"
		);

		write(&mut held, "0.1");
		held.finish().unwrap();
		write(&mut unheld, "1.1");
		step(&mut pump);
		assert_eq!(drain(&mut reading), (vec!["0.0".into(), "0.1".into()], Some(Ok(()))));
		assert!(
			a.subscription().is_none(),
			"the source is let go once the held group ends"
		);
		assert_eq!(handle.resume_floor(), Some(Position { group: 1, frame: 1 }));
	}

	/// An idle track owing nothing rejoins at the live edge: what it cached may be long
	/// stale. A returning reader gets the cache only once the next route's answer shows it
	/// leads into its feed; one starting past a gap leaves it to fetches.
	#[test]
	fn an_idle_track_rejoins_at_the_live_edge() {
		let (mut pump, mut handle, logical) = logical();
		let (a, a_copy) = copy("a");
		feed(&mut pump, &mut handle, &a_copy);
		let mut group = a.create_group(group::Info { sequence: 0 }).unwrap();
		write(&mut group, "0.0");
		group.finish().unwrap();
		step(&mut pump);
		drop(logical);
		assert_eq!(handle.resume_floor(), Some(Position::group(1)));

		handle.detach();
		step(&mut pump);
		assert_eq!(handle.resume_floor(), None, "a stale cache asks for no start");

		let logical = handle.weak.try_consume().expect("cached");
		let mut sub = subscribe(&logical);
		assert!(sub.recv_group().now_or_never().is_none(), "the stale cache was served");
		let (mut b, b_copy) = copy("b");
		b.start_at(7).unwrap();
		feed(&mut pump, &mut handle, &b_copy);
		let mut live = b.create_group(group::Info { sequence: 7 }).unwrap();
		write(&mut live, "7.0");
		live.finish().unwrap();
		step(&mut pump);
		assert_eq!(recv(&mut sub).sequence, 7);
		assert!(
			sub.recv_group().now_or_never().is_none(),
			"a cache past a gap was served"
		);
		assert_eq!(handle.resume_floor(), Some(Position::group(8)));
		assert!(
			logical.fetch_group(0, None).now_or_never().is_some(),
			"fetches still find it"
		);
	}

	/// A route whose answer picks up where the hidden cache leaves off brings it back,
	/// ahead of its own groups. The hide waits for a passing lookup to go, so a reader
	/// that saw the cache never gets it twice.
	#[test]
	fn a_connected_route_brings_the_hidden_cache_back() {
		let (mut pump, mut handle, logical) = logical();
		let (a, a_copy) = copy("a");
		feed(&mut pump, &mut handle, &a_copy);
		let mut group = a.create_group(group::Info { sequence: 0 }).unwrap();
		write(&mut group, "0.0");
		group.finish().unwrap();
		step(&mut pump);
		drop(logical);

		// A lookup passes through as the Detach lands: hidden once it is gone.
		let passing = handle.weak.try_consume().expect("cached");
		handle.detach();
		step(&mut pump);
		drop(passing);
		step(&mut pump);

		let logical = handle.weak.try_consume().expect("cached");
		let mut sub = subscribe(&logical);
		assert!(
			sub.recv_group().now_or_never().is_none(),
			"the cache was served before a route vouched for it"
		);
		let (b, b_copy) = copy("b");
		feed(&mut pump, &mut handle, &b_copy);
		let mut next = b.create_group(group::Info { sequence: 1 }).unwrap();
		write(&mut next, "1.0");
		next.finish().unwrap();
		step(&mut pump);
		assert_eq!(recv(&mut sub).sequence, 0);
		assert_eq!(recv(&mut sub).sequence, 1);
	}

	/// A route that dies before answering its subscription judged nothing: the cache stays
	/// hidden, and the next route's answer settles it.
	#[test]
	fn a_route_dying_before_its_answer_keeps_the_cache_hidden() {
		let (mut pump, mut handle, logical) = logical();
		let (a, a_copy) = copy("a");
		feed(&mut pump, &mut handle, &a_copy);
		let mut group = a.create_group(group::Info { sequence: 0 }).unwrap();
		write(&mut group, "0.0");
		group.finish().unwrap();
		step(&mut pump);
		drop(logical);
		handle.detach();
		step(&mut pump);

		let logical = handle.weak.try_consume().expect("cached");
		let mut sub = subscribe(&logical);
		let request = track::Request::new(Arc::new(broadcast::Info::default()), "b");
		let b_copy = request.consume();
		let b = request.resolving_start().accept(None);
		feed(&mut pump, &mut handle, &b_copy);
		assert!(sub.recv_group().now_or_never().is_none(), "revealed before B answered");
		kill(b, Vec::<group::Producer>::new());
		step(&mut pump);
		assert!(sub.recv_group().now_or_never().is_none(), "revealed by a route that never answered");

		let (mut c, c_copy) = copy("c");
		c.start_at(1).unwrap();
		feed(&mut pump, &mut handle, &c_copy);
		assert_eq!(recv(&mut sub).sequence, 0, "C picks up where the cache leaves off");
	}

	/// A group a fetch put in the cache is hidden from readers of the live feed. Once
	/// the live feed delivers it too, they get it.
	#[test]
	fn a_live_delivery_reveals_a_fetched_group() {
		let (mut pump, mut handle, logical) = logical();
		let (a, a_copy) = copy("a");
		let mut three = a.create_group(group::Info { sequence: 3 }).unwrap();
		write(&mut three, "3.0");
		three.finish().unwrap();
		handle.feed(Feed {
			copy: a_copy.clone(),
			info: Some(track::Info::default()),
			sub: None,
			floor: None,
		});
		step(&mut pump);
		let fetching = logical.fetch_group(3, None);
		step(&mut pump);
		step(&mut pump);
		let mut fetched = fetching.now_or_never().expect("resolved").expect("served");
		assert_eq!(drain(&mut fetched), (vec!["3.0".into()], Some(Ok(()))));

		// A reader subscribes; the live feed starts at group 3.
		let mut sub = subscribe(&logical);
		step(&mut pump);
		let mut reading = recv(&mut sub);
		assert_eq!(reading.sequence, 3);
		assert_eq!(drain(&mut reading), (vec!["3.0".into()], Some(Ok(()))));
	}

	/// A group the subscription delivered fails partway while its route stays healthy:
	/// the subscription will not deliver it again, so the rest is fetched.
	#[tokio::test(start_paused = true)]
	async fn a_group_lost_on_a_healthy_route_is_fetched_again() {
		let (pump, mut handle, logical) = logical();
		tokio::spawn(pump.run());
		let request = track::Request::new(Arc::new(broadcast::Info::default()), "a");
		let a_copy = request.consume();
		let groups = request.dynamic();
		let a = request.accept(None);
		let floor = handle.resume_floor();
		handle.feed(Feed {
			copy: a_copy.clone(),
			info: Some(track::Info::default()),
			sub: Some(a_copy.subscribe(Subscription {
				start: floor,
				..replay()
			})),
			floor,
		});
		let mut sub = logical.subscribe(replay()).await.unwrap();

		let mut live = a.create_group(group::Info { sequence: 0 }).unwrap();
		write(&mut live, "0.0");
		let mut reading = sub.recv_group().await.unwrap().unwrap();
		assert_eq!(reading.read_frame().await.unwrap().unwrap().payload, b"0.0".as_ref());
		live.abort(Error::Cancel).unwrap();

		let refetch = tokio::time::timeout(Duration::from_secs(1), groups.requested_group())
			.await
			.expect("the rest is fetched")
			.unwrap();
		assert_eq!((refetch.sequence(), refetch.frame_start()), (0, 1));
		let mut rest = refetch.accept(None).unwrap();
		write(&mut rest, "0.1");
		rest.finish().unwrap();
		assert_eq!(reading.read_frame().await.unwrap().unwrap().payload, b"0.1".as_ref());
		assert!(reading.read_frame().await.unwrap().is_none());
	}

	/// Fetched backfill is not the live feed's: a replacement starts past the newest
	/// live group, whatever a fetch has open below it or cached above it.
	#[tokio::test(start_paused = true)]
	async fn fetched_groups_do_not_move_the_resume_floor() {
		let (pump, mut handle, logical) = logical();
		tokio::spawn(pump.run());
		let request = track::Request::new(Arc::new(broadcast::Info::default()), "a");
		let a_copy = request.consume();
		let groups = request.dynamic();
		let a = request.accept(None);
		handle.feed(Feed {
			copy: a_copy.clone(),
			info: Some(track::Info::default()),
			sub: Some(a_copy.subscribe(replay())),
			floor: None,
		});
		let mut sub = logical.subscribe(replay()).await.unwrap();
		let mut live = a.create_group(group::Info { sequence: 5 }).unwrap();
		write(&mut live, "5.0");
		live.finish().unwrap();
		assert_eq!(sub.recv_group().await.unwrap().unwrap().sequence, 5);

		let mut held = Vec::new();
		for (sequence, finished) in [(2, false), (9, true)] {
			let fetching = tokio::spawn({
				let logical = logical.clone();
				async move { logical.fetch_group(sequence, None).await }
			});
			let upstream = tokio::time::timeout(Duration::from_secs(1), groups.requested_group())
				.await
				.expect("the fetch reaches the copy")
				.unwrap();
			let mut group = upstream.accept(None).unwrap();
			write(&mut group, "backfill");
			if finished {
				group.finish().unwrap();
			}
			held.push((group, fetching.await.unwrap().unwrap()));
		}
		assert_eq!(handle.resume_floor(), Some(Position::group(6)));
	}

	// ---- audit ----

	/// The front let go (its route was retracted) while a reader was mid-track. Once that
	/// reader leaves too, nothing reads the track, so the upstream must be let go.
	#[test]
	fn a_concluded_pump_lets_go_once_unread() {
		let (mut pump, mut handle, logical) = logical();
		let (a, a_copy) = copy("a");
		feed(&mut pump, &mut handle, &a_copy);
		drop(a_copy);
		let sub = subscribe(&logical);
		let mut group = a.create_group(group::Info { sequence: 0 }).unwrap();
		write(&mut group, "0.0");
		group.finish().unwrap();
		step(&mut pump);

		drop(handle);
		step(&mut pump);
		drop((sub, logical));
		let done = pump.poll(&kio::Waiter::noop());
		assert!(
			a.subscription().is_none(),
			"the upstream stays subscribed with nobody reading"
		);
		assert!(done.is_ready(), "the pump outlives every reader");
	}

	/// Datagrams are written into the logical track, so an input that delivered only
	/// datagrams before dying delivered something: it did not refuse the track.
	#[test]
	fn datagrams_count_as_delivered() {
		let (mut pump, mut handle, logical) = logical();
		let (mut a, a_copy) = copy("a");
		feed(&mut pump, &mut handle, &a_copy);
		let mut sub = subscribe(&logical);
		a.insert_datagram(0, Timestamp::ZERO, b"d".to_vec()).unwrap();
		step(&mut pump);
		assert!(sub.recv_datagram().now_or_never().unwrap().unwrap().is_some());

		kill(a, Vec::<group::Producer>::new());
		step(&mut pump);
		let Poll::Ready(ended) = handle.poll_ended(0, &kio::Waiter::noop()) else {
			panic!("not reported")
		};
		assert!(ended.result.is_err());
		assert!(ended.delivered, "the datagram it wrote does not count");
	}

	/// The front picks the replacement's floor when it queries it, but the beaten route
	/// keeps feeding until the replacement's info comes back. A group that route opens
	/// below the floor meanwhile stays its own to finish, and is fetched from the
	/// replacement if it dies first, not given up.
	#[test]
	fn a_group_opened_while_the_replacement_is_queried_survives() {
		let (mut pump, mut handle, logical) = logical();
		let (a, a_copy) = copy("a");
		feed(&mut pump, &mut handle, &a_copy);
		let mut sub = subscribe(&logical);

		let mut five = a.create_group(group::Info { sequence: 5 }).unwrap();
		write(&mut five, "5.0");
		five.finish().unwrap();
		step(&mut pump);
		assert_eq!(recv(&mut sub).sequence, 5);

		// B beats A: the front queries B, subscribing from where the logical track stops.
		let floor = handle.resume_floor();
		assert_eq!(floor, Some(Position::group(6)));
		let (mut b, b_copy) = copy("b");
		let mut b_four = b.create_group(group::Info { sequence: 4 }).unwrap();
		write(&mut b_four, "4.0");
		write(&mut b_four, "4.1");
		b_four.finish().unwrap();
		let b_sub = b_copy.subscribe(Subscription {
			start: floor,
			..replay()
		});

		// While B's info is in flight, A (healthy) delivers group 4, whose stream was slow.
		let mut four = a.create_group(group::Info { sequence: 4 }).unwrap();
		write(&mut four, "4.0");
		step(&mut pump);
		let mut reading = recv(&mut sub);
		assert_eq!(reading.sequence, 4);
		assert_eq!(drain(&mut reading), (vec!["4.0".into()], None));

		// B is spliced in, declaring the start it was asked for.
		b.start_at(6).unwrap();
		handle.feed(Feed {
			copy: b_copy.clone(),
			info: Some(track::Info::default()),
			sub: Some(b_sub),
			floor,
		});
		step(&mut pump);
		assert_eq!(drain(&mut reading), (vec![], None), "A still feeds group 4");

		// A dies partway through it: B starts past group 4, so it is fetched from B.
		kill(a, [four]);
		step(&mut pump);
		assert_eq!(
			drain(&mut reading),
			(vec!["4.1".into()], Some(Ok(()))),
			"B continues group 4, which A was still delivering"
		);
	}

	/// The front parks a track it saw go unread, but a reader may subscribe before the
	/// pump gets the Detach. The cache that reader may have read stays as it is, so it
	/// never gets those groups twice.
	#[test]
	fn a_reader_across_a_park_gets_no_group_twice() {
		let (mut pump, mut handle, logical) = logical();
		let (a, a_copy) = copy("a");
		feed(&mut pump, &mut handle, &a_copy);
		let mut group = a.create_group(group::Info { sequence: 0 }).unwrap();
		write(&mut group, "0.0");
		group.finish().unwrap();
		step(&mut pump);

		// The front saw the last reader leave and parks; a new reader beats the command.
		let mut sub = subscribe(&logical);
		assert_eq!(recv(&mut sub).sequence, 0);
		handle.detach();
		step(&mut pump);

		// The front sees the reader and feeds the serving source again.
		let (_b, b_copy) = copy("b");
		feed(&mut pump, &mut handle, &b_copy);
		step(&mut pump);
		let again = sub.recv_group().now_or_never();
		assert!(
			again.is_none(),
			"group {} delivered twice",
			again.unwrap().unwrap().unwrap().sequence
		);
	}

	/// A session closed locally seals its receive tracks without declaring an end: the
	/// route went away, the content did not end, so the front may still fail over.
	#[test]
	fn a_locally_closed_route_is_not_a_clean_end() {
		let (mut pump, mut handle, logical) = logical();
		let (a, a_copy) = copy("a");
		feed(&mut pump, &mut handle, &a_copy);
		drop(a_copy);
		let mut sub = subscribe(&logical);
		let mut group = a.create_group(group::Info { sequence: 0 }).unwrap();
		write(&mut group, "0.0");
		group.finish().unwrap();
		step(&mut pump);
		assert_eq!(recv(&mut sub).sequence, 0);

		a.close().unwrap();
		step(&mut pump);
		let Poll::Ready(ended) = handle.poll_ended(0, &kio::Waiter::noop()) else {
			panic!("not reported")
		};
		assert!(
			ended.result.is_err(),
			"a route closed locally reads as the track's clean end"
		);
	}

	/// The replacement's subscription replays whatever datagrams its copy still buffers;
	/// those already written are not written again.
	#[test]
	fn a_switch_delivers_no_datagram_twice() {
		let (mut pump, mut handle, logical) = logical();
		let (mut a, a_copy) = copy("a");
		feed(&mut pump, &mut handle, &a_copy);
		let mut sub = subscribe(&logical);
		a.insert_datagram(0, Timestamp::ZERO, b"d0".to_vec()).unwrap();
		step(&mut pump);
		assert_eq!(
			sub.recv_datagram().now_or_never().unwrap().unwrap().unwrap().sequence,
			0
		);

		// B serves the same broadcast and already buffered that datagram.
		let (mut b, b_copy) = copy("b");
		b.insert_datagram(0, Timestamp::ZERO, b"d0".to_vec()).unwrap();
		feed(&mut pump, &mut handle, &b_copy);
		step(&mut pump);
		let again = sub.recv_datagram().now_or_never();
		assert!(
			again.is_none(),
			"datagram {} delivered twice",
			again.unwrap().unwrap().unwrap().sequence
		);
	}

	/// An orphan waiting for the next route can lose its logical group to the cache
	/// (expiry of an idle group, or eviction) while the pump still tracks it. A fetch of
	/// that sequence then writes the fresh group the cache holds.
	#[test]
	fn a_fetch_over_an_evicted_orphan_is_served() {
		let (mut pump, mut handle, logical) = logical();
		let (a, a_copy) = copy("a");
		feed(&mut pump, &mut handle, &a_copy);
		let _sub = subscribe(&logical);
		let mut group = a.create_group(group::Info { sequence: 0 }).unwrap();
		write(&mut group, "0.0");
		step(&mut pump);
		kill(a, [group]);
		step(&mut pump);

		// The idle orphan expires out of the logical cache.
		pump.groups.get(&0).unwrap().dst.clone().abort(Error::Old).unwrap();
		step(&mut pump);

		// A reader fetches it while the front finds a replacement, which has it whole.
		let fetching = logical.fetch_group(0, None);
		step(&mut pump);
		let (b, b_copy) = copy("b");
		let mut whole = b.create_group(group::Info { sequence: 0 }).unwrap();
		write(&mut whole, "0.0");
		write(&mut whole, "0.1");
		whole.finish().unwrap();
		let floor = handle.resume_floor();
		handle.feed(Feed {
			copy: b_copy.clone(),
			info: Some(track::Info::default()),
			sub: Some(b_copy.subscribe(Subscription {
				start: floor,
				..replay()
			})),
			floor,
		});
		step(&mut pump);
		step(&mut pump);

		let mut fetched = fetching.now_or_never().expect("resolved").expect("served");
		assert_eq!(
			drain(&mut fetched),
			(vec!["0.0".into(), "0.1".into()], Some(Ok(()))),
			"the fetched group is never written"
		);
	}

	/// A replacement that no longer has an orphan's group (evicted upstream) declares a
	/// start at or below it but goes on with later groups, and refuses the fetch. Its
	/// subscription might still deliver the group late, so it stays open until the readers'
	/// budget convicts it: it neither stays open for good nor pins the resume floor.
	#[test]
	fn an_orphan_the_replacement_skips_is_given_up_once_stale() {
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

		// B is asked from (0, 1) and declares group 0 as its start (as a pre-06 session
		// does), but its cache lost group 0: its subscription goes on from group 1.
		let (mut b, b_copy) = copy("b");
		b.start_at(0).unwrap();
		feed(&mut pump, &mut handle, &b_copy);
		let mut next = b.create_group(group::Info { sequence: 1 }).unwrap();
		next.write_frame(Timestamp::from_secs(10).unwrap(), b"x".to_vec())
			.unwrap();
		next.finish().unwrap();
		step(&mut pump);
		assert_eq!(recv(&mut sub).sequence, 1);
		assert_eq!(
			drain(&mut reading),
			(vec!["0.0".into()], None),
			"group 0 may still arrive"
		);

		// The track runs on past the readers' 30 s budget.
		let mut later = b.create_group(group::Info { sequence: 2 }).unwrap();
		later
			.write_frame(Timestamp::from_secs(60).unwrap(), b"x".to_vec())
			.unwrap();
		later.finish().unwrap();
		step(&mut pump);
		assert_eq!(drain(&mut reading).1, Some(Err(Error::Old.to_string())));
		assert_eq!(handle.resume_floor(), Some(Position::group(3)));
	}

	/// A returning reader's next input declares its start, delivers a whole group, and
	/// dies, all before the pump task runs. Its group is still written, and it reads as a
	/// route that served the track, not one that refused it.
	#[test]
	fn a_copy_dying_after_its_start_still_counts() {
		let (mut pump, mut handle, logical) = logical();
		let (a, a_copy) = copy("a");
		feed(&mut pump, &mut handle, &a_copy);
		let mut group = a.create_group(group::Info { sequence: 0 }).unwrap();
		write(&mut group, "0.0");
		group.finish().unwrap();
		step(&mut pump);
		drop(logical);
		handle.detach();
		step(&mut pump);

		let logical = handle.weak.try_consume().expect("cached");
		let mut sub = subscribe(&logical);
		let request = track::Request::new(Arc::new(broadcast::Info::default()), "b");
		let b_copy = request.consume();
		let mut b = request.resolving_start().accept(None);
		feed(&mut pump, &mut handle, &b_copy);

		// B says it starts at group 0 (the cache still leads into its feed), delivers
		// group 1 whole, then its session dies.
		b.start_at(0).unwrap();
		let mut one = b.create_group(group::Info { sequence: 1 }).unwrap();
		write(&mut one, "1.0");
		one.finish().unwrap();
		kill(b, Vec::<group::Producer>::new());
		step(&mut pump);

		let Poll::Ready(ended) = handle.poll_ended(1, &kio::Waiter::noop()) else {
			panic!("not reported")
		};
		let got: Vec<u64> = std::iter::from_fn(|| sub.recv_group().now_or_never().and_then(|g| g.ok().flatten()))
			.map(|g| g.sequence)
			.collect();
		assert_eq!(got, vec![0, 1], "B delivered group 1");
		assert!(ended.delivered, "B delivered a whole group but reads as a refusal");
	}

	/// An orphan the cache aborted (idle expiry, eviction) leaves the pump's map. The
	/// replacement, already asked from where the orphan stopped, then delivers its tail,
	/// which must not create the group a second time for a reader that already got it.
	#[test]
	fn an_evicted_orphan_is_not_delivered_twice() {
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

		// B is asked from (0, 2); before its tail lands, the idle orphan expires.
		let (b, b_copy) = copy("b");
		feed(&mut pump, &mut handle, &b_copy);
		assert_eq!(b.subscription().unwrap().start, Some(Position { group: 0, frame: 2 }));
		pump.groups.get(&0).unwrap().dst.clone().abort(Error::Old).unwrap();
		step(&mut pump);

		let mut tail = b.create_group(group::Info { sequence: 0 }).unwrap();
		tail.start_at(2).unwrap();
		write(&mut tail, "0.2");
		tail.finish().unwrap();
		step(&mut pump);

		let again = sub.recv_group().now_or_never().map(|g| {
			let mut g = g.unwrap().unwrap();
			(g.sequence, drain(&mut g))
		});
		assert!(again.is_none(), "group delivered twice: {again:?}");
	}

	/// The replacement is asked from (0, 2) and declares group 0 as its start, but its
	/// copy of group 0 begins at frame 3 (its upstream joined that group late). Frame 2
	/// is fetched from it, and the late copy goes on from frame 3.
	#[test]
	fn a_late_starting_copy_of_the_orphan_is_fetched() {
		let (mut pump, mut handle, logical) = logical();
		let (a, a_copy) = copy("a");
		feed(&mut pump, &mut handle, &a_copy);
		let mut sub = subscribe(&logical);
		let mut group = a.create_group(group::Info { sequence: 0 }).unwrap();
		write(&mut group, "0.0");
		write(&mut group, "0.1");
		step(&mut pump);
		let mut reading = recv(&mut sub);
		kill(a, [group]);
		step(&mut pump);

		let request = track::Request::new(Arc::new(broadcast::Info::default()), "b");
		let b_copy = request.consume();
		let groups = request.dynamic();
		let mut b = request.accept(None);
		b.start_at(0).unwrap();
		feed(&mut pump, &mut handle, &b_copy);
		let mut late = b.create_group(group::Info { sequence: 0 }).unwrap();
		late.start_at(3).unwrap();
		write(&mut late, "0.3");
		step(&mut pump);
		step(&mut pump);

		let asked = groups.poll_requested_group(&kio::Waiter::noop());
		let asked = match asked {
			Poll::Ready(Ok(request)) => Some((request.sequence(), request.frame_start())),
			_ => None,
		};
		assert_eq!(
			asked,
			Some((0, 2)),
			"group 0 is stuck open at frame 2: {:?}",
			drain(&mut reading)
		);
	}

	/// A reader fetches the whole of a live group the logical track joined at frame 2.
	/// Answered now, the fetch's group would take the live one's cache slot, dropping it
	/// from the resume floor and from readers yet to reach it. It waits for the live group
	/// to end instead.
	#[test]
	fn a_fetch_beside_a_live_group_keeps_it_in_the_resume_floor() {
		let (mut pump, mut handle, logical) = logical();
		let request = track::Request::new(Arc::new(broadcast::Info::default()), "a");
		let a_copy = request.consume();
		let groups = request.dynamic();
		let a = request.accept(None);
		let resumed = Subscription {
			start: Some(Position { group: 0, frame: 2 }),
			..replay()
		};
		handle.feed(Feed {
			copy: a_copy.clone(),
			info: Some(track::Info::default()),
			sub: Some(a_copy.subscribe(resumed.clone())),
			floor: None,
		});
		step(&mut pump);
		let mut sub = logical.subscribe(resumed).now_or_never().unwrap().unwrap();

		let mut live = a.create_group(group::Info { sequence: 0 }).unwrap();
		live.start_at(2).unwrap();
		write(&mut live, "0.2");
		step(&mut pump);
		let mut reading = recv(&mut sub);
		reading.start_at(2);
		assert_eq!(drain(&mut reading), (vec!["0.2".into()], None));
		assert_eq!(handle.resume_floor(), Some(Position { group: 0, frame: 3 }));

		let _fetching = logical.fetch_group(0, None);
		step(&mut pump);
		assert!(
			groups.poll_requested_group(&kio::Waiter::noop()).is_pending(),
			"a fetch took the open live group's slot"
		);
		assert_eq!(handle.resume_floor(), Some(Position { group: 0, frame: 3 }));

		live.finish().unwrap();
		step(&mut pump);
		assert_eq!(drain(&mut reading), (vec![], Some(Ok(()))));
		let upstream = match groups.poll_requested_group(&kio::Waiter::noop()) {
			Poll::Ready(Ok(request)) => request,
			_ => panic!("the fetch did not reach A once the live group ended"),
		};
		assert_eq!((upstream.sequence(), upstream.frame_start()), (0, 0));
	}

	/// An input fed for a fetch alone (nobody subscribed yet) dies. The front only fails
	/// over on the input's `Ended`, so without it the fetch retries "on the next input"
	/// that never comes.
	#[test]
	fn an_unsubscribed_input_that_dies_is_reported() {
		let (mut pump, mut handle, logical) = logical();
		let (a, a_copy) = copy("a");
		handle.feed(Feed {
			copy: a_copy,
			info: Some(track::Info::default()),
			sub: None,
			floor: None,
		});
		step(&mut pump);
		let _fetch = logical.fetch_group(3, None);
		step(&mut pump);

		kill(a, Vec::<group::Producer>::new());
		step(&mut pump);
		assert!(
			handle.poll_ended(0, &kio::Waiter::noop()).is_ready(),
			"the dead input is never reported, so the front never fails over"
		);
	}

	/// One reader fetches group 3 from frame 1 (a downstream relay resuming mid-group),
	/// then another fetches the whole group. The second waits for the first group to end
	/// rather than taking its cache slot from under its reader.
	#[test]
	fn a_wider_fetch_does_not_cut_off_a_narrower_one() {
		let (mut pump, mut handle, logical) = logical();
		let request = track::Request::new(Arc::new(broadcast::Info::default()), "a");
		let a_copy = request.consume();
		let groups = request.dynamic();
		let _a = request.accept(None);
		handle.feed(Feed {
			copy: a_copy,
			info: Some(track::Info::default()),
			sub: None,
			floor: None,
		});
		step(&mut pump);
		let upstream = || match groups.poll_requested_group(&kio::Waiter::noop()) {
			Poll::Ready(Ok(request)) => request,
			_ => panic!("the fetch did not reach A"),
		};

		let narrow = logical.fetch_group(3, group::Fetch::default().with_frame_start(1));
		step(&mut pump);
		let mut tail = upstream().accept(None).unwrap();
		write(&mut tail, "3.1");
		step(&mut pump);
		step(&mut pump);
		let mut narrow = narrow.now_or_never().expect("resolved").expect("served");
		assert_eq!(drain(&mut narrow), (vec!["3.1".into()], None));

		let wide = logical.fetch_group(3, None);
		step(&mut pump);
		assert!(groups.poll_requested_group(&kio::Waiter::noop()).is_pending());

		write(&mut tail, "3.2");
		tail.finish().unwrap();
		step(&mut pump);
		assert_eq!(
			drain(&mut narrow),
			(vec!["3.2".into()], Some(Ok(()))),
			"the first fetcher's group was dropped by the second fetch"
		);

		let mut whole = upstream().accept(None).unwrap();
		for frame in ["3.0", "3.1", "3.2"] {
			write(&mut whole, frame);
		}
		whole.finish().unwrap();
		step(&mut pump);
		step(&mut pump);
		let mut wide = wide.now_or_never().expect("resolved").expect("served");
		assert_eq!(
			drain(&mut wide),
			(vec!["3.0".into(), "3.1".into(), "3.2".into()], Some(Ok(())))
		);
	}

	/// A is beaten (its path is slow) partway through a frame and stalls there, still
	/// connected. B, asked from that frame, delivers the rest of the group whole. Every
	/// copy is a candidate, but the copy that started a frame keeps it to itself, so the
	/// group waits on A's stalled stream with B's copy of the frame in hand.
	#[test]
	fn a_stalled_copy_does_not_hold_a_half_written_frame_hostage() {
		let (mut pump, mut handle, logical) = logical();
		let (a, a_copy) = copy("a");
		feed(&mut pump, &mut handle, &a_copy);
		let mut sub = subscribe(&logical);

		let mut group = a.create_group(group::Info { sequence: 0 }).unwrap();
		write(&mut group, "0.0");
		let mut stalled = group
			.create_frame(frame::Info {
				size: 6,
				timestamp: Timestamp::ZERO,
			})
			.unwrap();
		stalled.write(b"foo".to_vec()).unwrap();
		step(&mut pump);
		let mut reading = recv(&mut sub);
		assert_eq!(drain(&mut reading).0, vec!["0.0".to_string()]);
		assert_eq!(handle.resume_floor(), Some(Position { group: 0, frame: 1 }));

		let (b, b_copy) = copy("b");
		feed(&mut pump, &mut handle, &b_copy);
		let mut tail = b.create_group(group::Info { sequence: 0 }).unwrap();
		tail.start_at(1).unwrap();
		write(&mut tail, "foobar");
		write(&mut tail, "0.2");
		tail.finish().unwrap();
		step(&mut pump);

		assert_eq!(
			drain(&mut reading),
			(vec!["foobar".into(), "0.2".into()], Some(Ok(()))),
			"the group waits on the stalled copy's frame"
		);
		drop(stalled);
		drop((a, group));
	}

	/// A opened group 0 and died before sending a frame; readers already hold it from its
	/// head, so B is asked from (0, 0). B's copy of group 0 starts at frame 2 (its upstream
	/// joined that group late). Frames 0 and 1 are owed and B may still serve them by
	/// fetch, as it would were the logical group not empty, rather than the group being
	/// started at frame 2 under the readers.
	#[test]
	fn a_late_copy_of_an_empty_orphan_fetches_its_head() {
		let (mut pump, mut handle, logical) = logical();
		let (a, a_copy) = copy("a");
		feed(&mut pump, &mut handle, &a_copy);
		let mut sub = subscribe(&logical);
		let group = a.create_group(group::Info { sequence: 0 }).unwrap();
		step(&mut pump);
		let mut reading = recv(&mut sub);
		kill(a, [group]);
		step(&mut pump);
		assert_eq!(handle.resume_floor(), Some(Position { group: 0, frame: 0 }));

		let request = track::Request::new(Arc::new(broadcast::Info::default()), "b");
		let b_copy = request.consume();
		let groups = request.dynamic();
		let mut b = request.accept(None);
		b.start_at(0).unwrap();
		feed(&mut pump, &mut handle, &b_copy);
		let mut late = b.create_group(group::Info { sequence: 0 }).unwrap();
		late.start_at(2).unwrap();
		write(&mut late, "0.2");
		step(&mut pump);
		step(&mut pump);

		let asked = match groups.poll_requested_group(&kio::Waiter::noop()) {
			Poll::Ready(Ok(request)) => Some((request.sequence(), request.frame_start())),
			_ => None,
		};
		assert_eq!(
			asked,
			Some((0, 0)),
			"the head of group 0 was skipped under its readers: {:?}",
			drain(&mut reading)
		);
	}

	/// B is asked from (0, 2) and declares group 0, so it owes the orphan's tail. Its
	/// group 1 stream overtakes group 0's (QUIC reordering), so group 0 is fetched, which
	/// B cannot serve yet. The refusal is no verdict while B's subscription may still
	/// deliver it, and group 0's tail then lands on the subscription.
	#[test]
	fn a_reordered_stream_does_not_lose_the_orphan() {
		let (mut pump, mut handle, logical) = logical();
		let (a, a_copy) = copy("a");
		feed(&mut pump, &mut handle, &a_copy);
		let mut sub = subscribe(&logical);
		let mut group = a.create_group(group::Info { sequence: 0 }).unwrap();
		write(&mut group, "0.0");
		write(&mut group, "0.1");
		step(&mut pump);
		let mut reading = recv(&mut sub);
		kill(a, [group]);
		step(&mut pump);

		let (mut b, b_copy) = copy("b");
		b.start_at(0).unwrap();
		feed(&mut pump, &mut handle, &b_copy);
		let mut one = b.create_group(group::Info { sequence: 1 }).unwrap();
		write(&mut one, "1.0");
		one.finish().unwrap();
		step(&mut pump);
		let mut tail = b.create_group(group::Info { sequence: 0 }).unwrap();
		tail.start_at(2).unwrap();
		write(&mut tail, "0.2");
		tail.finish().unwrap();
		step(&mut pump);

		assert_eq!(
			drain(&mut reading),
			(vec!["0.0".into(), "0.1".into(), "0.2".into()], Some(Ok(()))),
			"group 0 given up while its tail was in flight"
		);
	}
}
