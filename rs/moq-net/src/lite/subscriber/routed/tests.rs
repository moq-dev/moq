use crate::lite::{AnnounceBroadcast as Msg, PathRef, test_transport::SinkSession};
use crate::{
	Instance, Path,
	auth::Permit,
	origin::Cost,
	route::path::{Hop, Offer},
};

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

	let mut routed = super::Routed::asking(&subscriber, &prefix, 1, &public).unwrap();
	let floor = Msg::Floor {
		id: 0,
		revision: Some(0),
	};
	let route = Msg::RouteStart {
		node: 7,
		prefix: 0,
		suffix: Path::empty(),
		offer: offer(),
	};
	for msg in [route, start("live/a/public/x"), start("live/a/private/y"), floor] {
		routed.receive(msg).unwrap();
	}
	routed.outputs(&subscriber);
	assert_eq!(served(&routed), ["live/a/public/x"]);

	routed.relimit(&subscriber, &everything).unwrap();
	assert_eq!(served(&routed), ["live/a/private/y", "live/a/public/x"]);

	routed.relimit(&subscriber, &public).unwrap();
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
	let mut routed = super::Routed::asking(&subscriber, &prefix, 1, &Default::default()).unwrap();
	let floor = Msg::Floor {
		id: 0,
		revision: Some(0),
	};
	let route = Msg::RouteStart {
		node: 7,
		prefix: 0,
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
		routed.receive(msg).unwrap();
	}
	let shown = |routed: &super::Routed| -> Vec<String> {
		routed.node.shown().keys().map(|(_, path)| path.to_string()).collect()
	};
	routed.outputs(&subscriber);
	assert_eq!(shown(&routed), ["live"]);
	assert_eq!(served(&routed), ["live"]);

	// A narrower limit certifies nothing, so widening it again replays every
	// committed claim into a fresh view; only the replay filter keeps `.stats` out.
	routed.relimit(&subscriber, &limited("live/**")).unwrap();
	assert_eq!(shown(&routed), ["live"]);
	assert_eq!(served(&routed), ["live"]);

	routed.relimit(&subscriber, &Default::default()).unwrap();
	assert_eq!(shown(&routed), ["live"]);
	assert_eq!(served(&routed), ["live"]);
}

/// An unscoped stream narrowed to a limit that leaves a claim out certifies nothing,
/// so widening it again brings that claim back at the same revision.
#[kio::test]
async fn a_new_scope_certifies_against_its_own_regions() {
	let (subscriber, _consumer) = super::super::tests::restart_subscriber(SinkSession::new(Default::default()));
	let prefix = Path::empty();
	let mut routed = super::Routed::asking(&subscriber, &prefix, 1, &Default::default()).unwrap();
	let floor = Msg::Floor {
		id: 0,
		revision: Some(0),
	};
	let route = Msg::RouteStart {
		node: 7,
		prefix: 0,
		suffix: Path::empty(),
		offer: offer(),
	};
	for msg in [route, start("live/a/public/x"), start("live/a/private/y"), floor] {
		routed.receive(msg).unwrap();
	}
	routed.outputs(&subscriber);
	assert_eq!(served(&routed), ["live/a/private/y", "live/a/public/x"]);

	routed.relimit(&subscriber, &limited("live/*/public/**")).unwrap();
	assert_eq!(served(&routed), ["live/a/public/x"]);

	routed.relimit(&subscriber, &limited("live/**")).unwrap();
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
	let mut wide = super::Routed::asking(&subscriber, &prefix, 1, &everything).unwrap();
	let mut narrow = super::Routed::asking(&subscriber, &prefix, 1, &everything).unwrap();
	narrow.session_key = wide.session_key;
	narrow.relimit(&subscriber, &public).unwrap();

	for routed in [&mut wide, &mut narrow] {
		let floor = Msg::Floor {
			id: 0,
			revision: Some(0),
		};
		let route = Msg::RouteStart {
			node: 7,
			prefix: 0,
			suffix: Path::empty(),
			offer: offer(),
		};
		for msg in [route, start("live/a/public/x"), start("live/a/private/y"), floor] {
			routed.receive(msg).unwrap();
		}
	}
	wide.outputs(&subscriber);
	narrow.outputs(&subscriber);
	assert_eq!(served(&wide), ["live/a/private/y", "live/a/public/x"]);
	assert_eq!(served(&narrow), ["live/a/public/x"]);
}

/// The route and claims a peer sends, its Route ID 0.
fn feed(routed: &mut super::Routed, paths: &[&'static str]) -> Result<(), crate::Error> {
	let route = Msg::RouteStart {
		node: 7,
		prefix: 0,
		suffix: Path::empty(),
		offer: offer(),
	};
	routed.receive(route)?;
	for path in paths {
		routed.receive(start(path))?;
	}
	Ok(())
}

/// A transport that reports its first RTT only after the stream opened, such as
/// qmux before its first ping returns, still gets its link measured.
#[kio::test]
async fn a_late_rtt_is_still_read() {
	let session = SinkSession::new(Default::default());
	let (subscriber, _consumer) = super::super::tests::restart_subscriber(session.clone());
	let prefix = Path::empty();
	let mut routed = super::Routed::asking(&subscriber, &prefix, 1, &Default::default()).unwrap();
	feed(&mut routed, &[]).unwrap();
	let waiter = kio::Waiter::noop();
	routed.poll(&subscriber, &waiter);
	kio::clock::sim::advance(crate::route::LinkRtt::INTERVAL).await;
	routed.poll(&subscriber, &waiter);
	assert_eq!(routed.rtt.reported(), None);

	let rtt = std::time::Duration::from_millis(20);
	session.set_stats(crate::lite::test_transport::SinkStats::default().with_rtt(rtt));
	kio::clock::sim::advance(crate::route::LinkRtt::INTERVAL).await;
	routed.poll(&subscriber, &waiter);
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
	let mut routed = super::Routed::asking(&subscriber, &prefix, 1, &Default::default()).unwrap();
	let route = Msg::RouteStart {
		node: 7,
		prefix: 0,
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
		routed.receive(msg).unwrap();
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
	let mut routed = super::Routed::asking(&subscriber, &prefix, 1, &Default::default()).unwrap();
	let waiter = kio::Waiter::noop();
	feed(&mut routed, &[]).unwrap();
	routed.poll(&subscriber, &waiter);
	assert_eq!(routed.rtt.reported(), Some(ms(10)));

	routed.receive(Msg::RouteEnd { id: 0 }).unwrap();
	routed.poll(&subscriber, &waiter);
	assert!(routed.read_rtt.is_none(), "no route, no timer");
	session.set_stats(SinkStats::default().with_rtt(ms(100)));
	kio::clock::sim::advance(std::time::Duration::from_secs(60)).await;

	let route = Msg::RouteStart {
		node: 7,
		prefix: 0,
		suffix: Path::empty(),
		offer: offer(),
	};
	routed.receive(route).unwrap();
	routed.poll(&subscriber, &waiter);
	assert_eq!(routed.rtt.reported(), Some(ms(100)));
}

/// What the stream keeps between floors is the net change to its committed claims,
/// however many messages the peer sends without one.
#[kio::test]
async fn pending_changes_net_out() {
	let (subscriber, _consumer) = super::super::tests::restart_subscriber(SinkSession::new(Default::default()));
	let prefix = Path::empty();
	let mut routed = super::Routed::asking(&subscriber, &prefix, 1, &Default::default()).unwrap();
	let floor = Msg::Floor {
		id: 0,
		revision: Some(0),
	};
	let route = Msg::RouteStart {
		node: 7,
		prefix: 0,
		suffix: Path::empty(),
		offer: offer(),
	};
	for msg in [route, start("room/host"), floor] {
		routed.receive(msg).unwrap();
	}
	let pending = |routed: &super::Routed| routed.routes[&0].pending.len();
	assert_eq!(pending(&routed), 0);

	for cost in 1..1000 {
		let update = Msg::Update {
			id: 0,
			hops: Default::default(),
			cost: Cost::new(cost),
		};
		routed.receive(update).unwrap();
	}
	assert_eq!(pending(&routed), 1, "updates to one claim");

	let back = Msg::Update {
		id: 0,
		hops: Default::default(),
		cost: Cost::new(0),
	};
	routed.receive(back).unwrap();
	assert_eq!(pending(&routed), 0, "an update back to the committed claim");

	for _ in 0..100 {
		routed.receive(start("room/guest")).unwrap();
		let id = routed.decoder.last();
		routed.receive(Msg::EndedId { id }).unwrap();
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
	let mut routed = super::Routed::asking(&subscriber, &prefix, 1, &Default::default()).unwrap();
	let peer = crate::route::node::Peer { tag: u64::MAX, node: 9 };
	let edge = (routed.node)
		.answer(peer, crate::route::Link::default(), Path::empty())
		.unwrap();
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
		prefix: 0,
		suffix: Path::empty(),
		offer: offer(),
	};
	for msg in [route, start("live/a"), start("live/b"), floor(0)] {
		routed.receive(msg).unwrap();
	}
	assert_eq!(
		batches(&mut routed),
		[(vec![("live/a".into(), true), ("live/b".into(), true)], Some(0))]
	);

	// Refused, live/a goes without a withdrawal certified for it.
	for msg in [refused(0), floor(1)] {
		routed.receive(msg).unwrap();
	}
	assert_eq!(batches(&mut routed), [(vec![("live/a".into(), false)], None)]);
	// Nor does live/b's change certify anything, while the route holds the refused claim.
	let update = Msg::Update {
		id: 1,
		hops: Default::default(),
		cost: Cost::new(5),
	};
	for msg in [update, floor(2)] {
		routed.receive(msg).unwrap();
	}
	assert_eq!(batches(&mut routed), [(vec![("live/b".into(), true)], None)]);

	// A usable restart brings live/a back, certified again.
	for msg in [restart(0), floor(3)] {
		routed.receive(msg).unwrap();
	}
	assert_eq!(batches(&mut routed), [(vec![("live/a".into(), true)], Some(3))]);

	// A refused restart withholds it as the update did.
	for msg in [refused(0), floor(4)] {
		routed.receive(msg).unwrap();
	}
	assert_eq!(batches(&mut routed), [(vec![("live/a".into(), false)], None)]);
}

/// A DEAD and the ROUTE_UPDATEs it counts apply as one: nothing changes before the
/// last update, which moves what they named and ends the rest. Another message
/// between them, an update of a route that is not live, not through the hop, or named
/// twice, and a count above the routes through the hop are each a violation.
#[kio::test]
async fn a_dead_and_its_updates_apply_as_one() {
	use crate::lite::Dead;
	let (subscriber, _consumer) = super::super::tests::restart_subscriber(SinkSession::new(Default::default()));
	let prefix = Path::empty();
	let hop = |session| Hop { node: 5, session };
	// Node 8's route reaches node 5 through node 7, over the same session as 7's own.
	let through = |node, session| Offer {
		path: match node {
			7 => [Hop { node: 7, session: 0 }, hop(session)].into(),
			_ => [Hop { node, session: 0 }, Hop { node: 7, session: 1 }, hop(session)].into(),
		},
		..offer()
	};
	let start = || {
		let mut routed = super::Routed::asking(&subscriber, &prefix, 1, &Default::default()).unwrap();
		for node in [7, 8] {
			let start = Msg::RouteStart {
				node,
				prefix: 0,
				suffix: Path::empty(),
				offer: through(node, 3),
			};
			routed.receive(start).unwrap();
		}
		// A route that does not cross the hop.
		let aside = Msg::RouteStart {
			node: 9,
			prefix: 0,
			suffix: Path::empty(),
			offer: Offer {
				path: [Hop { node: 9, session: 0 }].into(),
				..offer()
			},
		};
		routed.receive(aside).unwrap();
		routed
	};
	let dead = |moved| {
		Msg::Dead(Dead::Closed {
			hop: hop(3),
			far: 7,
			gone: true,
			moved,
		})
	};
	let update = |id, session| Msg::RouteUpdate {
		id,
		offer: through(7, session),
	};

	let mut routed = start();
	routed.receive(dead(1)).unwrap();
	assert_eq!(routed.routes.len(), 3);
	assert!(routed.routes[&0].path().contains(&hop(3)), "nothing applies before the last update");
	routed.receive(update(0, 4)).unwrap();
	let mut live: Vec<_> = routed.routes.keys().copied().collect();
	live.sort_unstable();
	assert_eq!(live, [0, 2], "the route it did not move ended");
	assert!(routed.routes[&0].path().contains(&hop(4)));

	// Another message between the DEAD and its updates.
	let mut routed = start();
	routed.receive(dead(1)).unwrap();
	let floor = Msg::Floor {
		id: 0,
		revision: Some(0),
	};
	assert!(routed.receive(floor).is_err());

	// An update of a route that is not live, and of one not through the hop.
	for id in [9, 2] {
		let mut routed = start();
		routed.receive(dead(1)).unwrap();
		assert!(routed.receive(update(id, 4)).is_err(), "route {id}");
	}

	// A route named twice.
	let mut routed = start();
	routed.receive(dead(2)).unwrap();
	routed.receive(update(0, 4)).unwrap();
	assert!(routed.receive(update(0, 5)).is_err());

	// More updates than the stream has routes through the hop.
	let mut routed = start();
	assert!(routed.receive(dead(3)).is_err());
}

/// A route this node refuses still follows a DEAD naming a hop on its path, so both
/// ends agree on which routes live: the closed session it crossed ends it, its claims
/// with it, and the peer may start it over. A refused moved offer withdraws the route
/// it names until a usable offer comes.
#[kio::test]
async fn a_refused_route_still_follows_a_dead() {
	use crate::lite::{Dead, Refusable, Refusal};
	let (subscriber, _consumer) = super::super::tests::restart_subscriber(SinkSession::new(Default::default()));
	let prefix = Path::empty();
	let mut routed = super::Routed::asking(&subscriber, &prefix, 1, &Default::default()).unwrap();
	let path = |session| -> std::sync::Arc<[Hop]> { [Hop { node: 7, session: 0 }, Hop { node: 5, session }].into() };
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
			prefix: 0,
			suffix: Some(Path::empty()),
			path: path(3),
		},
		reason: Refusal::Flags(8),
	};
	for msg in [refused, claim(0, "live/a"), floor(0)] {
		routed.receive(msg).unwrap();
	}
	routed.outputs(&subscriber);
	assert!(served(&routed).is_empty());

	// The session it crossed closed for good: it ends, and its Announce ID retires.
	routed
		.receive(closed(3, true, 0))
		.unwrap();
	assert!(routed.routes.is_empty() && routed.claims.is_empty() && routed.named.is_empty());
	assert!(routed.receive(Msg::EndedId { id: 0 }).is_err());

	// So the peer starts the route over, usably.
	let mut routed = super::Routed::asking(&subscriber, &prefix, 1, &Default::default()).unwrap();
	let usable = |session| Offer {
		path: path(session),
		..offer()
	};
	let route = |session| Msg::RouteStart {
		node: 7,
		prefix: 0,
		suffix: Path::empty(),
		offer: usable(session),
	};
	for msg in [
		Msg::Refused {
			message: Refusable::RouteStart {
				node: 7,
				prefix: 0,
				suffix: Some(Path::empty()),
				path: path(3),
			},
			reason: Refusal::Flags(8),
		},
		claim(0, "live/a"),
		closed(3, true, 0),
		route(4),
		claim(1, "live/b"),
		floor(1),
	] {
		routed.receive(msg).unwrap();
	}
	routed.outputs(&subscriber);
	assert_eq!(served(&routed), ["live/b"]);

	// It moved to an offer this node refuses: withdrawn, though the peer still holds it.
	let moved = Msg::Refused {
		message: Refusable::RouteUpdate { id: 1, path: path(5) },
		reason: Refusal::Flags(8),
	};
	routed.receive(closed(4, false, 1)).unwrap();
	routed.receive(moved).unwrap();
	routed.outputs(&subscriber);
	assert!(served(&routed).is_empty());
	assert!(routed.routes[&1].reach().is_none());

	// A usable offer files it again with its last floor's claims.
	let update = Msg::RouteUpdate {
		id: 1,
		offer: usable(6),
	};
	routed.receive(update).unwrap();
	assert!(routed.routes[&1].reach().is_some());
	assert_eq!(
		routed.routes[&1].committed.as_ref().map(|(claims, _)| claims.len()),
		Some(1)
	);
}

/// A DEAD the node does not credit, here naming the wrong far node, still changes the
/// stream's routes as the peer applies it, and the node hears each change as the
/// peer's own: a gone route ends there too, and a kept one holds, so neither side
/// keeps a route the other let go.
#[kio::test]
async fn an_uncredited_dead_keeps_the_node_in_step() {
	use crate::lite::Dead;
	let (subscriber, _consumer) = super::super::tests::restart_subscriber(SinkSession::new(Default::default()));
	let prefix = Path::empty();
	let shown = |routed: &super::Routed| -> Vec<(String, bool)> {
		(routed.node.shown().into_iter())
			.map(|((_, path), (_, available))| (path.to_string(), available))
			.collect()
	};
	for gone in [true, false] {
		let mut routed = super::Routed::asking(&subscriber, &prefix, 1, &Default::default()).unwrap();
		let route = Msg::RouteStart {
			node: 7,
			prefix: 0,
			suffix: Path::empty(),
			offer: Offer {
				path: [Hop { node: 7, session: 0 }, Hop { node: 5, session: 3 }].into(),
				..offer()
			},
		};
		let floor = Msg::Floor {
			id: 0,
			revision: Some(0),
		};
		for msg in [route, start("live"), floor] {
			routed.receive(msg).unwrap();
		}
		routed.outputs(&subscriber);
		assert_eq!(shown(&routed), [("live".to_string(), true)], "gone: {gone}");

		// Node 7, not 9, is the far end of the session node 5 learned the route on.
		let dead = Msg::Dead(Dead::Closed {
			hop: Hop { node: 5, session: 3 },
			far: 9,
			gone,
			moved: 0,
		});
		routed.receive(dead).unwrap();
		routed.outputs(&subscriber);
		match gone {
			true => {
				assert!(routed.routes.is_empty());
				assert_eq!(shown(&routed), []);
			}
			false => {
				assert!(routed.routes[&0].held);
				assert_eq!(shown(&routed), [("live".to_string(), false)]);
			}
		}
	}
}

/// A scoped peer's DRAIN ranks only its own routes through the node draining, which the
/// node keeps in their offers alone, so a new limit files them again still draining.
/// An unscoped peer's drain is the node's to remember, and the offers stay as sent.
#[kio::test]
async fn a_scoped_drain_survives_a_new_limit() {
	use crate::lite::Dead;
	let (subscriber, _consumer) = super::super::tests::restart_subscriber(SinkSession::new(Default::default()));
	let prefix = Path::empty();
	for (permit, scoped) in [(limited("live/**"), true), (Permit::default(), false)] {
		let mut routed = super::Routed::asking(&subscriber, &prefix, 1, &permit).unwrap();
		let route = Msg::RouteStart {
			node: 9,
			prefix: 0,
			suffix: Path::empty(),
			offer: Offer {
				path: [Hop { node: 9, session: 0 }, Hop { node: 7, session: 1 }].into(),
				..offer()
			},
		};
		routed.receive(route).unwrap();
		routed
			.receive(Msg::Dead(Dead::Drain { node: 9 }))
			.unwrap();
		let draining = |routed: &super::Routed| routed.routes[&0].offer.as_ref().unwrap().draining;
		assert_eq!(draining(&routed), scoped, "scoped: {scoped}");
		routed.relimit(&subscriber, &limited("live/a/**")).unwrap();
		assert_eq!(draining(&routed), scoped, "scoped: {scoped}");
	}
}

/// A route whose path leaves no room for this node's hop is one the node refuses as
/// ended, so its claims wait on the stream; a later shorter path files the route and
/// its claims again.
#[kio::test]
async fn a_route_shortened_from_the_hop_limit_files_its_claims() {
	use crate::route::MAX_HOPS;
	let (subscriber, _consumer) = super::super::tests::restart_subscriber(SinkSession::new(Default::default()));
	let prefix = Path::empty();
	let mut routed = super::Routed::asking(&subscriber, &prefix, 1, &Default::default()).unwrap();
	let along = |hops: usize| Offer {
		path: (0..hops as u64)
			.map(|i| Hop {
				node: 7 + i,
				session: u64::from(i > 0),
			})
			.collect(),
		..offer()
	};
	let shown = |routed: &super::Routed| -> Vec<String> {
		routed.node.shown().keys().map(|(_, path)| path.to_string()).collect()
	};
	let route = Msg::RouteStart {
		node: 7,
		prefix: 0,
		suffix: Path::empty(),
		offer: along(MAX_HOPS),
	};
	let floor = Msg::Floor {
		id: 0,
		revision: Some(0),
	};
	for msg in [route, start("live"), floor] {
		routed.receive(msg).unwrap();
	}
	routed.outputs(&subscriber);
	assert!(shown(&routed).is_empty(), "no room for this node's hop");

	let update = Msg::RouteUpdate {
		id: 0,
		offer: along(MAX_HOPS - 1),
	};
	routed.receive(update).unwrap();
	routed.outputs(&subscriber);
	assert_eq!(shown(&routed), ["live"]);
}
