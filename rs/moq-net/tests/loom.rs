#![cfg(feature = "async")]

//! Loom model checks for the model layer (broadcast -> track -> group -> frame).
//!
//! The unit tests drive these handles from one task at a time, so they can only
//! catch logic bugs. In production a publisher writes on one thread while a
//! session driver reads on another, and the handoff is kio's `Arc<Mutex<State>>`
//! plus a waker list. These tests hand each side to its own loom thread and let
//! loom permute the interleavings.
//!
//! A lost wakeup shows up as loom reporting a deadlock (every thread parked, none
//! runnable), so "the test terminates" is itself the assertion. Anything else is
//! asserted directly.
//!
//! The model mutexes and kio's primitives use Loom's locks, so a contending thread
//! yields to its scheduler. Reference counts that need weak pointers and moq-net's
//! bare atomic counters still use std: these models cover the locking and wakeup
//! handoffs, not every possible reordering of the cache's counters.
//!
//! Unlike kio's own models, these are bounded (see [`model`]): each one composes
//! dozens of kio operations, so an exhaustive search grows exponentially with every
//! lock or refcount the model layer adds to a path.
//!
//! Run with `just rs loom`; the whole file compiles away without `cfg(loom)`.
#![cfg(loom)]

use bytes::Bytes;
use loom::{future::block_on, thread};
use moq_net::{Error, Hop, Timestamp, broadcast, cache, origin};

/// A millisecond track: the frames written here carry timestamps.
fn timed() -> moq_net::track::Info {
	moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

/// Preemptions per execution unless `LOOM_MAX_PREEMPTIONS` overrides it. Loom's own
/// guidance is that 2 or 3 catches most bugs, and each extra level multiplies the
/// search several times over. Unbounded, `back_to_back_groups_arrive_in_order` no
/// longer finishes within an hour.
const PREEMPTION_BOUND: usize = 3;

/// [`loom::model`] with a preemption bound, so the search stays polynomial in the
/// number of operations on a path rather than exponential.
fn model<F: Fn() + Sync + Send + 'static>(f: F) {
	let mut builder = loom::model::Builder::new();
	// The origin models include setup and teardown in each execution. Their finite
	// paths exceed Loom's default 1,000 branches; this raises the execution budget,
	// not the number of schedules or preemptions explored.
	if std::env::var_os("LOOM_MAX_BRANCHES").is_none() {
		builder.max_branches = 10_000;
	}
	builder.preemption_bound.get_or_insert(PREEMPTION_BOUND);
	builder.check(f);
}

/// A frame written on the publisher thread must reach a subscriber parked on
/// `next_frame`, however the write interleaves with the reader's parking.
#[test]
fn frame_reaches_a_parked_subscriber() {
	model(|| {
		let broadcast = broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let track = broadcast.create_track("video", timed()).expect("create track");
		// Subscribed before the publisher runs: a bare handle is no demand, so a track
		// finished and dropped before the subscribe would already be gone.
		let subscribing = consumer.track("video").expect("track").subscribe(None);

		let publisher = thread::spawn(move || {
			let mut group = track.append_group().expect("append group");
			group
				.write_frame(Timestamp::ZERO, Bytes::from_static(b"frame"))
				.expect("write frame");
			group.finish().expect("finish group");
			track.finish().expect("finish track");
		});

		let mut subscriber = block_on(subscribing).expect("subscribe");
		let mut group = block_on(subscriber.recv_group())
			.expect("recv group")
			.expect("a group was published");
		let frame = block_on(group.read_frame()).expect("read frame").expect("a frame");
		assert_eq!(frame.payload, Bytes::from_static(b"frame"));

		publisher.join().unwrap();
	});
}

/// Two groups written back to back must both reach the subscriber in order. This is
/// the path where the producer takes the waiter list for group 1 and wakes outside
/// the lock while group 2 is already being written.
///
/// The subscription retains the backlog from group 0: a floor alone does not keep
/// superseded groups within a zero delay budget.
#[test]
fn back_to_back_groups_arrive_in_order() {
	model(|| {
		let broadcast = broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let track = broadcast.create_track("video", None).expect("create track");
		let from_start = moq_net::track::Subscription::default()
			.with_range(0..)
			.with_max_delay(std::time::Duration::MAX);
		let subscribing = consumer.track("video").expect("track").subscribe(from_start);

		let publisher = thread::spawn(move || {
			for _ in 0..2 {
				let mut group = track.append_group().expect("append group");
				group.finish().expect("finish group");
			}
			track.finish().expect("finish track");
		});

		let mut subscriber = block_on(subscribing).expect("subscribe");
		let first = block_on(subscriber.recv_group()).expect("recv").expect("first group");
		let second = block_on(subscriber.recv_group()).expect("recv").expect("second group");
		assert!(second.sequence > first.sequence, "groups arrived out of order");

		publisher.join().unwrap();
	});
}

/// A track scan reads the group's abort through a mirrored flag rather than the
/// group's state, so the two can disagree. The direction that disagreement is allowed
/// to run is the invariant: the mirror may lag the state (a scan hands out a group
/// that is already aborting, and its consumer surfaces the abort), but it may never
/// lead it (a scan drops a group a reader could still drain).
///
/// So the racing scan asserts against the group's *own state*, not against the flag it
/// just read: whenever the scan skips the group, a consumer must agree the group is
/// aborted. Only the abort transition takes the loom-backed group lock while delivery
/// takes the loom-backed track lock, which is what lets loom permute the two sides.
///
/// The mirror itself is a bare `std` atomic, which loom does not instrument (see the
/// module docs). It cannot be: any cross-thread look at the group's state has to take
/// the group lock the abort is holding, so a store moved earlier *within* that guard
/// is unobservable by construction. What is observable, and what this pins down, is a
/// store escaping the guard entirely.
#[test]
fn group_abort_flag_never_leads_the_group_state() {
	model(|| {
		let broadcast = broadcast::Info::new().produce();
		let track = broadcast.create_track("video", None).expect("create track");
		let group = track.append_group().expect("append group");
		let mut before = track.subscribe(None);
		let mut racing = track.subscribe(None);
		let mut after = track.subscribe(None);
		track.finish().expect("finish track");

		assert!(matches!(
			before.poll_recv_group(&kio::Waiter::noop()),
			std::task::Poll::Ready(Ok(Some(_)))
		));

		// The group holds no frames and the track is finished, so this consumer parks
		// while the group is live and errors once the abort reaches the state itself.
		let mut observer = group.consume();
		let aborter = thread::spawn(move || group.abort(Error::Cancel).expect("abort group"));

		match racing.poll_recv_group(&kio::Waiter::noop()) {
			// Skipped, so the scan read the mirror as aborted. The state must already
			// agree, or the mirror led it and this scan dropped a readable group.
			std::task::Poll::Ready(Ok(None)) => assert!(
				matches!(
					observer.poll_next_frame(&kio::Waiter::noop()),
					std::task::Poll::Ready(Err(_))
				),
				"a scan skipped a group whose own state is not aborted"
			),
			// Delivered, which the mirror is allowed to do right up until the abort
			// commits; the consumer is what surfaces it.
			std::task::Poll::Ready(Ok(Some(_))) => {}
			_ => panic!("a finalized track cannot park during the abort race"),
		}
		aborter.join().unwrap();

		assert!(matches!(
			after.poll_recv_group(&kio::Waiter::noop()),
			std::task::Poll::Ready(Ok(None))
		));
	});
}

/// A publisher parked on `Demand::wanted_changed` drives on-demand capture, so a subscriber
/// appearing on another thread must always wake it.
#[test]
fn subscriber_wakes_parked_demand() {
	model(|| {
		let broadcast = broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let track = broadcast.create_track("video", None).expect("create track");
		let mut demand = track.demand();

		let subscriber = thread::spawn(move || {
			let track_consumer = consumer.track("video").expect("track");
			// Held until joined, so it outlives the `wanted_changed()` poll.
			block_on(track_consumer.subscribe(None)).expect("subscribe")
		});

		while block_on(demand.wanted_changed())
			.expect("the new subscriber was missed")
			.is_none()
		{}

		drop(subscriber.join().unwrap());
		drop(track);
	});
}

/// A publisher waiting on `broadcast::Demand` while one reader hands over to the next:
/// the last reader leaves and a new one arrives straight from the track producer, which
/// never touches the broadcast's own state. Whatever the publisher's poll saw, it must be
/// woken for the newcomer.
#[test]
fn broadcast_demand_sees_a_reader_return() {
	model(|| {
		let broadcast = broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let video = broadcast.create_track("video", None).expect("create track");
		let mut demand = broadcast.demand();
		let first = block_on(consumer.track("video").expect("track").subscribe(None)).expect("subscribe");

		let reader = thread::spawn(move || {
			drop(first);
			let next = video.subscribe(None);
			// Returned, so neither the reader nor the track goes before the join.
			(video, next)
		});

		// The first call reports whatever the swap left; a reader gone must not stay unseen.
		while !block_on(demand.wanted_changed()).expect("the returning reader was missed") {}
		drop(reader.join().unwrap());
	});
}

/// A publisher waiting on `broadcast::Demand` while a reader comes and goes straight
/// through the track producer: its leaving must wake the publisher.
#[test]
fn broadcast_demand_sees_a_reader_leave() {
	model(|| {
		let broadcast = broadcast::Info::new().produce();
		let video = broadcast.create_track("video", None).expect("create track");
		let mut demand = broadcast.demand();
		let reader = video.subscribe(None);

		let leaver = thread::spawn(move || {
			drop(reader);
			video
		});

		while block_on(demand.wanted_changed()).expect("the reader leaving was missed") {}
		drop(leaver.join().unwrap());
	});
}

/// A reader taking and dropping a track around the front's demand poll. The front must
/// see it go: one that missed the edge keeps its source subscribed for nobody, which
/// shows up here as a deadlock waiting for the source to go unread.
///
/// A lookup today also writes the broadcast state the front watches for new tracks,
/// which wakes it regardless, so this only bites once a lookup stops writing. The
/// front's demand poll must not lean on that wake.
#[test]
fn a_front_sees_a_reader_come_and_go() {
	model(|| {
		let (producer, mut driver) = origin::Producer::new(origin::Config::new(Hop::new(1).unwrap()));
		let consumer = producer.consume();
		let source = producer.publish("room", origin::Route::default()).unwrap();
		let video = source.create_track("video", None).expect("create track");
		let demand = video.demand();
		let noop = kio::Waiter::noop();
		let now = moq_net::time::Instant::now();

		let pending = consumer.request_broadcast("room", None);
		driver.poll(now, &noop).unwrap();
		let room = block_on(pending).expect("resolves");

		// A first reader gets the source asked, then leaves.
		let first = room.track("video").expect("track").info();
		driver.poll(now, &noop).unwrap();
		assert!(demand.wanted().is_some(), "the front never asked the source");
		drop(first);

		let racing = room.clone();
		let reader = thread::spawn(move || drop(racing.track("video").expect("track").info()));
		driver.poll(now, &noop).unwrap();
		reader.join().unwrap();

		block_on(std::future::poll_fn(|cx| {
			driver.poll(now, &kio::Waiter::new(cx.waker().clone())).unwrap();
			match demand.wanted() {
				Some(_) => std::task::Poll::Pending,
				None => std::task::Poll::Ready(()),
			}
		}));
	});
}

/// The other direction: with the track parked, one reader leaves and another arrives
/// around the front's demand poll. A front that missed the arrival leaves the newcomer
/// stalled on a parked track, which shows up here as a deadlock waiting for the source
/// to be asked again. Like `a_front_sees_a_reader_come_and_go`, it must hold without
/// the lookup's incidental wake.
#[test]
fn a_front_sees_a_reader_return() {
	model(|| {
		let (producer, mut driver) = origin::Producer::new(origin::Config::new(Hop::new(1).unwrap()));
		let consumer = producer.consume();
		let source = producer.publish("room", origin::Route::default()).unwrap();
		let video = source.create_track("video", None).expect("create track");
		let demand = video.demand();
		let noop = kio::Waiter::noop();
		let now = moq_net::time::Instant::now();

		let pending = consumer.request_broadcast("room", None);
		driver.poll(now, &noop).unwrap();
		let room = block_on(pending).expect("resolves");

		// A first reader comes and goes, which parks the track.
		let first = room.track("video").expect("track").info();
		driver.poll(now, &noop).unwrap();
		drop(first);
		driver.poll(now, &noop).unwrap();
		assert!(demand.wanted().is_none(), "the front kept the source for nobody");

		let leaving = room.track("video").expect("track").info();
		let racing = room.clone();
		let reader = thread::spawn(move || {
			drop(leaving);
			racing.track("video").expect("track").info()
		});
		driver.poll(now, &noop).unwrap();
		let returned = reader.join().unwrap();

		block_on(std::future::poll_fn(|cx| {
			driver.poll(now, &kio::Waiter::new(cx.waker().clone())).unwrap();
			match demand.wanted() {
				Some(_) => std::task::Poll::Ready(()),
				None => std::task::Poll::Pending,
			}
		}));
		drop(returned);
	});
}

/// Two tracks publish into one bounded pool from separate threads, and the pool must
/// be back to zero once every handle is gone: a charge that outlives its group is how
/// a cache leaks, and loom's Arc-leak check catches the reference-cycle flavor of the
/// same bug.
///
/// This does not model the charge accounting itself. `cache::Pool` uses standard
/// atomics, so loom permutes the two tracks only where they meet in kio. Reordering
/// the pool's own counters would need those swapped for loom's atomics too.
#[test]
fn concurrent_tracks_drain_a_shared_pool() {
	model(|| {
		let config = cache::Config::default()
			.with_capacity(512)
			.with_expiry(cache::DEFAULT_EXPIRY);
		let pool = cache::Pool::new(config);
		let mut info = broadcast::Info::new();
		info.pool = pool.clone();
		let broadcast = info.produce();

		let handles: Vec<_> = ["video", "audio"]
			.into_iter()
			.map(|name| {
				let track = broadcast.create_track(name, timed()).expect("create track");
				thread::spawn(move || {
					let mut group = track.append_group().expect("append group");
					group
						.write_frame(Timestamp::ZERO, Bytes::from_static(b"0123456789"))
						.expect("write frame");
					group.finish().expect("finish group");
					track.finish().expect("finish track");
				})
			})
			.collect();

		for handle in handles {
			handle.join().unwrap();
		}
		broadcast.close();
		drop(broadcast);

		assert_eq!(pool.used(), 0, "the pool kept a charge after every group was dropped");
	});
}

/// The teardown every wire subscriber runs when its copy of a track goes idle,
/// racing a viewer coming back for the same name.
///
/// `poll_unused` is a level snapshot, so the teardown it wakes can only be trusted
/// at the instant it commits. The invariant is that the two can't both win: either
/// the subscribe joins the live track and the teardown declines, or the teardown
/// commits and the subscribe never sees the dead track at all. A subscription joined
/// and then cancelled by that teardown is the bug, and it shows up here as the
/// teardown's `Cancel` reaching the viewer.
#[test]
fn an_idle_teardown_never_cancels_a_returning_viewer() {
	model(|| {
		let broadcast = broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		// Nothing is consuming it, which is the wake the teardown acts on.
		let track = broadcast.create_track("video", None).expect("create track");

		// A bare handle is no demand, so the subscribe is what races the teardown.
		let viewer = thread::spawn(move || consumer.track("video").map(|track| track.subscribe(None).into_inner()));

		// Held to the end: a declined teardown hands back the producer still serving.
		let teardown = track.abort_unused(Error::Cancel);
		let subscribed = viewer.join().unwrap().and_then(|subscribing| {
			match subscribing.poll_ok(&kio::Waiter::noop()) {
				// A live track parks (nothing has been published); only an error matters.
				std::task::Poll::Ready(Ok(mut subscriber)) => match subscriber.poll_recv_group(&kio::Waiter::noop()) {
					std::task::Poll::Ready(Err(err)) => Err(err),
					_ => Ok(()),
				},
				std::task::Poll::Ready(Err(err)) => Err(err),
				std::task::Poll::Pending => Ok(()),
			}
		});

		// The track was already gone, so nothing was joined to cancel. Nobody serves
		// this broadcast on demand, so the re-request has nowhere to go.
		if let Err(err) = subscribed {
			assert!(teardown.is_ok(), "a declined teardown refused a viewer");
			assert!(
				matches!(err, Error::NotFound),
				"the teardown cancelled a subscription it raced: {err}"
			);
		}
	});
}

/// Dropping the publisher must resolve a subscriber parked on `recv_group` rather
/// than leaving it waiting for a group that will never come.
#[test]
fn publisher_drop_resolves_a_parked_subscriber() {
	model(|| {
		let broadcast = broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let track = broadcast.create_track("video", None).expect("create track");
		let subscribing = consumer.track("video").expect("track").subscribe(None);

		let publisher = thread::spawn(move || drop(track));

		let mut subscriber = block_on(subscribing).expect("subscribe");
		// Ok(None) on a clean finish, Err on an abort; either resolves the park.
		let _ = block_on(subscriber.recv_group());

		publisher.join().unwrap();
	});
}

/// The last `fetch_group` caller leaving, racing a new fetch of the same group while
/// the handler drops the request the moment it sees it unused.
///
/// The leaver withdraws the attempt under the fetch lock a join takes, so the new
/// fetch either joined the live attempt (keeping its request wanted) or queued a fresh
/// one. A fetch joining the withdrawn attempt and failing as its handler drops it is
/// the bug, and shows up as `Err` here.
#[test]
fn a_fetch_never_joins_a_withdrawn_attempt() {
	model(|| {
		let broadcast = broadcast::Info::new().produce();
		let track = broadcast.create_track("video", None).expect("create track");
		let dynamic = track.requests();
		let consumer = track.consume();

		let first = consumer.fetch_group(5, None);
		let request = block_on(dynamic.requested_fill()).expect("requested");

		let leaver = thread::spawn(move || {
			drop(first);
			request.is_used().then_some(request)
		});
		let mut retry = consumer.fetch_group(5, None);
		let request = match leaver.join().unwrap() {
			Some(request) => request,
			None => {
				let polled = kio::Task::poll(&mut *retry, &kio::Waiter::noop());
				if let std::task::Poll::Ready(Err(err)) = polled {
					panic!("the retry joined a withdrawn attempt: {err}");
				}
				block_on(dynamic.requested_fill()).expect("the retry queued a fresh request")
			}
		};
		request.accept(5, None).expect("accept").finish().expect("finish");
		block_on(retry).expect("the retry resolves");
	});
}

/// A request racing the front it joins as that front retires: the last holder lets go
/// while the origin's driver runs. The request either holds the front in time, which
/// keeps it, or finds it closed and mints a fresh one. Being handed the broadcast of a
/// front that is ending is the bug, and shows up here as a closed broadcast.
#[test]
fn a_request_never_joins_a_retiring_front() {
	model(|| {
		let (producer, mut driver) = origin::Producer::new(origin::Config::new(Hop::new(1).unwrap()));
		let consumer = producer.consume();
		let _broadcast = producer.publish("room", origin::Route::default()).unwrap();
		let waiter = kio::Waiter::noop();
		let now = moq_net::time::Instant::now();

		let pending = consumer.request_broadcast("room", None);
		driver.poll(now, &waiter).unwrap();
		let first = block_on(pending).expect("resolves");

		let requester = thread::spawn(move || consumer.request_broadcast("room", None));
		drop(first);
		driver.poll(now, &waiter).unwrap();
		let pending = requester.join().unwrap();
		driver.poll(now, &waiter).unwrap();
		let resolved = block_on(pending).expect("resolves");
		assert!(!resolved.is_closed(), "handed a front that is ending");
	});
}

/// A claim answer published while its claim retracts on another thread. The answer
/// either lands before the retraction, and stays announced, or is refused without
/// ever being announced. Announcing it and withdrawing it again shows up here as a
/// route seen mid-race on an answer that was refused.
#[test]
fn a_refused_answer_is_never_announced() {
	model(|| {
		let (producer, mut driver) = origin::Producer::new(origin::Config::new(Hop::new(1).unwrap()));
		let claim = producer.requests("room").unwrap();
		let pending = producer.consume().request_broadcast("room/alice", None);
		driver
			.poll(moq_net::time::Instant::now(), &kio::Waiter::noop())
			.unwrap();
		let prepared = block_on(claim.requested_broadcast())
			.expect("requested")
			.prepare(moq_net::Epoch::mint())
			.expect("prepare");
		// Keep a refused answer alive so dropping it cannot hide a stray announcement.
		let _answer = prepared.broadcast().clone();
		let announced = |consumer: &origin::Consumer| {
			let mut route = std::pin::pin!(consumer.routed("room/alice"));
			matches!(
				kio::Waiter::noop().poll_future(route.as_mut()),
				std::task::Poll::Ready(Some(_))
			)
		};

		let watch = producer.consume();
		let releaser = thread::spawn(move || {
			drop(claim);
			announced(&watch)
		});
		let published = prepared.publish();
		let seen = releaser.join().unwrap();

		assert_eq!(
			published.is_ok(),
			announced(&producer.consume()),
			"the route outlived its answer"
		);
		assert!(!seen || published.is_ok(), "a refused answer was announced");
		drop(pending);
	});
}

/// Read a track whose source already aborted with `read`, racing the front ending it.
/// A reader that saw no end, then found one set when it went to park, parked on nothing
/// and never woke, which shows up here as a deadlock. `read` says whether the refusal
/// surfaced as an error.
fn refused_track_race(read: fn(&mut moq_net::track::Subscriber) -> bool) {
	model(move || {
		let (producer, mut driver) = origin::Producer::new(origin::Config::new(Hop::new(1).unwrap()));
		let consumer = producer.consume();
		let source = producer.publish("room", origin::Route::default()).unwrap();
		let video = source.create_track("video", None).expect("create track");
		let noop = kio::Waiter::noop();
		let now = moq_net::time::Instant::now();

		let pending = consumer.request_broadcast("room", None);
		driver.poll(now, &noop).unwrap();
		let room = block_on(pending).expect("resolves");

		let subscribing = room.track("video").expect("track").subscribe(None);
		driver.poll(now, &noop).unwrap();
		let mut subscriber = block_on(subscribing).expect("subscribe");

		video.abort(Error::NotFound).unwrap();
		let reader = thread::spawn(move || read(&mut subscriber));
		driver.poll(now, &noop).unwrap();
		assert!(reader.join().unwrap(), "a refused track read as an end");
	});
}

#[test]
fn a_group_reader_sees_the_front_end_a_refused_track() {
	refused_track_race(|subscriber| block_on(subscriber.recv_group()).is_err());
}

#[test]
fn a_finished_reader_sees_the_front_end_a_refused_track() {
	refused_track_race(|subscriber| block_on(subscriber.finished()).is_err());
}

/// An abort racing the first parking poll must either be observed or wake the cursor.
#[test]
fn an_aborted_parked_group_releases_a_finished_track() {
	model(|| {
		let broadcast = broadcast::Info::new().produce();
		let track = broadcast.create_track("parked", timed()).unwrap();
		let mut group = track.append_group().unwrap();
		group.finish().unwrap();
		let mut subscriber = track.subscribe(moq_net::track::Subscription::default().with_range(0..));
		subscriber = subscriber.with_range(..0);
		track.finish().unwrap();
		let aborting = thread::spawn(move || group.abort(Error::Old).unwrap());
		assert!(block_on(subscriber.recv_group()).unwrap().is_none());
		aborting.join().unwrap();
	});
}

/// An arrival takes the track write lock; revealing an already cached group only takes
/// its read lock. Either path must observe a registered expiry read or leave the
/// successor visible when that read judges it. The landing hint uses Loom atomics.
fn landing_wakes_a_parked_expiry(reveal: bool) {
	model(move || {
		let broadcast = broadcast::Info::new().produce();
		let track = broadcast.create_track("video", timed()).unwrap();
		let mut head = track.append_group().unwrap();
		head.write_frame(Timestamp::ZERO, Bytes::from_static(b"head")).unwrap();
		let mut subscriber = track.subscribe(
			moq_net::track::Subscription::default()
				.with_range(0..)
				.with_max_delay(std::time::Duration::from_millis(500)),
		);
		let mut held = block_on(subscriber.recv_group()).unwrap().unwrap();
		assert!(block_on(held.read_frame()).unwrap().is_some());

		let landing = if reveal {
			let landing = track.append_group().unwrap();
			let mut edge = track.append_group().unwrap();
			edge.write_frame(Timestamp::from_millis(2000).unwrap(), Bytes::from_static(b"edge"))
				.unwrap();
			edge.finish().unwrap();
			Some(landing)
		} else {
			let mut successor = track.append_group().unwrap();
			successor
				.write_frame(Timestamp::from_millis(1000).unwrap(), Bytes::from_static(b"successor"))
				.unwrap();
			successor.finish().unwrap();
			None
		};
		let publisher = thread::spawn(move || {
			let mut landing = landing.unwrap_or_else(|| track.append_group().unwrap());
			let at = Timestamp::from_millis(if reveal { 1000 } else { 2000 }).unwrap();
			landing.write_frame(at, Bytes::from_static(b"landing")).unwrap();
			landing.finish().unwrap();
			(track, landing)
		});
		assert!(matches!(block_on(held.read_frame()), Err(Error::Old)));
		drop(publisher.join().unwrap());
	});
}

#[test]
fn a_group_arrival_landing_wakes_parked_expiry() {
	landing_wakes_a_parked_expiry(false);
}

#[test]
fn a_group_reveal_landing_wakes_parked_expiry() {
	landing_wakes_a_parked_expiry(true);
}
