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
const PEER: NodeId = NodeId {
	relay: 7,
	start: 1,
	marked: false,
};

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
		marked: false,
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
	assert_eq!(
		(edit.remove.as_slice(), edit.add.as_slice()),
		(&[][..], &[room.clone()][..])
	);
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

/// A peer slow to answer an edit is sent no more of them: what the readers want by
/// the time it answers goes out as the next one.
#[kio::test]
async fn interest_changes_wait_for_the_unanswered_edit() {
	let (subscriber, _consumer) = subscriber();
	let mut routed = Routed::new(&subscriber, 1, &Permit::default(), None, None).unwrap();
	let stream = routed.stream();
	let room = Path::from_static("room");
	routed.want(room.clone(), subscriber.origin.snapshot("room"));
	let Some(Msg::Ask(edit)) = stream.try_wire() else {
		panic!("the node asks for the prefix");
	};
	routed.ask(edit);

	// Unanswered: readers come and go, and nothing more is asked.
	let other = Path::from_static("other");
	for _ in 0..8 {
		routed.want(other.clone(), subscriber.origin.snapshot("other"));
		routed.unwant(&other);
	}
	routed.want(other.clone(), subscriber.origin.snapshot("other"));
	assert_eq!(stream.try_wire(), None, "an edit is still unanswered");
	assert_eq!(routed.edits.len(), 1);

	routed.receive(Wire::Ok).unwrap();
	let Some(Msg::Ask(edit)) = stream.try_wire() else {
		panic!("the answer asks for what is wanted now");
	};
	assert_eq!((edit.remove.as_slice(), edit.add.as_slice()), (&[][..], &[other][..]));
	assert_eq!(stream.try_wire(), None);
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

/// A grant change holds unsynced only the readers whose prefix it newly shows something
/// in, until an answer after it is in; a reader it misses, and every reader on a
/// narrowing, stays synced, since nothing new can arrive there.
#[kio::test]
async fn a_grant_change_unsyncs_only_the_readers_it_can_show_more() {
	let (subscriber, consumer) = subscriber();
	let granting = |paths: &[&str]| {
		Permit::granting(paths.iter().map(|path| crate::Pattern::subtree(path).unwrap()).collect())
	};
	let all = crate::Patterns::from(crate::Pattern::all());
	let everything = consumer.routes();
	let a = consumer.scope("a", &all).unwrap().routes();
	let synced = |routes: &crate::origin::Routes| routes.poll_synced(&kio::Waiter::noop()).is_ready();
	let answer = |routed: &mut Routed, prefix: u64| {
		pump(routed, &subscriber);
		routed.receive(Wire::Synced { prefix }).unwrap();
		routed.release();
	};

	let mut routed = Routed::new(&subscriber, 1, &granting(&["a"]), None, None).unwrap();
	routed.want(Path::empty(), subscriber.origin.snapshot(""));
	answer(&mut routed, 0);
	assert!(synced(&everything) && synced(&a));

	// `b` joins the grant: the whole tree waits for it, and `a` cannot gain from it.
	routed.limit(&granting(&["a", "b"])).unwrap();
	assert!(!synced(&everything), "the whole tree holds the new region");
	assert!(synced(&a), "a region the change misses stays synced");
	answer(&mut routed, 1);
	assert!(synced(&everything));

	// A narrowing shows nothing new.
	routed.limit(&granting(&["a"])).unwrap();
	assert!(synced(&everything) && synced(&a), "a narrowing keeps every reader synced");
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

/// A routing neighbour that withholds the floor holds one change per claim here,
/// however often it updates the claim, and none for a claim it starts and ends.
#[kio::test]
async fn a_batch_keeps_one_change_per_claim() {
	let (subscriber, _consumer) = forwarding();
	let mut routed = routing(&subscriber);
	routed.receive(route(PEER)).unwrap();
	routed.receive(start("live/a")).unwrap();
	for cost in 0..64 {
		let mut claim = claim(0);
		claim.cost = cost;
		routed.receive(Wire::Change { id: 0, claim }).unwrap();
	}
	assert_eq!(routed.pending.changes.len(), 1);
	for id in 1..64 {
		routed.receive(start("live/b")).unwrap();
		routed.receive(Wire::End { id, lost: false }).unwrap();
	}
	assert_eq!(routed.pending.changes.len(), 1);

	routed.receive(Wire::Floor { id: 0, revision: 1 }).unwrap();
	pump(&mut routed, &subscriber);
	assert_eq!(served(&routed), ["live/a"]);

	// One the node was told ends at the next floor.
	routed.receive(Wire::End { id: 0, lost: false }).unwrap();
	assert_eq!(routed.pending.changes.len(), 1);
	routed.receive(Wire::Floor { id: 0, revision: 2 }).unwrap();
	pump(&mut routed, &subscriber);
	assert!(served(&routed).is_empty());
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
	let other = NodeId {
		relay: 9,
		start: 1,
		marked: false,
	};
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
	// A grant that names everything confines nothing, as a relay's token for a peer does.
	routed.limit(&limited("**")).unwrap();
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

/// What a closed session leaves behind holds the origin, so the origin's teardown lets go
/// of the node holding it, or the two keep each other alive.
#[kio::test]
async fn a_torn_down_origin_lets_go_of_what_closed_sessions_left() {
	let (subscriber, consumer) = subscriber();
	let node = subscriber.origin.route_node().downgrade();
	let mut routed = asking(&subscriber, &Permit::default());
	routed.receive(start("room")).unwrap();
	pump(&mut routed, &subscriber);
	drop(routed);
	assert!(node.alive(), "the origin keeps what the session left");

	drop((subscriber, consumer));
	// The origin's driver tears it down once it sees the last owner gone.
	kio::clock::sleep(std::time::Duration::from_millis(1)).await;
	assert!(!node.alive(), "the node outlived its origin");
}

/// A reader waiting on what a closed session left does not hold up an origin nothing owns:
/// the claim goes offline, the request concludes, and the origin tears down.
#[kio::test]
async fn an_unowned_origin_lets_go_of_a_request_waiting_on_what_closed_sessions_left() {
	let (subscriber, consumer) = subscriber();
	let node = subscriber.origin.route_node().downgrade();
	let mut routed = asking(&subscriber, &Permit::default());
	routed.receive(start("room")).unwrap();
	pump(&mut routed, &subscriber);
	drop(routed);

	let mut request = std::pin::pin!(consumer.request("room", None));
	kio::clock::sleep(std::time::Duration::from_millis(1)).await;
	assert!(
		futures::poll!(request.as_mut()).is_pending(),
		"an owned origin waits for a session to serve the claim again"
	);

	drop(subscriber);
	let resolved = kio::clock::timeout(std::time::Duration::from_secs(1), request).await;
	assert!(matches!(resolved, Ok(Err(_))), "{:?}", resolved.map(|r| r.map(|_| ())));
	kio::clock::sleep(std::time::Duration::from_millis(1)).await;
	assert!(!node.alive(), "the node outlived its origin");
}

/// Prefixes named `head/0`, `head/1`, ... each padded to about `bytes`.
fn prefixes(head: &str, count: usize, bytes: usize) -> std::collections::BTreeSet<Path> {
	(0..count)
		.map(|i| Path::new(&format!("{head}/{i}{}", "x".repeat(bytes))).expect("a path"))
		.collect()
}

#[test]
fn a_set_past_one_request_is_asked_for_more_coarsely() {
	let fits =
		|set: &std::collections::BTreeSet<Path>| crate::lite::request_fits(set.iter(), crate::lite::Version::Lite07);
	let path = |path: &str| Path::new(path).expect("a path");

	// A set that fits is asked for as it is.
	let small = prefixes("a", 16, 64);
	assert_eq!(super::coarsen(small.clone(), &Default::default()), small);

	// Without a confined grant, what the prefixes share stands for them.
	let wide = prefixes("a", 17, 4000);
	assert!(!fits(&wide));
	assert_eq!(
		super::coarsen(wide.clone(), &Default::default()),
		[path("a")].into_iter().collect()
	);

	// Within a grant, each region's prefixes give way inside it, the largest group first,
	// and nothing is asked for above a region.
	let regions: std::collections::BTreeSet<Path> = [path("a"), path("b")].into_iter().collect();
	let mut both = wide.clone();
	both.extend(prefixes("b", 2, 64));
	let asked = super::coarsen(both, &regions);
	assert!(fits(&asked));
	assert!(asked.contains(&path("a")));
	assert_eq!(asked.len(), 3);

	// One prefix alone in each region gives way to its region.
	let regions: std::collections::BTreeSet<Path> = (0..17).map(|i| path(&format!("r{i}"))).collect();
	let alone: std::collections::BTreeSet<Path> = (regions.iter())
		.map(|region| path(&format!("{region}/{}", "x".repeat(4000))))
		.collect();
	assert!(!fits(&alone));
	let asked = super::coarsen(alone, &regions);
	assert!(fits(&asked));
	assert!(
		asked
			.iter()
			.all(|prefix| regions.iter().any(|region| prefix.has_prefix(region)))
	);
	assert_eq!(asked.len(), 17);

	// Regions too many on their own give way to the longest prefix at least two of them
	// share, up to the root, and none goes unasked.
	let mut regions = prefixes("g/h", 16, 4000);
	let lone = prefixes("g/i", 1, 4000);
	regions.extend(lone.iter().cloned());
	regions.insert(path("k"));
	assert!(!fits(&regions));
	let mut expected = lone;
	expected.extend([path("g/h"), path("k")]);
	assert_eq!(super::coarsen(regions.clone(), &regions), expected);
	let regions: std::collections::BTreeSet<Path> =
		(0..17).map(|i| path(&format!("r{i}/{}", "x".repeat(4000)))).collect();
	assert_eq!(
		super::coarsen(regions.clone(), &regions),
		[Path::empty()].into_iter().collect()
	);
}

/// A claim whose path this node cannot resolve lies in a prefix it cannot name, so
/// removing another prefix leaves it for the peer to end by its id.
#[kio::test]
async fn an_unresolved_claim_outlives_another_prefixs_removal() {
	let (subscriber, _consumer) = subscriber();
	let mut routed = Routed::new(&subscriber, 1, &Permit::default(), None, None).unwrap();
	routed.want(Path::from_static("a"), subscriber.origin.snapshot("a"));
	routed.want(Path::from_static("b"), subscriber.origin.snapshot("b"));
	pump(&mut routed, &subscriber);

	// A base and a rest that each fit a path, and together do not.
	let long = "x".repeat(Path::MAX_BYTES - 8);
	let literal = Wire::Start {
		suffix: PathRef::literal(Path::new(&format!("a/{long}")).unwrap()),
		instance: Instance::random(),
		claim: claim(0),
	};
	let unresolved = || Wire::Start {
		suffix: PathRef {
			base: 1,
			keep: 2,
			rest: Path::new(&long).unwrap(),
		},
		instance: Instance::random(),
		claim: claim(0),
	};
	routed.receive(literal).unwrap();
	routed.receive(unresolved()).unwrap();
	routed.receive(unresolved()).unwrap();
	assert!(routed.claims[&1].relative.is_none() && routed.claims[&2].relative.is_none());

	routed.unwant(&Path::from_static("b"));
	pump(&mut routed, &subscriber);
	routed.receive(Wire::End { id: 1, lost: false }).unwrap();

	// Once no earlier prefix is left for it to lie in, it goes with the rest.
	routed.unwant(&Path::from_static("a"));
	pump(&mut routed, &subscriber);
	assert!(routed.claims.is_empty());
	let ended = routed.receive(Wire::End { id: 2, lost: false });
	assert!(matches!(ended, Err(Error::ProtocolViolation)), "{ended:?}");
}

/// The node shows a peer's claim to the sessions it reaches before the session serving it
/// lands its route in the origin, each on its own task. A reader asking for the claim the
/// moment it hears of it waits for that route, and is not refused for want of it.
#[kio::test]
async fn a_request_for_a_claim_shown_waits_for_its_route() {
	use crate::model::ProduceTest;
	const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
	let version = crate::Version::lite(crate::lite::Version::Lite07);
	let (subscriber, _consumer) = forwarding();
	let mut upstream = asking(&subscriber, &Permit::default());

	// A reader on lite-07, served from the relay's origin.
	let reader = crate::origin::Config::new(std::num::NonZeroU64::new(3).unwrap()).produce();
	let (client, server) = crate::transport::memory::pair(crate::transport::memory::Config {
		protocol: Some(version.alpn().to_string()),
		..Default::default()
	});
	let dial = crate::Client::new()
		.with_versions(version.into())
		.with_subscriber(reader.clone());
	let serve = crate::Server::new()
		.with_versions(version.into())
		.with_publisher(subscriber.origin.consume());
	let (_client, _server) = futures::join!(
		async {
			let (session, driver) = dial.connect(kio::clock::now(), client).await.unwrap();
			drop(kio::clock::sim::spawn(crate::time::run(driver)));
			session
		},
		async {
			let (session, driver) = serve.accept(kio::clock::now(), server).await.unwrap();
			drop(kio::clock::sim::spawn(crate::time::run(driver)));
			session
		},
	);

	// The claim reaches the node, which shows it to the reader, while the session it
	// arrived on has yet to land its route.
	upstream.receive(start("live")).unwrap();
	upstream.flush();
	let live = kio::clock::timeout(TIMEOUT, reader.consume().follow("live", None).unwrap().available())
		.await
		.expect("the reader hears of the claim")
		.unwrap()
		.expect("the claim is shown");
	// It asks at once, and its request reaches the relay before the route does.
	let _subscribing = kio::clock::sim::spawn(async move { live.track("video").unwrap().subscribe(None).await });
	kio::clock::sleep(std::time::Duration::from_millis(100)).await;

	// The route lands, and the request goes on to the session serving the claim. Refused
	// instead, the reader would wait for a route change that never comes.
	let stream = upstream.stream();
	while let Some(output) = stream.try_serve() {
		upstream.output(&subscriber, output);
	}
	let minted = kio::clock::timeout(
		TIMEOUT,
		kio::wait(|waiter| {
			upstream.poll(&subscriber, waiter);
			subscriber.sources.poll_pop(waiter)
		}),
	)
	.await
	.expect("the request reaches the claim")
	.unwrap();
	assert_eq!(minted.path.as_str(), "live");
}

/// Count `subscriber`'s sessions under the label `peer`.
fn counted(subscriber: &mut Subscriber) -> crate::stats::Registry {
	let registry = crate::stats::Registry::new(crate::stats::Config::new());
	let session = (registry.tier(crate::stats::Tier::default()))
		.session(crate::stats::SessionInfo::new(Path::from_static("peer")));
	subscriber.origin = subscriber.origin.clone().with_stats(session);
	registry
}

/// What `registry`'s one label counted on the route layer.
fn routes(registry: &crate::stats::Registry) -> crate::stats::Routes {
	let sessions = registry.snapshot().sessions();
	let [(_, presence)] = sessions.as_slice() else {
		panic!("one label: {sessions:?}");
	};
	assert_eq!(presence.routes_refused, presence.routes.refused.total());
	presence.routes
}

/// A claim the limit leaves out counts as refused once, as it starts, and not again as it
/// changes or as a later limit takes it in and out.
#[kio::test]
async fn a_claim_outside_the_limit_counts_once() {
	let (mut subscriber, _consumer) = subscriber();
	let registry = counted(&mut subscriber);
	let public = limited("live/*/public/**");
	let mut routed = asking(&subscriber, &public);
	for msg in [start("live/a/public/x"), start("live/a/private/y")] {
		routed.receive(msg).unwrap();
	}
	routed.receive(Wire::Change { id: 1, claim: claim(1) }).unwrap();
	routed.limit(&limited("live/**")).unwrap();
	routed.limit(&public).unwrap();
	pump(&mut routed, &subscriber);
	let refused = routes(&registry).refused;
	assert_eq!((refused.scope, refused.total()), (1, 1), "{refused:?}");
}

/// A routing neighbour's withdrawals count as received, and an input the route layer
/// refuses counts the session closed by its kind.
#[kio::test]
async fn a_routing_neighbours_withdrawals_and_errors_count() {
	let (mut subscriber, _consumer) = forwarding();
	let registry = counted(&mut subscriber);
	let mut routed = routing(&subscriber);
	let far = NodeId { relay: 8, ..PEER };
	routed.receive(route(far)).unwrap();
	routed.receive(Wire::Drain { node: far }).unwrap();
	let hop = Hop { node: PEER, session: 1 };
	routed.receive(Wire::Dead { hop, far }).unwrap();
	pump(&mut routed, &subscriber);
	let counted = routes(&registry);
	assert_eq!((counted.received.drain, counted.received.dead), (1, 1), "{counted:?}");
	assert_eq!(counted.closed, Default::default());

	// A route whose path does not end at the neighbour that sent it.
	let Wire::Route { mut offer } = route(far) else {
		unreachable!()
	};
	offer.path = vec![Hop { node: far, session: 0 }].into();
	let err = routed.receive(Wire::Route { offer });
	assert!(matches!(err, Err(Error::ProtocolViolation)), "{err:?}");
	// The session's route stream counts the close, by the kind the route layer gave.
	assert_eq!(routed.closed(), crate::stats::RouteClose::Malformed);
}

/// A peer that declared no Node ID sends no routes: the close it earns is its role's.
#[kio::test]
async fn a_route_from_a_non_routing_peer_closes_for_its_role() {
	let (subscriber, _consumer) = subscriber();
	let mut routed = asking(&subscriber, &Permit::default());
	let err = routed.receive(route(PEER));
	assert!(matches!(err, Err(Error::ProtocolViolation)), "{err:?}");
	assert_eq!(routed.closed(), crate::stats::RouteClose::Role);
	// Noted once: an error the route layer gives no kind counts as malformed.
	assert_eq!(routed.closed(), crate::stats::RouteClose::Malformed);
}

/// A claim a peer forwards whose chain already names this relay came back to it: the node
/// takes it as lost, keeps the session, and counts it as looped.
#[kio::test]
async fn a_claim_back_through_this_relay_counts_as_looped() {
	let (mut subscriber, _consumer) = forwarding();
	let registry = counted(&mut subscriber);
	let mut routed = asking(&subscriber, &Permit::default());
	let mut looped = claim(0);
	looped.chain = vec![routed.me.relay];
	let start = |claim| Wire::Start {
		suffix: PathRef::literal(Path::from_static("live/a")),
		instance: Instance::random(),
		claim,
	};
	routed.receive(start(looped)).unwrap();
	pump(&mut routed, &subscriber);
	assert!(served(&routed).is_empty());
	let refused = routes(&registry).refused;
	assert_eq!((refused.looped, refused.total()), (1, 1), "{refused:?}");
}

/// A claim the origin refuses to serve counts once as its slot turns refused, not again
/// each time the node hands it over with its route changed.
#[kio::test]
async fn a_claim_refused_in_serve_counts_once_across_route_changes() {
	let (mut subscriber, _consumer) = forwarding();
	let registry = counted(&mut subscriber);
	subscriber.origin = subscriber.origin.exclude("live").unwrap();
	let mut routed = routing(&subscriber);
	let far = NodeId { relay: 8, ..PEER };
	routed.receive(route(far)).unwrap();
	routed.receive(start("live/a")).unwrap();
	routed.receive(Wire::Floor { id: 0, revision: 1 }).unwrap();
	pump(&mut routed, &subscriber);
	assert!(served(&routed).is_empty());
	assert_eq!(routes(&registry).refused.scope, 1);

	// The route drains, so the node hands the claim over again with its new rank.
	routed.receive(Wire::Drain { node: far }).unwrap();
	pump(&mut routed, &subscriber);
	let refused = routes(&registry).refused;
	assert_eq!((refused.scope, refused.total()), (1, 1), "{refused:?}");
}
