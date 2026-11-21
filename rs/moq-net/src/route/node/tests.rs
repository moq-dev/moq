use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use super::*;
use crate::route::relay::{Edit, Hop, Offer, Role};

fn id(relay: u64) -> NodeId {
	NodeId {
		relay,
		start: 1,
		marked: false,
	}
}

fn node() -> (Node, Deadlines) {
	let clock = Deadlines::new(Instant::now());
	(Node::new(id(1), true, clock.clone()), clock)
}

/// A link to a neighbour in `role`.
fn port(role: Role) -> Port {
	Port {
		cost: 1,
		priority: 0,
		upstream: false,
		role,
	}
}

/// A neighbour that forwards nothing and is asked for everything.
fn publisher(node: &Node, tag: u64) -> Stream {
	let stream = node.connect(tag, port(Role::Scoped(vec![Path::empty()]))).unwrap();
	assert!(matches!(stream.try_wire(), Some(Msg::Ask(_))), "the node asks at once");
	stream.recv(Msg::Edited).unwrap();
	stream
}

/// A neighbour that forwards nothing, is asked for nothing, and asks for everything.
fn viewer(node: &Node, tag: u64) -> Stream {
	let stream = node.connect(tag, port(Role::Scoped(Vec::new()))).unwrap();
	let ask = Edit {
		remove: Vec::new(),
		add: vec![Path::empty()],
	};
	stream.recv(Msg::Ask(ask)).unwrap();
	stream.flush();
	stream
}

fn cast(path: &str) -> Broadcast {
	(Path::new(path).unwrap(), "i".parse().unwrap())
}

fn live(epoch: u64) -> Change {
	Change::Live(Claim {
		item: Attr { epoch, above: 0 },
		cost: 0,
		priority: i8::MAX,
		draining: false,
		unavailable: false,
		chain: Vec::new(),
		marked: false,
	})
}

/// `stream`'s peer announces `change` to `broadcast`.
fn announce(stream: &Stream, broadcast: &Broadcast, change: Change) {
	let changes = vec![(broadcast.clone(), change)];
	stream.recv(Msg::Announce { changes }).unwrap();
	stream.flush();
}

/// The claims queued for `stream`'s peer: each path, and whether it is live.
fn told(stream: &Stream) -> Vec<(String, bool)> {
	std::iter::from_fn(|| stream.try_wire())
		.filter_map(|msg| match msg {
			Msg::Announce { changes } => Some(changes),
			_ => None,
		})
		.flatten()
		.map(|((path, _), change)| (path.to_string(), matches!(change, Change::Live(_))))
		.collect()
}

fn serves(stream: &Stream) -> Vec<Output> {
	std::iter::from_fn(|| stream.try_serve()).collect()
}

/// Confirm every claim `stream` was handed, returning how many there were.
fn confirm(stream: &Stream) -> usize {
	let serves = serves(stream);
	for serve in &serves {
		if let Output::Serve {
			origin, path, instance, ..
		} = serve
		{
			stream.serving(*origin, &(path.clone(), instance.clone()), true);
		}
	}
	serves.len()
}

/// Fire the node's next timer.
fn expire(node: &Node, clock: &Deadlines) {
	let Poll::Ready(Some(at)) = node.poll_deadline(None, &kio::Waiter::noop()) else {
		panic!("no timer is armed");
	};
	clock.advance(at);
	node.timeout();
}

/// What a closed session left behind, and how the node let go of it.
struct Left(Arc<AtomicU8>);

impl Left {
	const HELD: u8 = 0;
	/// Another session serves the claim.
	const MOVED: u8 = 1;
	/// The node let go of the claim, and every copy with it.
	const LOST: u8 = 2;
	const ENDED: u8 = 3;
	const WITHDRAWN: u8 = 4;
}

impl Orphan for Left {
	fn release(self: Box<Self>, gone: Gone) {
		let how = match gone {
			Gone::Moved => Self::MOVED,
			Gone::Lost => Self::LOST,
			Gone::Ended => Self::ENDED,
			Gone::Withdrawn => Self::WITHDRAWN,
		};
		self.0.store(how, Ordering::SeqCst);
	}
}

/// The origin's own claims are told to a session that asks for them, and are never
/// handed to a session to serve: the origin's table already holds them.
#[test]
fn own_claims_are_told_and_never_served() {
	let (node, _clock) = node();
	let room = cast("room");
	node.own(false, room.clone(), live(0));
	let viewer = viewer(&node, 1);
	assert_eq!(told(&viewer), [("room".to_string(), true)]);
	assert!(serves(&viewer).is_empty());

	node.own(false, room, Change::Ended);
	assert_eq!(told(&viewer), [("room".to_string(), false)]);
}

/// An own claim refiled as having crossed a marked link leaves its unmarked filing, so a
/// marked neighbour stops being told it, and is told it again once filed back.
#[test]
fn an_own_claim_refiled_as_marked_is_withdrawn_from_a_marked_neighbour() {
	let (node, _clock) = node();
	let room = cast("room");
	let marked = Port {
		upstream: true,
		..port(Role::Scoped(Vec::new()))
	};
	let viewer = node.connect(1, marked).unwrap();
	let ask = Edit {
		remove: Vec::new(),
		add: vec![Path::empty()],
	};
	viewer.recv(Msg::Ask(ask)).unwrap();
	viewer.flush();
	node.own(false, room.clone(), live(0));
	assert_eq!(told(&viewer), [("room".to_string(), true)]);
	node.own(true, room.clone(), live(0));
	assert_eq!(told(&viewer), [("room".to_string(), false)]);
	node.own(false, room, live(0));
	assert_eq!(told(&viewer), [("room".to_string(), true)]);
}

/// A claim a neighbour announces is handed back to that session to serve, naming no
/// node, since the neighbour is where requests for it go.
#[test]
fn a_claim_is_served_through_the_session_that_gave_it() {
	let (node, _clock) = node();
	let room = cast("room");
	let publisher = publisher(&node, 1);
	let viewer = viewer(&node, 2);
	announce(&publisher, &room, live(0));

	let serves = serves(&publisher);
	let [Output::Serve { path, served, .. }] = serves.as_slice() else {
		panic!("one claim to serve: {serves:?}");
	};
	assert_eq!((path, served.named, served.available), (&room.0, None, true));
	assert_eq!(told(&viewer), [("room".to_string(), true)]);
	assert!(self::serves(&viewer).is_empty(), "a viewer serves nothing");
	// It is never told back to the session it came from.
	assert!(told(&publisher).is_empty());
}

/// Of two sessions giving a claim, only the one whose copy is closest to its path and
/// then newest serves it: the other is unserved once the first serves.
#[test]
fn only_the_closest_newest_copy_is_served() {
	let attr = |epoch, above| {
		Change::Live(Claim {
			item: Attr { epoch, above },
			cost: 0,
			priority: i8::MAX,
			draining: false,
			unavailable: false,
			marked: false,
			chain: Vec::new(),
		})
	};
	for (worse, better) in [(attr(1, 0), attr(2, 0)), (attr(0, 1), attr(0, 0))] {
		let (node, _clock) = node();
		let room = cast("room");
		let first = publisher(&node, 1);
		let second = publisher(&node, 2);
		announce(&first, &room, worse);
		assert_eq!(confirm(&first), 1);
		announce(&second, &room, better);
		assert_eq!(confirm(&second), 1);
		let serves = serves(&first);
		assert!(
			matches!(serves.as_slice(), [Output::Unserve { gone: Gone::Moved, .. }]),
			"{serves:?}"
		);
	}
}

/// A request naming no node goes to the session serving the claim, once that session
/// confirmed it, and never back to the session it came from.
#[test]
fn a_request_waits_for_the_session_to_serve_the_claim() {
	let (node, _clock) = node();
	let room = cast("room");
	let waiter = kio::Waiter::noop();
	let publisher = publisher(&node, 1);
	let _viewer = viewer(&node, 2);
	announce(&publisher, &room, live(0));
	assert!(node.poll_forward(2, None, &room, &waiter).is_pending());

	assert_eq!(confirm(&publisher), 1);
	let Poll::Ready(Some(Via::Route { node: named })) = node.poll_forward(2, None, &room, &waiter) else {
		panic!("the request follows the claim");
	};
	assert_eq!(named, node.me(), "a claim adopted from an endpoint is this node's");
	assert_eq!(node.poll_forward(1, None, &room, &waiter), Poll::Ready(None));
	assert_eq!(node.poll_forward(2, None, &cast("other"), &waiter), Poll::Ready(None));
}

/// A session refused a claim it may not carry leaves the request unroutable instead of
/// waiting forever.
#[test]
fn a_refused_claim_answers_the_request() {
	let (node, _clock) = node();
	let room = cast("room");
	let publisher = publisher(&node, 1);
	announce(&publisher, &room, live(0));
	for serve in serves(&publisher) {
		if let Output::Serve {
			origin, path, instance, ..
		} = serve
		{
			publisher.serving(origin, &(path, instance), false);
		}
	}
	assert_eq!(
		node.poll_forward(2, None, &room, &kio::Waiter::noop()),
		Poll::Ready(None)
	);
}

/// A forwarding neighbour's claim is served naming the node that claims it, and a
/// request naming that node follows the session the route leaves on.
#[test]
fn a_routed_claim_names_its_node() {
	let (node, _clock) = node();
	let room = cast("room");
	let far = id(9);
	let relay = node.connect(1, port(Role::Routing(id(7)))).unwrap();
	relay.recv(Msg::Edited).unwrap();
	let offer = Offer {
		path: [
			Hop { node: far, session: 0 },
			Hop {
				node: id(7),
				session: 4,
			},
		]
		.into(),
		draining: false,
		priority: i8::MAX,
		cost: 3,
		marked: false,
	};
	let offer = Some(offer);
	relay.recv(Msg::Route { origin: far, offer }).unwrap();
	let changes = vec![(room.clone(), live(0))];
	let claims = Msg::Claims {
		origin: far,
		changes,
		floor: 1,
	};
	relay.recv(claims).unwrap();
	relay.flush();

	let serves = serves(&relay);
	let [Output::Serve { origin, served, .. }] = serves.as_slice() else {
		panic!("one claim to serve: {serves:?}");
	};
	assert_eq!((*origin, served.named), (far, Some(far)));
	// The link's cost is added to the route's.
	assert_eq!(served.cost, 4);
	relay.serving(far, &room, true);
	let waiter = kio::Waiter::noop();
	assert_eq!(
		node.poll_forward(2, Some(far), &room, &waiter),
		Poll::Ready(Some(Via::Route { node: far }))
	);
	assert_eq!(node.poll_forward(2, Some(id(8)), &room, &waiter), Poll::Ready(None));
}

/// A routed claim told unavailable while its route stays is served unavailable, once
/// however often it changes, and a request naming its node goes nowhere.
#[test]
fn an_unavailable_routed_claim_takes_no_request() {
	let (node, _clock) = node();
	let room = cast("room");
	let far = id(9);
	let relay = node.connect(1, port(Role::Routing(id(7)))).unwrap();
	relay.recv(Msg::Edited).unwrap();
	let offer = Offer {
		path: [
			Hop { node: far, session: 0 },
			Hop {
				node: id(7),
				session: 4,
			},
		]
		.into(),
		draining: false,
		priority: i8::MAX,
		cost: 3,
		marked: false,
	};
	relay
		.recv(Msg::Route {
			origin: far,
			offer: Some(offer),
		})
		.unwrap();
	let claims = |cost, unavailable, floor| Msg::Claims {
		origin: far,
		changes: vec![(
			room.clone(),
			Change::Live(Claim {
				item: Attr { epoch: 0, above: 0 },
				cost,
				priority: i8::MAX,
				draining: false,
				unavailable,
				marked: false,
				chain: Vec::new(),
			}),
		)],
		floor,
	};
	relay.recv(claims(0, false, 1)).unwrap();
	relay.flush();
	assert_eq!(confirm(&relay), 1);

	relay.recv(claims(0, true, 2)).unwrap();
	relay.flush();
	let serves = serves(&relay);
	let [Output::Serve { served, .. }] = serves.as_slice() else {
		panic!("served unavailable: {serves:?}");
	};
	assert!(!served.available);
	relay.recv(claims(5, true, 3)).unwrap();
	relay.flush();
	assert!(self::serves(&relay).is_empty(), "already served unavailable");
	assert_eq!(
		node.poll_forward(2, Some(far), &room, &kio::Waiter::noop()),
		Poll::Ready(None)
	);
}

/// A session that closes leaves what it served as an orphan, which the node holds while
/// the reader still shows the claim and releases as lost, every copy with it, once the
/// debounce runs out.
#[test]
fn an_orphan_is_released_when_the_reader_lets_go() {
	let (node, clock) = node();
	let room = cast("room");
	let publisher = publisher(&node, 1);
	announce(&publisher, &room, live(0));
	assert_eq!(confirm(&publisher), 1);
	let left = Arc::new(AtomicU8::new(Left::HELD));
	publisher.orphan(node.me(), &room, Box::new(Left(left.clone())));
	drop(publisher);
	assert_eq!(
		left.load(Ordering::SeqCst),
		Left::HELD,
		"the reader still shows the claim"
	);

	expire(&node, &clock);
	assert_eq!(left.load(Ordering::SeqCst), Left::LOST);
}

/// Once nothing owns the origin, the node releases each orphan as lost, and a session
/// still open keeps serving its claims.
#[test]
fn releasing_orphans_keeps_the_open_sessions_claims() {
	let (node, _clock) = node();
	let (room, other) = (cast("room"), cast("other"));
	let first = publisher(&node, 1);
	let second = publisher(&node, 2);
	let _viewer = viewer(&node, 3);
	announce(&first, &room, live(0));
	announce(&second, &other, live(0));
	confirm(&first);
	confirm(&second);
	let left = Arc::new(AtomicU8::new(Left::HELD));
	first.orphan(node.me(), &room, Box::new(Left(left.clone())));
	drop(first);

	node.release_orphans();
	assert_eq!(left.load(Ordering::SeqCst), Left::LOST);
	assert_eq!(
		node.poll_forward(3, None, &other, &kio::Waiter::noop()),
		Poll::Ready(Some(Via::Route { node: node.me() }))
	);
}

/// A session that returns before the debounce runs out takes the claim over: the orphan
/// is released only once the new session serves it, so the origin never drops the claim.
#[test]
fn an_orphan_is_released_once_another_session_serves_the_claim() {
	let (node, _clock) = node();
	let room = cast("room");
	let first = publisher(&node, 1);
	announce(&first, &room, live(0));
	assert_eq!(confirm(&first), 1);
	let left = Arc::new(AtomicU8::new(Left::HELD));
	first.orphan(node.me(), &room, Box::new(Left(left.clone())));
	drop(first);

	let second = publisher(&node, 2);
	announce(&second, &room, live(0));
	assert_eq!(left.load(Ordering::SeqCst), Left::HELD, "nothing serves the claim yet");
	assert_eq!(confirm(&second), 1);
	assert_eq!(left.load(Ordering::SeqCst), Left::MOVED);
}

/// A claim another session takes over keeps its showing, and one shown again after the
/// node let go of it gets a new one, so the origin never retires the new copies with the
/// old.
#[test]
fn a_claim_shown_again_is_a_new_showing() {
	let lifecycle = |stream: &Stream| {
		let serves = serves(stream);
		let [
			Output::Serve {
				origin,
				path,
				instance,
				served,
			},
		] = serves.as_slice()
		else {
			panic!("one claim to serve: {serves:?}");
		};
		stream.serving(*origin, &(path.clone(), instance.clone()), true);
		served.lifecycle
	};
	let (node, clock) = node();
	let room = cast("room");
	let first = publisher(&node, 1);
	announce(&first, &room, live(0));
	let shown = lifecycle(&first);
	let left = Arc::new(AtomicU8::new(Left::HELD));
	first.orphan(node.me(), &room, Box::new(Left(left.clone())));
	drop(first);

	let second = publisher(&node, 2);
	announce(&second, &room, live(0));
	assert_eq!(lifecycle(&second), shown, "taken over, not shown again");
	assert_eq!(left.load(Ordering::SeqCst), Left::MOVED);
	let left = Arc::new(AtomicU8::new(Left::HELD));
	second.orphan(node.me(), &room, Box::new(Left(left.clone())));
	drop(second);
	expire(&node, &clock);
	assert_eq!(left.load(Ordering::SeqCst), Left::LOST);

	let third = publisher(&node, 3);
	announce(&third, &room, live(0));
	assert_ne!(lifecycle(&third), shown, "shown again after the node let go");
}

/// The origin's own copy of a claim never waits to be served, so a session refusing the
/// claim still releases the orphan another session left.
#[test]
fn an_orphan_is_released_once_every_session_refuses_the_claim() {
	let (node, _clock) = node();
	let room = cast("room");
	node.own(false, room.clone(), live(0));
	let first = publisher(&node, 1);
	announce(&first, &room, live(0));
	assert_eq!(confirm(&first), 1);
	let left = Arc::new(AtomicU8::new(Left::HELD));
	first.orphan(node.me(), &room, Box::new(Left(left.clone())));
	drop(first);

	let second = publisher(&node, 2);
	announce(&second, &room, live(0));
	assert_eq!(left.load(Ordering::SeqCst), Left::HELD);
	second.serving(node.me(), &room, false);
	assert_eq!(left.load(Ordering::SeqCst), Left::MOVED);
}

/// A claim its publisher withdraws stops being served as ended, so the origin withdraws
/// it rather than showing it offline.
#[test]
fn a_withdrawn_claim_is_unserved_as_ended() {
	let (node, _clock) = node();
	let room = cast("room");
	let publisher = publisher(&node, 1);
	announce(&publisher, &room, live(0));
	assert_eq!(confirm(&publisher), 1);
	announce(&publisher, &room, Change::Ended);
	let serves = serves(&publisher);
	assert!(
		matches!(serves.as_slice(), [Output::Unserve { gone: Gone::Ended, .. }]),
		"{serves:?}"
	);
}

/// A mount is a key of its own on the session: it is told what it asks for, never asks
/// the peer itself however much its session does, and closing the session closes it too.
#[test]
fn a_mount_asks_on_its_own_key() {
	let (node, _clock) = node();
	node.own(false, cast("a/x"), live(0));
	node.own(false, cast("b/y"), live(0));
	let session = publisher(&node, 1);
	let mount = session.mount().unwrap();
	let ask = Edit {
		remove: Vec::new(),
		add: vec![Path::new("b").unwrap()],
	};
	mount.recv(Msg::Ask(ask)).unwrap();
	mount.flush();
	let sent: Vec<Msg> = std::iter::from_fn(|| mount.try_wire()).collect();
	assert!(!sent.iter().any(|msg| matches!(msg, Msg::Ask(_))), "{sent:?}");
	let mounted: Vec<_> = (sent.into_iter())
		.filter_map(|msg| match msg {
			Msg::Announce { changes } => Some(changes),
			_ => None,
		})
		.flatten()
		.map(|((path, _), change)| (path.to_string(), matches!(change, Change::Live(_))))
		.collect();
	assert_eq!(mounted, [("b/y".to_string(), true)]);
	assert!(told(&session).is_empty());

	drop(session);
	node.own(false, cast("b/z"), live(0));
	assert!(told(&mount).is_empty(), "the mount closed with its session");
}

/// A session that is leaving is queued nothing more, and loses what was queued.
#[test]
fn a_muted_session_is_sent_nothing() {
	let (node, _clock) = node();
	let viewer = viewer(&node, 1);
	node.own(false, cast("a"), live(0));
	viewer.mute();
	assert!(viewer.try_wire().is_none(), "what was queued is dropped");
	node.own(false, cast("b"), live(0));
	assert!(viewer.try_wire().is_none(), "nothing more is queued");
}

/// A request from a session still goes nowhere back to it once another session closes.
#[test]
fn a_closed_session_leaves_the_others_requests_alone() {
	let (node, _clock) = node();
	let room = cast("room");
	let publisher = publisher(&node, 1);
	announce(&publisher, &room, live(0));
	assert_eq!(confirm(&publisher), 1);
	drop(viewer(&node, 2));
	let waiter = kio::Waiter::noop();
	assert_eq!(node.poll_forward(1, None, &room, &waiter), Poll::Ready(None));
}

/// When one of two sessions serving a claim closes, the other keeps serving it as it
/// was, unchanged, and requests go to it at once.
#[test]
fn a_closed_session_leaves_the_others_slots_alone() {
	let (node, _clock) = node();
	let room = cast("room");
	let first = publisher(&node, 1);
	let second = publisher(&node, 2);
	let _viewer = viewer(&node, 3);
	announce(&first, &room, live(0));
	announce(&second, &room, live(0));
	confirm(&first);
	confirm(&second);
	drop(first);
	assert!(serves(&second).is_empty(), "nothing changed for the second session");
	assert_eq!(
		node.poll_forward(3, None, &room, &kio::Waiter::noop()),
		Poll::Ready(Some(Via::Route { node: node.me() }))
	);
}

struct Flag(AtomicBool);

impl std::task::Wake for Flag {
	fn wake(self: Arc<Self>) {
		self.0.store(true, Ordering::SeqCst);
	}
}

impl Flag {
	fn woken(&self) -> bool {
		self.0.swap(false, Ordering::SeqCst)
	}
}

fn flag() -> (Arc<Flag>, kio::Waiter) {
	let flag = Arc::new(Flag(AtomicBool::new(false)));
	let waiter = kio::Waiter::new(std::task::Waker::from(flag.clone()));
	(flag, waiter)
}

/// A request waiting on a claim wakes when the session confirms it, and when the claim
/// changes, and not when nothing it follows does.
#[test]
fn a_waiting_request_wakes_when_its_claim_moves() {
	let (node, _clock) = node();
	let room = cast("room");
	let (flag, waiter) = flag();
	let publisher = publisher(&node, 1);
	let _viewer = viewer(&node, 2);
	announce(&publisher, &room, live(0));
	assert!(node.poll_forward(2, None, &room, &waiter).is_pending());
	publisher.flush();
	assert!(!flag.woken(), "nothing changed");
	assert_eq!(confirm(&publisher), 1);
	assert!(flag.woken(), "the session serves the claim");

	assert!(node.poll_forward(2, None, &cast("other"), &waiter).is_ready());
	let hall = cast("hall");
	announce(&publisher, &hall, live(0));
	assert!(node.poll_forward(2, None, &hall, &waiter).is_pending());
	flag.woken();
	announce(&publisher, &hall, Change::Ended);
	assert!(flag.woken(), "the claim went");
	assert_eq!(node.poll_forward(2, None, &hall, &waiter), Poll::Ready(None));
}

/// A neighbour that routes to a node before its claims arrive serves none of them, and is
/// handed each once its own copy holds it, though nothing changed of the claim here.
#[test]
fn a_neighbour_serves_a_claim_once_its_copy_holds_it() {
	let (node, _clock) = node();
	let room = cast("room");
	let far = id(9);
	let route = |via: u64, cost| Msg::Route {
		origin: far,
		offer: Some(Offer {
			path: [
				Hop { node: far, session: 0 },
				Hop {
					node: id(via),
					session: 1,
				},
			]
			.into(),
			draining: false,
			priority: i8::MAX,
			cost,
			marked: false,
		}),
	};
	let claims = Msg::Claims {
		origin: far,
		changes: vec![(room.clone(), live(0))],
		floor: 1,
	};
	let lagging = node.connect(1, port(Role::Routing(id(7)))).unwrap();
	lagging.recv(Msg::Edited).unwrap();
	let ahead = node.connect(2, port(Role::Routing(id(8)))).unwrap();
	ahead.recv(Msg::Edited).unwrap();
	lagging.recv(route(7, 1)).unwrap();
	lagging.flush();
	ahead.recv(route(8, 2)).unwrap();
	ahead.recv(claims.clone()).unwrap();
	ahead.flush();
	assert!(serves(&lagging).is_empty(), "its copy lacks the claim");
	assert_eq!(confirm(&ahead), 1);
	let waiter = kio::Waiter::noop();
	assert_eq!(
		node.poll_forward(3, Some(far), &room, &waiter),
		Poll::Ready(Some(Via::Route { node: far }))
	);

	lagging.recv(claims).unwrap();
	lagging.flush();
	assert_eq!(confirm(&lagging), 1);
}

/// The neighbour whose copy holds a claim leaves while another still routes to its node
/// with a copy that lacks it. The node keeps showing the claim available, so a request for
/// it waits for that copy rather than fail: nothing the node shows would change once the
/// copy arrives, so a request refused meanwhile would never be asked again.
#[test]
fn a_request_for_a_claim_shown_available_waits_for_a_copy_that_holds_it() {
	let (node, _clock) = node();
	let room = cast("room");
	let far = id(9);
	let route = |via: u64, cost| Msg::Route {
		origin: far,
		offer: Some(Offer {
			path: [
				Hop { node: far, session: 0 },
				Hop {
					node: id(via),
					session: 1,
				},
			]
			.into(),
			draining: false,
			priority: i8::MAX,
			cost,
			marked: false,
		}),
	};
	let claims = Msg::Claims {
		origin: far,
		changes: vec![(room.clone(), live(0))],
		floor: 1,
	};
	let viewer = viewer(&node, 5);
	let lagging = node.connect(1, port(Role::Routing(id(7)))).unwrap();
	lagging.recv(Msg::Edited).unwrap();
	let ahead = node.connect(2, port(Role::Routing(id(8)))).unwrap();
	ahead.recv(Msg::Edited).unwrap();
	lagging.recv(route(7, 1)).unwrap();
	lagging.flush();
	ahead.recv(route(8, 2)).unwrap();
	ahead.recv(claims.clone()).unwrap();
	ahead.flush();
	assert_eq!(confirm(&ahead), 1);
	assert_eq!(told(&viewer), [("room".to_string(), true)]);

	drop(ahead);
	assert!(told(&viewer).is_empty(), "the claim still shows available");
	assert!(serves(&lagging).is_empty(), "its copy lacks the claim");
	let (flag, waiter) = flag();
	assert!(node.poll_forward(5, None, &room, &waiter).is_pending());
	assert!(node.poll_forward(5, Some(far), &room, &waiter).is_pending());

	lagging.recv(claims).unwrap();
	lagging.flush();
	assert_eq!(confirm(&lagging), 1);
	assert!(flag.woken(), "a way appeared");
	assert_eq!(
		node.poll_forward(5, None, &room, &waiter),
		Poll::Ready(Some(Via::Route { node: far }))
	);
}

/// A request waiting for a way to a claim fails once the node no longer routes to the
/// claim's node, which shows the claim unavailable.
#[test]
fn a_request_waiting_for_a_way_fails_once_its_route_goes() {
	let (node, _clock) = node();
	let room = cast("room");
	let far = id(9);
	let route = |via: u64, cost| Msg::Route {
		origin: far,
		offer: Some(Offer {
			path: [
				Hop { node: far, session: 0 },
				Hop {
					node: id(via),
					session: 1,
				},
			]
			.into(),
			draining: false,
			priority: i8::MAX,
			cost,
			marked: false,
		}),
	};
	let viewer = viewer(&node, 5);
	let lagging = node.connect(1, port(Role::Routing(id(7)))).unwrap();
	lagging.recv(Msg::Edited).unwrap();
	let ahead = node.connect(2, port(Role::Routing(id(8)))).unwrap();
	ahead.recv(Msg::Edited).unwrap();
	lagging.recv(route(7, 1)).unwrap();
	lagging.flush();
	ahead.recv(route(8, 2)).unwrap();
	ahead
		.recv(Msg::Claims {
			origin: far,
			changes: vec![(room.clone(), live(0))],
			floor: 1,
		})
		.unwrap();
	ahead.flush();
	confirm(&ahead);
	drop(ahead);
	told(&viewer);
	let (flag, waiter) = flag();
	assert!(node.poll_forward(5, None, &room, &waiter).is_pending());

	lagging
		.recv(Msg::Route {
			origin: far,
			offer: None,
		})
		.unwrap();
	lagging.flush();
	assert!(flag.woken(), "the route went");
	assert_eq!(node.poll_forward(5, None, &room, &waiter), Poll::Ready(None));
	assert_eq!(node.poll_forward(5, Some(far), &room, &waiter), Poll::Ready(None));
}

/// A request waiting for a way to a claim fails once the neighbour reports a session on
/// the route's path closed: the route ends, but its copy of the claim stays known.
#[test]
fn a_request_waiting_for_a_way_fails_once_its_route_ends() {
	let (node, _clock) = node();
	let room = cast("room");
	let far = id(9);
	let route = |via: u64, cost| Msg::Route {
		origin: far,
		offer: Some(Offer {
			path: [
				Hop { node: far, session: 0 },
				Hop {
					node: id(via),
					session: 1,
				},
			]
			.into(),
			draining: false,
			priority: i8::MAX,
			cost,
			marked: false,
		}),
	};
	let viewer = viewer(&node, 5);
	let lagging = node.connect(1, port(Role::Routing(id(7)))).unwrap();
	lagging.recv(Msg::Edited).unwrap();
	let ahead = node.connect(2, port(Role::Routing(id(8)))).unwrap();
	ahead.recv(Msg::Edited).unwrap();
	lagging.recv(route(7, 1)).unwrap();
	lagging.flush();
	ahead.recv(route(8, 2)).unwrap();
	ahead
		.recv(Msg::Claims {
			origin: far,
			changes: vec![(room.clone(), live(0))],
			floor: 1,
		})
		.unwrap();
	ahead.flush();
	confirm(&ahead);
	drop(ahead);
	told(&viewer);
	let (flag, waiter) = flag();
	assert!(node.poll_forward(5, None, &room, &waiter).is_pending());

	lagging
		.recv(Msg::Dead {
			hop: Hop {
				node: id(7),
				session: 1,
			},
			far,
		})
		.unwrap();
	lagging.flush();
	assert!(flag.woken(), "the route ended");
	assert_eq!(node.poll_forward(5, None, &room, &waiter), Poll::Ready(None));
	assert_eq!(node.poll_forward(5, Some(far), &room, &waiter), Poll::Ready(None));
}

/// A request waiting for a way to a claim fails once a claim of better precedence outranks
/// it, as the node then shows the broadcast by that claim, which its sender asks anew.
#[test]
fn a_request_waiting_for_a_way_fails_once_its_claim_is_outranked() {
	let (node, _clock) = node();
	let room = cast("room");
	let far = id(9);
	let route = |via: u64, cost| Msg::Route {
		origin: far,
		offer: Some(Offer {
			path: [
				Hop { node: far, session: 0 },
				Hop {
					node: id(via),
					session: 1,
				},
			]
			.into(),
			draining: false,
			priority: i8::MAX,
			cost,
			marked: false,
		}),
	};
	let viewer = viewer(&node, 5);
	let publisher = publisher(&node, 6);
	let lagging = node.connect(1, port(Role::Routing(id(7)))).unwrap();
	lagging.recv(Msg::Edited).unwrap();
	let ahead = node.connect(2, port(Role::Routing(id(8)))).unwrap();
	ahead.recv(Msg::Edited).unwrap();
	lagging.recv(route(7, 1)).unwrap();
	lagging.flush();
	ahead.recv(route(8, 2)).unwrap();
	ahead
		.recv(Msg::Claims {
			origin: far,
			changes: vec![(room.clone(), live(0))],
			floor: 1,
		})
		.unwrap();
	ahead.flush();
	confirm(&ahead);
	drop(ahead);
	told(&viewer);
	let (flag, waiter) = flag();
	assert!(node.poll_forward(5, Some(far), &room, &waiter).is_pending());

	announce(&publisher, &room, live(1));
	confirm(&publisher);
	assert!(flag.woken(), "a newer claim arrived");
	assert_eq!(told(&viewer), [("room".to_string(), true)], "shown by the newer claim");
	assert_eq!(node.poll_forward(5, Some(far), &room, &waiter), Poll::Ready(None));
}
