use crate::lite::{AnnounceBroadcast as Msg, AnnounceDecoder, PathRef, test_transport::SinkSession};
use crate::{Instance, Path, auth::Permit, origin::Cost, route::path::{Hop, Offer}};

fn offer() -> Offer {
	Offer {
		path: [Hop { node: 7, session: 0 }].into(),
		draining: false,
		priority: i8::MAX,
		metric: 0,
		down_only: false,
		rtt: None,
	}
}

fn start(path: &'static str) -> Msg {
	Msg::Active {
		suffix: PathRef::literal(Path::from_static(path)),
		route: 0,
		instance: Some(Instance::random()),
		epoch: 0,
		hops: Default::default(),
		cost: Cost::new(0),
	}
}

fn limited(pattern: &str) -> Permit {
	Permit::limited([pattern.parse::<crate::Pattern>().unwrap()].into_iter().collect())
}

/// The claims the stream serves into the origin, by path.
fn served(routed: &super::Routed) -> Vec<String> {
	let mut served: Vec<String> = routed.served.keys().map(|(_, path, _)| path.to_string()).collect();
	served.sort();
	served
}

/// A limit narrower than its regions still leaves out the claims it does not
/// admit, and a change that keeps the regions brings them in and out again.
#[kio::test]
async fn the_limit_admits_each_claim() {
	let (subscriber, _consumer) = super::super::tests::restart_subscriber(SinkSession::new(Default::default()));
	let prefix = Path::empty();
	let public = limited("live/*/public/**");
	let everything = limited("live/**");
	assert_eq!(public.regions(), everything.regions(), "the limits share their regions");

	let mut routed = super::Routed::new(&subscriber, &prefix, 1, &public).unwrap();
	let mut decoder = AnnounceDecoder::default();
	let floor = Msg::Floor {
		id: 0,
		revision: Some(0),
	};
	let route = Msg::RouteStart {
		node: 7,
		suffix: Path::empty(),
		offer: offer(),
	};
	for msg in [route, start("live/a/public/x"), start("live/a/private/y"), floor] {
		routed.receive(&prefix, msg, &mut decoder).unwrap();
	}
	routed.outputs(&subscriber);
	assert_eq!(served(&routed), ["live/a/public/x"]);

	routed.limit(&subscriber, &everything).unwrap();
	assert_eq!(served(&routed), ["live/a/private/y", "live/a/public/x"]);

	routed.limit(&subscriber, &public).unwrap();
	assert_eq!(served(&routed), ["live/a/public/x"]);
}

/// A claim beneath a subtree the origin handle excludes never enters the node, on
/// the floor or on a replay, even though the permit admits it; a covering claim
/// overlapping the handle's scope still does.
#[kio::test]
async fn an_excluded_claim_never_enters_the_node() {
	let (mut subscriber, _consumer) = super::super::tests::restart_subscriber(SinkSession::new(Default::default()));
	subscriber.origin = {
		let patterns: crate::Patterns = ["live/*/public/**", ".stats/**"]
			.into_iter()
			.map(|pattern| pattern.parse::<crate::Pattern>().unwrap())
			.collect();
		subscriber.origin.scope("", &patterns).unwrap()
	}
	.exclude(".stats")
	.unwrap();
	let prefix = Path::empty();
	let mut routed = super::Routed::new(&subscriber, &prefix, 1, &Default::default()).unwrap();
	let mut decoder = AnnounceDecoder::default();
	let floor = Msg::Floor {
		id: 0,
		revision: Some(0),
	};
	let route = Msg::RouteStart {
		node: 7,
		suffix: Path::empty(),
		offer: offer(),
	};
	for msg in [
		route,
		start("live"),
		start(".stats/node"),
		start(".stats/node/evil"),
		floor,
	] {
		routed.receive(&prefix, msg, &mut decoder).unwrap();
	}
	let shown = |routed: &super::Routed| -> Vec<String> {
		routed.node.shown().keys().map(|(_, path)| path.to_string()).collect()
	};
	routed.outputs(&subscriber);
	assert_eq!(shown(&routed), ["live"]);
	assert_eq!(served(&routed), ["live"]);

	// A narrower limit certifies nothing, so widening it again replays every
	// committed claim into a fresh view; only the replay filter keeps `.stats` out.
	routed.limit(&subscriber, &limited("live/**")).unwrap();
	assert_eq!(shown(&routed), ["live"]);
	assert_eq!(served(&routed), ["live"]);

	routed.limit(&subscriber, &Default::default()).unwrap();
	assert_eq!(shown(&routed), ["live"]);
	assert_eq!(served(&routed), ["live"]);
}

/// An unscoped stream narrowed to a limit that leaves a claim out certifies nothing,
/// so widening it again brings that claim back at the same revision.
#[kio::test]
async fn a_new_scope_certifies_against_its_own_regions() {
	let (subscriber, _consumer) = super::super::tests::restart_subscriber(SinkSession::new(Default::default()));
	let prefix = Path::empty();
	let mut routed = super::Routed::new(&subscriber, &prefix, 1, &Default::default()).unwrap();
	let mut decoder = AnnounceDecoder::default();
	let floor = Msg::Floor {
		id: 0,
		revision: Some(0),
	};
	let route = Msg::RouteStart {
		node: 7,
		suffix: Path::empty(),
		offer: offer(),
	};
	for msg in [route, start("live/a/public/x"), start("live/a/private/y"), floor] {
		routed.receive(&prefix, msg, &mut decoder).unwrap();
	}
	routed.outputs(&subscriber);
	assert_eq!(served(&routed), ["live/a/private/y", "live/a/public/x"]);

	routed.limit(&subscriber, &limited("live/*/public/**")).unwrap();
	assert_eq!(served(&routed), ["live/a/public/x"]);

	routed.limit(&subscriber, &limited("live/**")).unwrap();
	assert_eq!(served(&routed), ["live/a/private/y", "live/a/public/x"]);
}

/// Two sessions proving one route key share the peer's routes, but a session serves
/// only the claims its own limit admits, even where its regions reach the rest.
#[kio::test]
async fn a_claim_is_served_only_where_the_limit_admits_it() {
	let (subscriber, _consumer) = super::super::tests::restart_subscriber(SinkSession::new(Default::default()));
	let prefix = Path::empty();
	let everything = limited("live/**");
	let public = limited("live/*/public/**");
	let mut wide = super::Routed::new(&subscriber, &prefix, 1, &everything).unwrap();
	let mut narrow = super::Routed::new(&subscriber, &prefix, 1, &everything).unwrap();
	narrow.session_key = wide.session_key;
	narrow.limit(&subscriber, &public).unwrap();

	for routed in [&mut wide, &mut narrow] {
		let mut decoder = AnnounceDecoder::default();
		let floor = Msg::Floor {
			id: 0,
			revision: Some(0),
		};
		let route = Msg::RouteStart {
			node: 7,
			suffix: Path::empty(),
			offer: offer(),
		};
		for msg in [route, start("live/a/public/x"), start("live/a/private/y"), floor] {
			routed.receive(&prefix, msg, &mut decoder).unwrap();
		}
	}
	wide.outputs(&subscriber);
	narrow.outputs(&subscriber);
	assert_eq!(served(&wide), ["live/a/private/y", "live/a/public/x"]);
	assert_eq!(served(&narrow), ["live/a/public/x"]);
}

/// The route and claims a peer sends one stream, its Route ID 0.
fn feed(
	routed: &mut super::Routed,
	decoder: &mut AnnounceDecoder,
	paths: &[&'static str],
) -> Result<(), crate::Error> {
	let prefix = Path::empty();
	let route = Msg::RouteStart {
		node: 7,
		suffix: Path::empty(),
		offer: offer(),
	};
	routed.receive(&prefix, route, decoder)?;
	for path in paths {
		routed.receive(&prefix, start(path), decoder)?;
	}
	Ok(())
}

/// A request handed over keeps what it holds on a budget of its own, so the claims
/// arriving on both it and its replacement during the handover count once against the
/// session's cap: a session that fits its claims stays open.
#[kio::test]
async fn a_handover_counts_the_replacement_alone() {
	let (mut subscriber, _consumer) = super::super::tests::restart_subscriber(SinkSession::new(Default::default()));
	// One route and three claims.
	subscriber.announces = crate::session::Slots::new(4);
	let prefix = Path::empty();
	let mut old = super::Routed::new(&subscriber, &prefix, 1, &Default::default()).unwrap();
	let mut old_decoder = AnnounceDecoder::default();
	feed(&mut old, &mut old_decoder, &["x", "y"]).unwrap();

	old.hand_over();
	assert_eq!(
		subscriber.announces.available(),
		4,
		"the session's count is the replacement's"
	);
	let mut new = super::Routed::new(&subscriber, &prefix, 1, &Default::default()).unwrap();
	let mut new_decoder = AnnounceDecoder::default();
	feed(&mut new, &mut new_decoder, &["x", "y"]).unwrap();

	// A claim the peer starts during the handover reaches both requests.
	old.receive(&prefix, start("z"), &mut old_decoder)
		.expect("the old request fits it on its own budget");
	new.receive(&prefix, start("z"), &mut new_decoder)
		.expect("the session fits it");
	assert_eq!(subscriber.announces.available(), 0);

	drop(old);
	assert_eq!(
		subscriber.announces.available(),
		0,
		"the old request held none of the session's"
	);
	drop(new);
	assert_eq!(subscriber.announces.available(), 4);
}

/// A peer flooding a request handed over fails it on its own budget, the session's cap,
/// so the session holds at most twice the cap during one handover.
#[kio::test]
async fn a_handover_caps_the_old_request() {
	let (mut subscriber, _consumer) = super::super::tests::restart_subscriber(SinkSession::new(Default::default()));
	subscriber.announces = crate::session::Slots::new(3);
	let prefix = Path::empty();
	let mut old = super::Routed::new(&subscriber, &prefix, 1, &Default::default()).unwrap();
	let mut old_decoder = AnnounceDecoder::default();
	feed(&mut old, &mut old_decoder, &["x"]).unwrap();

	old.hand_over();
	let mut new = super::Routed::new(&subscriber, &prefix, 1, &Default::default()).unwrap();
	let mut new_decoder = AnnounceDecoder::default();
	feed(&mut new, &mut new_decoder, &["x", "y"]).unwrap();
	assert_eq!(subscriber.announces.available(), 0);

	old.receive(&prefix, start("y"), &mut old_decoder)
		.expect("the old request's budget has room for one more");
	assert!(
		old.receive(&prefix, start("z"), &mut old_decoder).is_err(),
		"the old request is capped on its own budget"
	);
	assert!(
		new.receive(&prefix, start("z"), &mut new_decoder).is_err(),
		"the replacement is capped on the session's"
	);
}

/// A transport that reports its first RTT only after the stream opened, such as
/// qmux before its first ping returns, still gets its link measured.
#[kio::test]
async fn a_late_rtt_is_still_read() {
	let session = SinkSession::new(Default::default());
	let (subscriber, _consumer) = super::super::tests::restart_subscriber(session.clone());
	let prefix = Path::empty();
	let mut routed = super::Routed::new(&subscriber, &prefix, 1, &Default::default()).unwrap();
	let mut decoder = AnnounceDecoder::default();
	feed(&mut routed, &mut decoder, &[]).unwrap();
	let waiter = kio::Waiter::noop();
	routed.poll(&subscriber, &waiter).unwrap();
	kio::clock::sim::advance(crate::route::LinkRtt::INTERVAL).await;
	routed.poll(&subscriber, &waiter).unwrap();
	assert_eq!(routed.rtt.reported(), None);

	let rtt = std::time::Duration::from_millis(20);
	session.set_stats(crate::lite::test_transport::SinkStats::default().with_rtt(rtt));
	kio::clock::sim::advance(crate::route::LinkRtt::INTERVAL).await;
	routed.poll(&subscriber, &waiter).unwrap();
	assert_eq!(routed.rtt.reported(), Some(rtt));
}

/// Claims served when the peer reports Synced, before the stream is first polled,
/// already carry the link's RTT.
#[kio::test]
async fn the_first_claims_served_carry_the_rtt() {
	use crate::lite::test_transport::SinkStats;
	let ms = std::time::Duration::from_millis;
	let session = SinkSession::new(Default::default()).with_stats(SinkStats::default().with_rtt(ms(10)));
	let (subscriber, _consumer) = super::super::tests::restart_subscriber(session);
	let prefix = Path::empty();
	let mut routed = super::Routed::new(&subscriber, &prefix, 1, &Default::default()).unwrap();
	let mut decoder = AnnounceDecoder::default();
	let route = Msg::RouteStart {
		node: 7,
		suffix: Path::empty(),
		offer: Offer {
			rtt: Some(ms(5)),
			..offer()
		},
	};
	let floor = Msg::Floor {
		id: 0,
		revision: Some(0),
	};
	for msg in [route, start("live"), floor] {
		routed.receive(&prefix, msg, &mut decoder).unwrap();
	}
	routed.outputs(&subscriber);
	let rtts: Vec<_> = routed.served.values().map(|entry| entry.route.route.rtt()).collect();
	assert_eq!(rtts, [Some(ms(15))]);
}

/// A stream whose peer ends every route stops reading, and starts a fresh window
/// once one returns, so a reading from before the pause never stays reported.
#[kio::test]
async fn a_pause_restarts_the_window() {
	use crate::lite::test_transport::SinkStats;
	let ms = std::time::Duration::from_millis;
	let session = SinkSession::new(Default::default()).with_stats(SinkStats::default().with_rtt(ms(10)));
	let (subscriber, _consumer) = super::super::tests::restart_subscriber(session.clone());
	let prefix = Path::empty();
	let mut routed = super::Routed::new(&subscriber, &prefix, 1, &Default::default()).unwrap();
	let mut decoder = AnnounceDecoder::default();
	let waiter = kio::Waiter::noop();
	feed(&mut routed, &mut decoder, &[]).unwrap();
	routed.poll(&subscriber, &waiter).unwrap();
	assert_eq!(routed.rtt.reported(), Some(ms(10)));

	routed.receive(&prefix, Msg::RouteEnd { id: 0 }, &mut decoder).unwrap();
	routed.poll(&subscriber, &waiter).unwrap();
	assert!(routed.read_rtt.is_none(), "no route, no timer");
	session.set_stats(SinkStats::default().with_rtt(ms(100)));
	kio::clock::sim::advance(std::time::Duration::from_secs(60)).await;

	let route = Msg::RouteStart {
		node: 7,
		suffix: Path::empty(),
		offer: offer(),
	};
	routed.receive(&prefix, route, &mut decoder).unwrap();
	routed.poll(&subscriber, &waiter).unwrap();
	assert_eq!(routed.rtt.reported(), Some(ms(100)));
}

/// What the stream keeps between floors is the net change to its committed claims,
/// however many messages the peer sends without one.
#[kio::test]
async fn pending_changes_net_out() {
	let (subscriber, _consumer) = super::super::tests::restart_subscriber(SinkSession::new(Default::default()));
	let prefix = Path::empty();
	let mut routed = super::Routed::new(&subscriber, &prefix, 1, &Default::default()).unwrap();
	let mut decoder = AnnounceDecoder::default();
	let floor = Msg::Floor {
		id: 0,
		revision: Some(0),
	};
	let route = Msg::RouteStart {
		node: 7,
		suffix: Path::empty(),
		offer: offer(),
	};
	for msg in [route, start("room/host"), floor] {
		routed.receive(&prefix, msg, &mut decoder).unwrap();
	}
	let pending = |routed: &super::Routed| routed.routes[&0].pending.len();
	assert_eq!(pending(&routed), 0);

	for cost in 1..1000 {
		let update = Msg::Update {
			id: 0,
			hops: Default::default(),
			cost: Cost::new(cost),
		};
		routed.receive(&prefix, update, &mut decoder).unwrap();
	}
	assert_eq!(pending(&routed), 1, "updates to one claim");

	let back = Msg::Update {
		id: 0,
		hops: Default::default(),
		cost: Cost::new(0),
	};
	routed.receive(&prefix, back, &mut decoder).unwrap();
	assert_eq!(pending(&routed), 0, "an update back to the committed claim");

	for _ in 0..100 {
		routed.receive(&prefix, start("room/guest"), &mut decoder).unwrap();
		let id = decoder.last();
		routed.receive(&prefix, Msg::EndedId { id }, &mut decoder).unwrap();
	}
	assert_eq!(pending(&routed), 0, "claims started and ended between floors");
}

/// A claim this node refuses was not withdrawn by its origin: while its route holds
/// one, the route's floors certify nothing, so a relay forwards no withdrawal for it,
/// and a usable restart certifies the route again.
#[kio::test]
async fn a_refused_claim_forwards_no_withdrawal() {
	use crate::lite::{Refusable, Refusal};
	let (subscriber, _consumer) = super::super::tests::restart_subscriber(SinkSession::new(Default::default()));
	let prefix = Path::empty();
	let mut routed = super::Routed::new(&subscriber, &prefix, 1, &Default::default()).unwrap();
	let peer = crate::route::node::Peer { tag: u64::MAX, node: 9 };
	let edge = (routed.node)
		.answer(peer, crate::route::Link::default(), Path::empty())
		.unwrap();
	let mut decoder = AnnounceDecoder::default();
	let floor = |revision| Msg::Floor {
		id: 0,
		revision: Some(revision),
	};
	let refused = |id| Msg::Refused {
		message: Refusable::Change { id },
		reason: Refusal::Instance,
	};
	let restart = |id| Msg::Restart {
		id,
		instance: Some(Instance::random()),
		epoch: 1,
		cost: Cost::new(0),
		hops: Default::default(),
	};
	// Each batch the edge would send for node 7: the paths it changes, whether each is
	// present, and its floor.
	let batches = |routed: &mut super::Routed| {
		routed.outputs(&subscriber);
		let mut batches = Vec::new();
		while let Some(output) = edge.try_output() {
			if let crate::route::node::Output::Batch {
				id: 7, changes, floor, ..
			} = output
			{
				let changes: Vec<_> = (changes.into_iter())
					.map(|(path, claim)| (path.to_string(), claim.is_some()))
					.collect();
				batches.push((changes, floor));
			}
		}
		batches
	};
	let route = Msg::RouteStart {
		node: 7,
		suffix: Path::empty(),
		offer: offer(),
	};
	for msg in [route, start("live/a"), start("live/b"), floor(0)] {
		routed.receive(&prefix, msg, &mut decoder).unwrap();
	}
	assert_eq!(
		batches(&mut routed),
		[(vec![("live/a".into(), true), ("live/b".into(), true)], Some(0))]
	);

	// Refused, live/a goes without a withdrawal certified for it.
	for msg in [refused(0), floor(1)] {
		routed.receive(&prefix, msg, &mut decoder).unwrap();
	}
	assert_eq!(batches(&mut routed), [(vec![("live/a".into(), false)], None)]);
	// Nor does live/b's change certify anything, while the route holds the refused claim.
	let update = Msg::Update {
		id: 1,
		hops: Default::default(),
		cost: Cost::new(5),
	};
	for msg in [update, floor(2)] {
		routed.receive(&prefix, msg, &mut decoder).unwrap();
	}
	assert_eq!(batches(&mut routed), [(vec![("live/b".into(), true)], None)]);

	// A usable restart brings live/a back, certified again.
	for msg in [restart(0), floor(3)] {
		routed.receive(&prefix, msg, &mut decoder).unwrap();
	}
	assert_eq!(batches(&mut routed), [(vec![("live/a".into(), true)], Some(3))]);

	// A refused restart withholds it as the update did.
	for msg in [refused(0), floor(4)] {
		routed.receive(&prefix, msg, &mut decoder).unwrap();
	}
	assert_eq!(batches(&mut routed), [(vec![("live/a".into(), false)], None)]);
}

/// A route this node refuses still follows a DEAD naming a hop on its path, so both
/// ends agree on which routes live: the closed session it crossed ends it, its claims
/// with it, and the peer may start it over. A refused moved offer withdraws the route
/// it names until a usable offer comes.
#[kio::test]
async fn a_refused_route_still_follows_a_dead() {
	use crate::lite::{Dead, Refusable, Refusal, Unusable};
	let (subscriber, _consumer) = super::super::tests::restart_subscriber(SinkSession::new(Default::default()));
	let prefix = Path::empty();
	let mut routed = super::Routed::new(&subscriber, &prefix, 1, &Default::default()).unwrap();
	let mut decoder = AnnounceDecoder::default();
	let path = |session| -> std::sync::Arc<[Hop]> {
		[Hop { node: 7, session: 0 }, Hop { node: 5, session }].into()
	};
	let floor = |id| Msg::Floor { id, revision: Some(0) };
	let claim = |route, path: &'static str| Msg::Active {
		suffix: PathRef::literal(Path::from_static(path)),
		route,
		instance: Some(Instance::random()),
		epoch: 0,
		hops: Default::default(),
		cost: Cost::new(0),
	};
	let closed = |session, gone, moved| {
		Msg::Dead(Dead::Closed {
			hop: Hop { node: 5, session },
			far: 7,
			gone,
			moved,
		})
	};

	// Refused for a flag this node does not know: its claim is held, never served.
	let refused = Msg::Refused {
		message: Refusable::RouteStart {
			node: 7,
			suffix: Some(Path::empty()),
			path: path(3),
		},
		reason: Refusal::Flags(8),
	};
	for msg in [refused, claim(0, "live/a"), floor(0)] {
		routed.receive(&prefix, msg, &mut decoder).unwrap();
	}
	routed.outputs(&subscriber);
	assert!(served(&routed).is_empty());

	// The session it crossed closed for good: it ends, and its Announce ID retires.
	routed.receive(&prefix, closed(3, true, Vec::new()), &mut decoder).unwrap();
	assert!(routed.routes.is_empty() && routed.claims.is_empty() && routed.named.is_empty());
	assert!(routed.receive(&prefix, Msg::EndedId { id: 0 }, &mut decoder).is_err());

	// So the peer starts the route over, usably.
	let mut decoder = AnnounceDecoder::default();
	let mut routed = super::Routed::new(&subscriber, &prefix, 1, &Default::default()).unwrap();
	let usable = |session| Offer {
		path: path(session),
		..offer()
	};
	let route = |session| Msg::RouteStart {
		node: 7,
		suffix: Path::empty(),
		offer: usable(session),
	};
	for msg in [
		Msg::Refused {
			message: Refusable::RouteStart {
				node: 7,
				suffix: Some(Path::empty()),
				path: path(3),
			},
			reason: Refusal::Flags(8),
		},
		claim(0, "live/a"),
		closed(3, true, Vec::new()),
		route(4),
		claim(1, "live/b"),
		floor(1),
	] {
		routed.receive(&prefix, msg, &mut decoder).unwrap();
	}
	routed.outputs(&subscriber);
	assert_eq!(served(&routed), ["live/b"]);

	// It moved to an offer this node refuses: withdrawn, though the peer still holds it.
	let moved = vec![(
		1,
		Err(Unusable {
			path: path(5),
			reason: Refusal::Flags(8),
		}),
	)];
	routed.receive(&prefix, closed(4, false, moved), &mut decoder).unwrap();
	routed.outputs(&subscriber);
	assert!(served(&routed).is_empty());
	assert!(routed.routes[&1].reach().is_none());

	// A usable offer files it again with its last floor's claims.
	let update = Msg::RouteUpdate { id: 1, offer: usable(6) };
	routed.receive(&prefix, update, &mut decoder).unwrap();
	assert!(routed.routes[&1].reach().is_some());
	assert_eq!(routed.routes[&1].committed.as_ref().map(|(claims, _)| claims.len()), Some(1));
}
