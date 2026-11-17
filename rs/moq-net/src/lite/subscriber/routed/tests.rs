use crate::lite::{AnnounceBroadcast as Wire, PathRef, test_transport::SinkSession};
use crate::{
	Error, Instance, Path,
	auth::Permit,
	route::{
		NodeId,
		node::{Attr, Msg},
		relay::{Claim, Hop, Offer},
	},
};

use super::{Routed, Subscriber};

/// The forwarding node the peer declares.
const PEER: NodeId = NodeId { relay: 7, start: 1 };

fn subscriber() -> (Subscriber, crate::origin::Consumer) {
	super::super::tests::restart_subscriber(SinkSession::new(Default::default()))
}

/// A subscriber whose origin forwards, as a relay's does.
fn forwarding() -> (Subscriber, crate::origin::Consumer) {
	let (mut subscriber, consumer) = subscriber();
	subscriber.origin = subscriber.origin.with_proxy(true);
	(subscriber, consumer)
}

/// The peer's route to `node`, which it reaches itself or through the hops after it.
fn route(node: NodeId) -> Wire {
	let path: Vec<Hop> = [node, PEER]
		.into_iter()
		.take(if node == PEER { 1 } else { 2 })
		.enumerate()
		.map(|(session, node)| Hop {
			node,
			session: session as u64,
		})
		.collect();
	Wire::Route {
		offer: Offer {
			path: path.into(),
			draining: false,
			priority: i8::MAX,
			cost: 0,
			marked: false,
		},
	}
}

fn claim(above: u64) -> Claim<Attr> {
	Claim {
		item: Attr { epoch: 0, above },
		cost: 0,
		priority: i8::MAX,
		draining: false,
		unavailable: false,
		chain: Vec::new(),
	}
}

fn start(path: &'static str) -> Wire {
	Wire::Start {
		suffix: PathRef::literal(Path::from_static(path)),
		instance: Instance::random(),
		claim: claim(0),
	}
}

fn limited(pattern: &str) -> Permit {
	Permit::limited([pattern.parse::<crate::Pattern>().unwrap()].into_iter().collect())
}

/// Send the peer what the node asks of it, which the peer answers at once, then serve
/// what the node hands the session.
fn pump(routed: &mut Routed, subscriber: &Subscriber) {
	let stream = routed.stream();
	routed.flush();
	while let Some(msg) = stream.try_wire() {
		if let Msg::Ask(edit) = msg {
			assert!(!routed.ask(edit).is_empty(), "an edit is at least one request");
			routed.receive(Wire::Ok).unwrap();
		}
	}
	routed.flush();
	while let Some(output) = stream.try_serve() {
		routed.output(subscriber, output);
	}
}

/// A session with a peer that declared no Node ID, asked for everything.
fn asking(subscriber: &Subscriber, permit: &Permit) -> Routed {
	let mut routed = Routed::new(subscriber, 1, permit, None, None).unwrap();
	routed.want(Path::empty(), subscriber.origin.snapshot(""));
	pump(&mut routed, subscriber);
	routed
}

/// A session with the forwarding node [`PEER`].
fn routing(subscriber: &Subscriber) -> Routed {
	let mut routed = Routed::new(subscriber, 1, &Permit::default(), None, Some(PEER)).unwrap();
	pump(&mut routed, subscriber);
	routed
}

/// The claims the session serves into the origin that a route reaches, by path.
fn served(routed: &Routed) -> Vec<String> {
	let mut served: Vec<String> = (routed.served.iter())
		.filter(|(_, served)| served.route.route.available)
		.map(|((_, path, _), _)| path.to_string())
		.collect();
	served.sort();
	served
}

/// A peer that exchanges no routes has each claim served as it arrives, and withdrawn
/// as it ends, with no route or floor.
#[kio::test]
async fn a_claim_is_served_as_it_arrives() {
	let (subscriber, consumer) = subscriber();
	let mut cursor = consumer.routes();
	let mut routed = asking(&subscriber, &Permit::default());
	routed.receive(start("room")).unwrap();
	pump(&mut routed, &subscriber);
	assert_eq!(served(&routed), ["room"]);
	cursor.expect_next_active("room").await;

	routed.receive(Wire::End { id: 0, lost: false }).unwrap();
	pump(&mut routed, &subscriber);
	assert!(served(&routed).is_empty());
	cursor.expect_next_ended("room").await;
}

/// A session whose peer declared no Node ID asks it only for what the origin's readers
/// want, and for nothing once none does.
#[kio::test]
async fn a_session_asks_for_what_its_readers_want() {
	let (subscriber, _consumer) = subscriber();
	let mut routed = Routed::new(&subscriber, 1, &Permit::default(), None, None).unwrap();
	let stream = routed.stream();
	assert_eq!(stream.try_wire(), None, "nothing is wanted yet");

	let room = Path::from_static("room");
	routed.want(room.clone(), subscriber.origin.snapshot("room"));
	let Some(Msg::Ask(edit)) = stream.try_wire() else {
		panic!("the node asks for the prefix");
	};
	assert_eq!((edit.remove.as_slice(), edit.add.as_slice()), (&[][..], &[room.clone()][..]));
	let requests = routed.ask(edit);
	assert_eq!(
		requests,
		[Wire::Request {
			more: false,
			remove: Vec::new(),
			add: vec![(room.clone(), true)],
		}]
	);
	routed.receive(Wire::Ok).unwrap();

	routed.unwant(&room);
	let Some(Msg::Ask(edit)) = stream.try_wire() else {
		panic!("the node stops asking for the prefix");
	};
	assert_eq!(
		routed.ask(edit),
		[Wire::Request {
			more: false,
			remove: vec![0],
			add: Vec::new(),
		}]
	);
}

/// Both ends drop every claim within a prefix an edit removes when its ANNOUNCE_OK
/// arrives: the claim's Announce ID is retired with no ANNOUNCE_END, and no route
/// reaches it any longer.
#[kio::test]
async fn an_answered_edit_drops_the_claims_of_a_removed_prefix() {
	let (subscriber, _consumer) = subscriber();
	let mut routed = asking(&subscriber, &Permit::default());
	routed.receive(start("room")).unwrap();
	pump(&mut routed, &subscriber);
	assert_eq!(served(&routed), ["room"]);

	routed.unwant(&Path::empty());
	pump(&mut routed, &subscriber);
	assert!(served(&routed).is_empty());
	let again = routed.receive(Wire::End { id: 0, lost: false });
	assert!(matches!(again, Err(Error::ProtocolViolation)), "{again:?}");
}

/// What the peer sends counts against the session's announce cap, one slot per live
/// claim, so the claims held never pass it.
#[kio::test]
async fn claims_never_pass_the_announce_cap() {
	let (mut subscriber, _consumer) = subscriber();
	subscriber.announces = crate::session::Slots::new(2);
	let mut routed = asking(&subscriber, &Permit::default());
	routed.receive(start("a")).unwrap();
	routed.receive(start("b")).unwrap();
	assert!(routed.receive(start("c")).is_err());

	// An ended claim frees its slot.
	let (mut subscriber, _consumer) = self::subscriber();
	subscriber.announces = crate::session::Slots::new(2);
	let mut routed = asking(&subscriber, &Permit::default());
	routed.receive(start("a")).unwrap();
	routed.receive(start("b")).unwrap();
	routed.receive(Wire::End { id: 0, lost: false }).unwrap();
	routed.receive(start("c")).unwrap();
}

/// A limit leaves out the claims it does not admit, and a change of limit brings them
/// in and out again.
#[kio::test]
async fn the_limit_admits_each_claim() {
	let (subscriber, _consumer) = subscriber();
	let public = limited("live/*/public/**");
	let everything = limited("live/**");

	let mut routed = asking(&subscriber, &public);
	for msg in [start("live/a/public/x"), start("live/a/private/y")] {
		routed.receive(msg).unwrap();
	}
	pump(&mut routed, &subscriber);
	assert_eq!(served(&routed), ["live/a/public/x"]);

	routed.limit(&everything).unwrap();
	pump(&mut routed, &subscriber);
	assert_eq!(served(&routed), ["live/a/private/y", "live/a/public/x"]);

	routed.limit(&public).unwrap();
	pump(&mut routed, &subscriber);
	assert_eq!(served(&routed), ["live/a/public/x"]);
}

/// A forwarding neighbour's claims wait for the floor that closes their batch, and are
/// served under the route the floor names.
#[kio::test]
async fn a_routing_neighbours_claims_wait_for_their_floor() {
	let (subscriber, consumer) = forwarding();
	let mut cursor = consumer.routes();
	let mut routed = routing(&subscriber);
	routed.receive(route(PEER)).unwrap();
	routed.receive(start("live/a")).unwrap();
	pump(&mut routed, &subscriber);
	assert!(served(&routed).is_empty(), "no floor closed the batch");

	routed.receive(Wire::Floor { id: 0, revision: 1 }).unwrap();
	pump(&mut routed, &subscriber);
	assert_eq!(served(&routed), ["live/a"]);
	let shown = cursor.expect_next_active("live/a").await;
	assert!(shown.available);
}

/// Only its floor closes a batch of claim messages: anything else between them closes
/// the session, as does a floor naming a route the peer never sent.
#[kio::test]
async fn a_batch_ends_only_at_its_floor() {
	let (subscriber, _consumer) = forwarding();
	for (what, msg) in [
		("a route", route(PEER)),
		("a synced", Wire::Synced { prefix: 0 }),
		("a drain", Wire::Drain { node: PEER }),
	] {
		let mut routed = routing(&subscriber);
		routed.receive(route(PEER)).unwrap();
		routed.receive(start("live/a")).unwrap();
		let inside = routed.receive(msg);
		assert!(matches!(inside, Err(Error::ProtocolViolation)), "{what}: {inside:?}");
	}

	let mut routed = routing(&subscriber);
	routed.receive(route(PEER)).unwrap();
	routed.receive(start("live/a")).unwrap();
	let unknown = routed.receive(Wire::Floor { id: 5, revision: 1 });
	assert!(matches!(unknown, Err(Error::ProtocolViolation)), "{unknown:?}");
}

/// A ROUTE naming a node the stream already has a route to replaces that route under
/// the same Route ID, and one naming another node takes the next.
#[kio::test]
async fn a_route_to_the_same_node_keeps_its_route_id() {
	let (subscriber, _consumer) = forwarding();
	let other = NodeId { relay: 9, start: 1 };
	let mut routed = routing(&subscriber);
	routed.receive(route(PEER)).unwrap();
	routed.receive(route(PEER)).unwrap();
	routed.receive(route(other)).unwrap();
	assert_eq!(routed.named[&PEER], 0);
	assert_eq!(routed.named[&other], 1);

	// Each floor files its batch under its own node.
	routed.receive(start("live/a")).unwrap();
	routed.receive(Wire::Floor { id: 0, revision: 1 }).unwrap();
	routed.receive(start("live/b")).unwrap();
	routed.receive(Wire::Floor { id: 1, revision: 1 }).unwrap();
	pump(&mut routed, &subscriber);
	assert_eq!(served(&routed), ["live/a", "live/b"]);
	let nodes: std::collections::BTreeSet<NodeId> = routed.served.keys().map(|(node, _, _)| *node).collect();
	assert_eq!(nodes, [PEER, other].into_iter().collect());
}

/// ROUTE_END retires the route's ID and every claim under it, with no ANNOUNCE_END, so
/// naming either again closes the session.
#[kio::test]
async fn a_route_end_retires_its_claims() {
	let (subscriber, _consumer) = forwarding();
	let mut routed = routing(&subscriber);
	routed.receive(route(PEER)).unwrap();
	routed.receive(start("live/a")).unwrap();
	routed.receive(Wire::Floor { id: 0, revision: 1 }).unwrap();
	pump(&mut routed, &subscriber);
	assert_eq!(served(&routed), ["live/a"]);

	routed.receive(Wire::RouteEnd { id: 0 }).unwrap();
	pump(&mut routed, &subscriber);
	assert!(served(&routed).is_empty());
	for (what, msg) in [
		("the claim", Wire::End { id: 0, lost: false }),
		("the route", Wire::RouteEnd { id: 0 }),
	] {
		let again = routed.receive(msg);
		assert!(matches!(again, Err(Error::ProtocolViolation)), "{what}: {again:?}");
	}
}

/// A forwarding neighbour is trusted with the whole tree, so a permit that confines it
/// ends the session rather than narrowing what it sends.
#[kio::test]
async fn a_routing_neighbour_keeps_a_whole_permit() {
	let (subscriber, _consumer) = forwarding();
	let mut routed = routing(&subscriber);
	routed.limit(&Permit::default()).unwrap();
	let narrowed = routed.limit(&limited("live/**"));
	assert!(matches!(narrowed, Err(Error::Unauthorized)), "{narrowed:?}");
}

/// A session that closes leaves what it served in the origin, shown but unavailable,
/// for the node to hand to another session or let go.
#[kio::test]
async fn a_closed_session_leaves_its_claims_unavailable() {
	let (subscriber, consumer) = subscriber();
	let mut cursor = consumer.routes();
	let mut routed = asking(&subscriber, &Permit::default());
	routed.receive(start("room")).unwrap();
	pump(&mut routed, &subscriber);
	assert!(cursor.expect_next_active("room").await.available);

	drop(routed);
	assert!(!cursor.expect_next_active("room").await.available);
}
