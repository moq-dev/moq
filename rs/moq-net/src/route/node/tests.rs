use std::sync::atomic::{AtomicU8, Ordering};

use super::*;
use crate::route::relay::{Edit, Hop, Offer, Role};

fn id(relay: u64) -> NodeId {
	NodeId { relay, start: 1 }
}

fn node() -> (Node, Deadlines) {
	let clock = Deadlines::new(Instant::now());
	(Node::new(id(1), clock.clone()), clock)
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
	const RELEASED: u8 = 1;
	const ENDED: u8 = 2;
}

impl Orphan for Left {
	fn release(self: Box<Self>, ended: bool) {
		let how = if ended { Self::ENDED } else { Self::RELEASED };
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
		path: [Hop { node: far, session: 0 }, Hop { node: id(7), session: 4 }].into(),
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

/// A session that closes leaves what it served as an orphan, which the node holds while
/// the reader still shows the claim and releases, not ended, once the debounce runs out.
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
	assert_eq!(left.load(Ordering::SeqCst), Left::HELD, "the reader still shows the claim");

	expire(&node, &clock);
	assert_eq!(left.load(Ordering::SeqCst), Left::RELEASED);
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
	assert_eq!(left.load(Ordering::SeqCst), Left::RELEASED);
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
		matches!(serves.as_slice(), [Output::Unserve { ended: true, .. }]),
		"{serves:?}"
	);
}

/// A mount is a key of its own on the session: it is told what it asks for, and closing
/// the session closes it too.
#[test]
fn a_mount_asks_on_its_own_key() {
	let (node, _clock) = node();
	node.own(false, cast("a/x"), live(0));
	node.own(false, cast("b/y"), live(0));
	let session = node.connect(1, port(Role::Scoped(Vec::new()))).unwrap();
	let mount = session.mount().unwrap();
	let ask = Edit {
		remove: Vec::new(),
		add: vec![Path::new("b").unwrap()],
	};
	mount.recv(Msg::Ask(ask)).unwrap();
	mount.flush();
	assert_eq!(told(&mount), [("b/y".to_string(), true)]);
	assert!(told(&session).is_empty());

	drop(session);
	node.own(false, cast("b/z"), live(0));
	assert!(told(&mount).is_empty(), "the mount closed with its session");
}
