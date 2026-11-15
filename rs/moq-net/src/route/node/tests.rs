use super::*;

fn p(path: &'static str) -> Path {
	Path::from_static(path)
}

fn claim(instance: &Instance, cost: u64) -> Claim {
	Claim {
		instance: instance.clone(),
		epoch: 0,
		cost,
		upstream: false,
		learned: false,
		chain: crate::Hops::new(),
	}
}

fn clock() -> Deadlines {
	Deadlines::new(Instant::ZERO)
}

fn peer(tag: u64, node: Id) -> Peer {
	Peer { tag, node }
}

fn outputs(stream: &Stream) -> Vec<Output> {
	std::iter::from_fn(|| stream.try_output()).collect()
}

/// Deliver everything `from` would send on the wire to the stream receiving it,
/// returning what else it queued.
fn pump(from: &Stream, to: &Stream) -> Vec<Output> {
	let mut rest = Vec::new();
	for output in outputs(from) {
		match output {
			Output::Route { id, prefix, reach } => to.route(id, prefix, reach),
			Output::Batch {
				id,
				prefix,
				changes,
				floor,
			} => to.batch(id, prefix, changes, floor),
			Output::Dead(dead) => to.dead(dead),
			other => rest.push(other),
		}
	}
	rest
}

/// The serve commands among a stream's outputs.
fn served(outputs: Vec<Output>) -> Vec<Output> {
	(outputs.into_iter())
		.filter(|output| matches!(output, Output::Serve { .. } | Output::Unserve { .. }))
		.collect()
}

/// The wire messages among a stream's outputs.
fn wire(outputs: Vec<Output>) -> Vec<Output> {
	(outputs.into_iter())
		.filter(|output| !matches!(output, Output::Serve { .. } | Output::Unserve { .. }))
		.collect()
}

/// The claim as a relay (node 2) serves it through the origin's (node 1) route.
fn served_at(claim: Claim, metric: Metric, primary: bool) -> Served {
	Served {
		claim,
		metric,
		rtt: None,
		available: true,
		draining: false,
		priority: 0,
		primary,
		selected: true,
		path: std::sync::Arc::new([1, 2]),
	}
}

#[test]
fn a_claim_is_served_through_the_stream_its_route_arrived_on() {
	let instance = Instance::random();
	let (origin, relay) = (Node::new(1, 1, clock()), Node::new(2, 2, clock()));
	let answer = origin.answer(peer(1, 2), Link::default(), Path::empty()).unwrap();
	let receive = relay.receive(peer(1, 1), Link::default()).unwrap();
	origin.own(p("live/x"), Some(claim(&instance, 5)));
	pump(&answer, &receive);
	assert_eq!(
		served(outputs(&receive)),
		[Output::Serve {
			id: 1,
			named: 1,
			claim: p("live/x"),
			prefix: Path::empty(),
			served: served_at(claim(&instance, 5), 1, true),
		}]
	);

	origin.own(p("live/x"), None);
	pump(&answer, &receive);
	assert_eq!(
		served(outputs(&receive)),
		[Output::Unserve {
			id: 1,
			claim: p("live/x"),
			prefix: Path::empty(),
			ended: true
		}]
	);
}

/// A claim covered by routes at two prefixes is served along both, and a reader follows
/// the narrower one.
#[test]
fn the_narrowest_live_route_is_primary() {
	let instance = Instance::random();
	let (origin, relay) = (Node::new(1, 1, clock()), Node::new(2, 2, clock()));
	let wide = origin.answer(peer(1, 2), Link::default(), Path::empty()).unwrap();
	let narrow = origin.answer(peer(2, 2), Link::default(), p("live")).unwrap();
	let via_wide = relay.receive(peer(1, 1), Link::default()).unwrap();
	let via_narrow = relay.receive(peer(2, 1), Link::default()).unwrap();
	origin.own(p("live/x"), Some(claim(&instance, 0)));
	pump(&wide, &via_wide);
	pump(&narrow, &via_narrow);
	let serve = |prefix: &'static str, primary| Output::Serve {
		id: 1,
		named: 1,
		claim: p("live/x"),
		prefix: p(prefix),
		served: served_at(claim(&instance, 0), 1, primary),
	};
	assert_eq!(served(outputs(&via_wide)), [serve("", false)]);
	assert_eq!(served(outputs(&via_narrow)), [serve("live", true)]);

	assert_eq!(
		relay.forward(3, 1, &p("live/x"), &Path::empty()),
		None,
		"no stream answers that session"
	);
	assert_eq!(origin.forward(1, 1, &p("live/x"), &Path::empty()), Some(Via::Here));
}

/// A request a peer sends follows the route this node offered it under the prefix it names.
#[test]
fn a_forwarded_request_follows_the_route_it_names() {
	let instance = Instance::random();
	let (origin, relay) = (Node::new(1, 1, clock()), Node::new(2, 2, clock()));
	let answer = origin.answer(peer(1, 2), Link::default(), Path::empty()).unwrap();
	let receive = relay.receive(peer(1, 1), Link::default()).unwrap();
	let downstream = relay.answer(peer(5, 3), Link::default(), Path::empty()).unwrap();
	origin.own(p("live/x"), Some(claim(&instance, 0)));
	pump(&answer, &receive);
	outputs(&receive)
		.into_iter()
		.filter_map(|output| match output {
			Output::Serve {
				id,
				claim,
				prefix,
				served,
				..
			} => Some((id, claim, prefix, served)),
			_ => None,
		})
		.for_each(|(id, claim, prefix, served)| receive.serving(id, &claim, &prefix, &served.claim));
	assert!(!wire(outputs(&downstream)).is_empty(), "the relay offers its route");
	assert_eq!(
		relay.forward(5, 1, &p("live/x"), &Path::empty()),
		Some(Via::Route { prefix: Path::empty() })
	);
}

/// A stream that ends while its session stays up ends its routes quietly; one that ends
/// with its session reports the hop dead to every neighbour sent a route through it.
#[test]
fn only_a_closed_session_reports_its_hop_dead() {
	for closed in [false, true] {
		let instance = Instance::random();
		let (origin, relay, leaf) = (
			Node::new(1, 1, clock()),
			Node::new(2, 2, clock()),
			Node::new(3, 3, clock()),
		);
		let answer = origin.answer(peer(1, 2), Link::default(), Path::empty()).unwrap();
		let receive = relay.receive(peer(1, 1), Link::default()).unwrap();
		let down = relay.answer(peer(2, 3), Link::default(), Path::empty()).unwrap();
		let up = leaf.receive(peer(1, 2), Link::default()).unwrap();
		origin.own(p("live/x"), Some(claim(&instance, 0)));
		pump(&answer, &receive);
		for output in served(outputs(&receive)) {
			if let Output::Serve {
				id,
				claim,
				prefix,
				served,
				..
			} = output
			{
				receive.serving(id, &claim, &prefix, &served.claim);
			}
		}
		pump(&down, &up);
		outputs(&up);

		let guard = closed.then(|| relay.closing(1));
		drop(receive);
		drop(guard);
		let sent = wire(outputs(&down));
		let dead = sent
			.iter()
			.any(|output| matches!(output, Output::Dead(Dead::Closed { .. })));
		assert_eq!(dead, closed, "{sent:?}");
		assert_eq!(
			relay.lock().router.deadline().is_some(),
			closed,
			"only a closed session casts doubt"
		);
	}
}

/// A route started and ended while the reader stalled leaves it nothing to read.
#[test]
fn a_route_ended_unread_leaves_nothing() {
	let instance = Instance::random();
	let origin = Node::new(1, 1, clock());
	let answer = origin.answer(peer(1, 2), Link::default(), Path::empty()).unwrap();
	outputs(&answer);
	origin.own(p("live/x"), Some(claim(&instance, 0)));
	origin.own(p("live/x"), None);
	// The origin's own route stays for the prefix; its claims net out.
	let read = outputs(&answer);
	let starts = (read.iter())
		.filter(
			|output| matches!(output, Output::Batch { changes, .. } if changes.iter().any(|(_, claim)| claim.is_some())),
		)
		.count();
	assert_eq!(starts, 0, "{read:?}");
}

/// However many changes queue while the reader stalls, it reads the net change.
#[test]
fn a_stalled_reader_holds_the_net_change() {
	let origin = Node::new(1, 1, clock());
	let answer = origin.answer(peer(1, 2), Link::default(), Path::empty()).unwrap();
	outputs(&answer);
	let instance = Instance::random();
	for cost in 0..1000 {
		origin.own(p("live/x"), Some(claim(&instance, cost)));
	}
	let read = outputs(&answer);
	let batches: Vec<_> = (read.iter())
		.filter_map(|output| match output {
			Output::Batch { changes, .. } => Some(changes.clone()),
			_ => None,
		})
		.collect();
	assert_eq!(batches, [vec![(p("live/x"), Some(claim(&instance, 999)))]]);
}

/// A DEAD applies to the routes the peer holds when it reads it, so no route change
/// queued after it is read before it.
#[test]
fn nothing_moves_across_a_dead() {
	let offer = |metric| Offer {
		path: [super::super::path::Hop { node: 1, session: 0 }].into(),
		draining: false,
		priority: 0,
		metric,
		down_only: false,
		rtt: None,
	};
	let mut outbox = Outbox::default();
	let mut waiting = Waiting::default();
	let key = (1, Path::empty());
	outbox.route(7, key.clone(), Some(Reach::Finite(offer(1))), &mut waiting);
	outbox.dead(7, Dead::Draining { node: 1 }, &mut waiting);
	outbox.route(7, key.clone(), Some(Reach::Finite(offer(2))), &mut waiting);
	let read: Vec<Output> = std::iter::from_fn(|| outbox.pop()).collect();
	let route = |metric| Output::Route {
		id: 1,
		prefix: Path::empty(),
		reach: Some(Reach::Finite(offer(metric))),
	};
	assert_eq!(read, [route(1), Output::Dead(Dead::Draining { node: 1 }), route(2)]);
}

/// A reader that falls behind repeated failures of the routes it holds overruns its
/// stream, while one behind a failure of every hop a path can cross does not, and
/// reading what was queued clears it. A route a DEAD ends counts no longer, so neither
/// does a publisher that came and went.
#[test]
fn a_reader_behind_repeated_failures_overruns() {
	use super::super::path::Hop;
	let offer = |session| Offer {
		path: [Hop { node: 1, session: 0 }, Hop { node: 2, session }].into(),
		draining: false,
		priority: 0,
		metric: 1,
		down_only: false,
		rtt: None,
	};
	let dead = |session, gone| Dead::Closed {
		hop: Hop { node: 2, session },
		far: 3,
		moved: Vec::new(),
		gone,
	};

	// One route whose hop keeps failing, the route moving on after each.
	let mut outbox = Outbox::default();
	let mut waiting = Waiting::default();
	let key = (1, Path::empty());
	outbox.route(7, key.clone(), Some(Reach::Finite(offer(0))), &mut waiting);
	let mut failures = 0;
	while !outbox.overrun() {
		failures += 1;
		outbox.dead(7, dead(failures, false), &mut waiting);
		outbox.route(7, key.clone(), Some(Reach::Finite(offer(failures))), &mut waiting);
	}
	assert!(failures > MAX_HOPS as u64, "overran {failures} failures behind");
	assert_eq!(std::iter::from_fn(|| outbox.pop()).count() as u64, 2 * failures + 1);
	assert!(!outbox.overrun());
	assert_eq!(outbox.sealed, 0, "everything read");

	// Publishers that each come and go with their session, the DEAD ending each route.
	let mut outbox = Outbox::default();
	let mut publishers = 0;
	while !outbox.overrun() {
		publishers += 1;
		assert!(publishers < 1000, "a reader holding no route never overran");
		let key = (publishers, Path::empty());
		outbox.route(7, key, Some(Reach::Finite(offer(publishers))), &mut waiting);
		outbox.dead(7, dead(publishers, true), &mut waiting);
	}
	assert!(outbox.known.is_empty(), "every route ended with its DEAD");

	// Many routes through one hop queued unread, which one DEAD ends together: a
	// single failure, however many routes it ends, never overruns.
	let mut outbox = Outbox::default();
	let routes = 4 * MAX_HOPS as u64;
	for id in 1..=routes {
		outbox.route(7, (id, Path::empty()), Some(Reach::Finite(offer(1))), &mut waiting);
	}
	outbox.dead(7, dead(1, true), &mut waiting);
	assert!(!outbox.overrun(), "{} sealed", outbox.sealed);
	let read = std::iter::from_fn(|| outbox.pop()).count() as u64;
	assert_eq!(read, routes + 1, "each route's start, then the DEAD that ends it");
	assert_eq!(outbox.sealed, 0, "everything read");

	// The same routes sealed by an earlier DEAD, which leaves them up, then ended by a
	// later one: what the first sealed stops counting once they end.
	let mut outbox = Outbox::default();
	for id in 1..=routes {
		outbox.route(7, (id, Path::empty()), Some(Reach::Finite(offer(1))), &mut waiting);
	}
	outbox.dead(7, Dead::Draining { node: 9 }, &mut waiting);
	outbox.dead(7, dead(1, true), &mut waiting);
	assert!(!outbox.overrun(), "{} sealed", outbox.sealed);
	let read = std::iter::from_fn(|| outbox.pop()).count() as u64;
	assert_eq!(read, routes + 2, "each route's start, then both DEADs");
	assert_eq!(outbox.sealed, 0, "everything read");
	assert!(outbox.sealing.is_empty());
}

/// A claim started and withdrawn before the reader read it leaves nothing queued, so a
/// route whose claims churn at fresh paths keeps only its live ones however slowly the
/// peer reads. A withdrawal of a claim the reader holds still goes, and so does one of a
/// claim sent ahead of a DEAD the reader has yet to read.
#[test]
fn a_claim_started_and_withdrawn_unread_leaves_nothing() {
	let offer = Offer {
		path: [super::super::path::Hop { node: 1, session: 0 }].into(),
		draining: false,
		priority: 0,
		metric: 1,
		down_only: false,
		rtt: None,
	};
	let instance = Instance::random();
	let mut outbox = Outbox::default();
	let mut waiting = Waiting::default();
	let key = (1, Path::empty());
	let mut batch = |outbox: &mut Outbox, path: Path, claim: Option<Claim>, floor| {
		let changes = (vec![(path, claim)], Some(floor));
		outbox.batch(7, key.clone(), changes, |_, _| None, &mut waiting);
	};
	let read = |changes: Vec<(Path, Option<Claim>)>, floor| Output::Batch {
		id: 1,
		prefix: Path::empty(),
		changes,
		floor: Some(floor),
	};
	outbox.route(7, key.clone(), Some(Reach::Finite(offer)), &mut Waiting::default());
	batch(&mut outbox, p("live/held"), Some(claim(&instance, 0)), 1);
	assert_eq!(std::iter::from_fn(|| outbox.pop()).count(), 2);

	for n in 0..100 {
		let path = Path::new(format!("live/{n}").as_str()).unwrap();
		batch(&mut outbox, path.clone(), Some(claim(&instance, 0)), 2 + 2 * n);
		batch(&mut outbox, path, None, 3 + 2 * n);
	}
	batch(&mut outbox, p("live/held"), None, 500);
	let sent: Vec<Output> = std::iter::from_fn(|| outbox.pop()).collect();
	assert_eq!(sent, [read(vec![(p("live/held"), None)], 500)]);
	assert!(outbox.claims.is_empty(), "the reader holds no claim");

	batch(&mut outbox, p("live/a"), Some(claim(&instance, 0)), 501);
	outbox.dead(7, Dead::Draining { node: 1 }, &mut Waiting::default());
	batch(&mut outbox, p("live/a"), None, 502);
	let sent: Vec<Output> = std::iter::from_fn(|| outbox.pop()).collect();
	assert_eq!(
		sent,
		[
			read(vec![(p("live/a"), Some(claim(&instance, 0)))], 501),
			Output::Dead(Dead::Draining { node: 1 }),
			read(vec![(p("live/a"), None)], 502),
		]
	);
}

/// A route the reader read ends even when its ROUTE_END is queued alone, with no batch
/// withdrawing its claims ahead of it, as when a hold ends.
#[test]
fn a_read_route_ends_alone() {
	let offer = Offer {
		path: [super::super::path::Hop { node: 1, session: 0 }].into(),
		draining: false,
		priority: 0,
		metric: 1,
		down_only: false,
		rtt: None,
	};
	let mut outbox = Outbox::default();
	let mut waiting = Waiting::default();
	let key = (1, Path::empty());
	outbox.route(7, key.clone(), Some(Reach::Finite(offer)), &mut waiting);
	outbox.dead(7, Dead::Draining { node: 1 }, &mut waiting);
	assert_eq!(std::iter::from_fn(|| outbox.pop()).count(), 2);
	outbox.route(7, key, None, &mut waiting);
	let read: Vec<Output> = std::iter::from_fn(|| outbox.pop()).collect();
	assert_eq!(
		read,
		[Output::Route {
			id: 1,
			prefix: Path::empty(),
			reach: None
		}]
	);
}

/// A relay sends a claim on only once a stream serving it confirmed its instance, so a
/// peer never names an instance this relay's origin cannot route yet.
#[test]
fn a_claim_goes_on_only_once_served() {
	let instance = Instance::random();
	let (origin, relay) = (Node::new(1, 1, clock()), Node::new(2, 2, clock()));
	let answer = origin.answer(peer(1, 2), Link::default(), Path::empty()).unwrap();
	let receive = relay.receive(peer(1, 1), Link::default()).unwrap();
	let down = relay.answer(peer(2, 3), Link::default(), Path::empty()).unwrap();
	outputs(&down);
	origin.own(p("live/x"), Some(claim(&instance, 0)));
	pump(&answer, &receive);
	let claims = |read: Vec<Output>| -> Vec<Path> {
		(read.into_iter())
			.filter_map(|output| match output {
				Output::Batch { changes, .. } => Some(changes),
				_ => None,
			})
			.flatten()
			.filter_map(|(path, claim)| claim.map(|_| path))
			.collect()
	};
	assert!(claims(outputs(&down)).is_empty(), "held back until served");
	for output in served(outputs(&receive)) {
		if let Output::Serve {
			id,
			claim,
			prefix,
			served,
			..
		} = output
		{
			receive.serving(id, &claim, &prefix, &served.claim);
		}
	}
	assert_eq!(claims(outputs(&down)), [p("live/x")]);
}

/// The router's next timer reaches the task that fires it, and firing it lifts the doubt.
#[test]
fn the_doubt_timer_fires_on_the_origins_clock() {
	let instance = Instance::random();
	let clock = clock();
	let (origin, relay) = (
		Node::new(1, 1, Deadlines::new(Instant::ZERO)),
		Node::new(2, 2, clock.clone()),
	);
	let answer = origin.answer(peer(1, 2), Link::default(), Path::empty()).unwrap();
	let receive = relay.receive(peer(1, 1), Link::default()).unwrap();
	origin.own(p("live/x"), Some(claim(&instance, 0)));
	pump(&answer, &receive);
	let guard = relay.closing(1);
	drop(receive);
	drop(guard);
	let waiter = kio::Waiter::noop();
	let Poll::Ready(Some(at)) = relay.poll_deadline(None, &waiter) else {
		panic!("a doubt is timed");
	};
	assert_eq!(at, Instant::ZERO.saturating_add(Doubt::DEFAULT.timer));
	clock.advance(at);
	relay.timeout();
	assert_eq!(relay.poll_deadline(Some(at), &waiter), Poll::Ready(None));
}

/// A route that already crossed as many nodes as one may is not forwarded, since this
/// node's hop would take it past what any peer can decode, so a session never fails to
/// encode one. One hop fewer forwards at exactly the limit.
#[test]
fn a_route_at_the_hop_limit_is_not_forwarded() {
	use crate::route::MAX_HOPS;
	for (hops, forwarded) in [(MAX_HOPS - 1, Some(MAX_HOPS)), (MAX_HOPS, None)] {
		let relay = Node::new(2, 2, clock());
		let from_a = relay.receive(peer(1, 100 + hops as Id - 1), Link::default()).unwrap();
		let to_b = relay.answer(peer(2, 3), Link::default(), Path::empty()).unwrap();
		let offer = Offer {
			path: (0..hops as Id)
				.map(|i| super::super::path::Hop {
					node: 100 + i,
					session: 1,
				})
				.collect(),
			draining: false,
			priority: i8::MAX,
			metric: 1,
			down_only: false,
			rtt: None,
		};
		from_a.route(100, Path::empty(), Some(Reach::Finite(offer)));
		// A route goes on the wire only with a claim it shows, once that is served.
		let live = claim(&Instance::random(), 0);
		from_a.batch(100, Path::empty(), vec![(p("live"), Some(live.clone()))], Some(0));
		from_a.serving(100, &p("live"), &Path::empty(), &live);
		let sent: Vec<Offer> = (outputs(&to_b).into_iter())
			.filter_map(|output| match output {
				Output::Route {
					reach: Some(Reach::Finite(offer)),
					..
				} => Some(offer),
				_ => None,
			})
			.collect();
		assert_eq!(
			sent.iter().map(|offer| offer.path.len()).max(),
			forwarded,
			"{hops} hops"
		);
		for offer in sent {
			let start = crate::lite::AnnounceBroadcast::RouteStart {
				node: 100,
				suffix: Path::empty(),
				offer,
			};
			crate::coding::Encode::encode_bytes(&start, crate::lite::Version::Lite07).unwrap();
		}
	}
}

/// Serve every claim a stream's outputs ask it to, as its session would.
fn serve_all(stream: &Stream) {
	for output in served(outputs(stream)) {
		if let Output::Serve {
			id,
			claim,
			prefix,
			served,
			..
		} = output
		{
			stream.serving(id, &claim, &prefix, &served.claim);
		}
	}
}

/// What a stream holds back leaves nothing waiting once it goes, whether withdrawn,
/// confirmed, or closed with the stream, so nothing builds up as claims churn.
#[test]
fn a_held_claim_leaves_nothing_waiting_once_it_goes() {
	let instance = Instance::random();
	let (origin, relay) = (Node::new(1, 1, clock()), Node::new(2, 2, clock()));
	let answer = origin.answer(peer(1, 2), Link::default(), Path::empty()).unwrap();
	let receive = relay.receive(peer(1, 1), Link::default()).unwrap();
	let waiting = || -> usize {
		(relay.lock().waiting.values())
			.flat_map(BTreeMap::values)
			.map(BTreeSet::len)
			.sum()
	};

	let edge = relay.answer(peer(2, 3), Link::default(), Path::empty()).unwrap();
	origin.own(p("live"), Some(claim(&instance, 0)));
	pump(&answer, &receive);
	assert_eq!(waiting(), 1);
	serve_all(&receive);
	assert_eq!(waiting(), 0, "confirmed");

	// The route stays up through the confirmed claim while the one below churns.
	origin.own(p("live/a"), Some(claim(&instance, 0)));
	pump(&answer, &receive);
	assert_eq!(waiting(), 1);
	origin.own(p("live/a"), None);
	pump(&answer, &receive);
	assert_eq!(waiting(), 0, "withdrawn");

	origin.own(p("live/a"), Some(claim(&instance, 0)));
	pump(&answer, &receive);
	assert_eq!(waiting(), 1);
	drop(edge);
	assert_eq!(waiting(), 0, "closed");
}

/// A peer's GOAWAY moves the route this node forwards onto another stream at once.
#[test]
fn a_goaway_moves_the_forwarded_route_to_another_stream() {
	let instance = Instance::random();
	let relay = Node::new(2, 2, clock());
	let edge = relay.answer(peer(3, 3), Link::default(), Path::empty()).unwrap();
	let first = relay.receive(peer(1, 10), Link::default()).unwrap();
	let second = relay.receive(peer(2, 20), Link::default()).unwrap();
	for (stream, node, metric) in [(&first, 10, 1), (&second, 20, 2)] {
		let offer = Offer {
			path: [Hop { node: 5, session: 0 }, Hop { node, session: 1 }].into(),
			draining: false,
			priority: i8::MAX,
			metric,
			down_only: false,
			rtt: None,
		};
		let live = claim(&instance, 0);
		stream.route(5, Path::empty(), Some(Reach::Finite(offer)));
		stream.batch(5, Path::empty(), vec![(p("live/x"), Some(live.clone()))], Some(1));
		stream.serving(5, &p("live/x"), &Path::empty(), &live);
	}
	let paths = |stream: &Stream| -> Vec<Vec<Id>> {
		(outputs(stream).into_iter())
			.filter_map(|output| match output {
				Output::Route {
					id: 5,
					reach: Some(Reach::Finite(offer)),
					..
				} => Some(offer.path.iter().map(|hop| hop.node).collect()),
				_ => None,
			})
			.collect()
	};
	assert_eq!(paths(&edge), [vec![5, 10, 2]]);

	first.goaway();
	assert_eq!(paths(&edge), [vec![5, 20, 2]]);
}

/// Streams dropped while a session's streams close disconnect together, once the
/// last guard held over them goes.
#[test]
fn dropped_streams_disconnect_once_the_last_closing_guard_goes() {
	let instance = Instance::random();
	let relay = Node::new(2, 2, clock());
	let receive = relay.receive(peer(1, 1), Link::default()).unwrap();
	let offer = Offer {
		path: [Hop { node: 1, session: 0 }].into(),
		draining: false,
		priority: i8::MAX,
		metric: 0,
		down_only: false,
		rtt: None,
	};
	receive.route(1, Path::empty(), Some(Reach::Finite(offer)));
	receive.batch(
		1,
		Path::empty(),
		vec![(p("live/x"), Some(claim(&instance, 0)))],
		Some(1),
	);
	let (outer, inner) = (relay.closing(1), relay.closing(1));
	drop(receive);
	drop(inner);
	assert!(relay.shown().contains_key(&(1, p("live/x"))), "another guard is held");
	drop(outer);
	assert!(relay.shown().is_empty());
}

/// The origin declares its hop id, without the qualified marker, or a random
/// identity when nothing is left of it.
#[test]
fn the_origin_declares_its_hop_id_unqualified() {
	let id = |id| std::num::NonZeroU64::new(id).unwrap();
	assert_eq!(Node::origin(id(5), clock()).declared(), 5);
	assert_eq!(Node::origin(id(QUALIFIED | 5), clock()).declared(), 5);
	let drawn = Node::origin(id(QUALIFIED), clock()).declared();
	assert!((1..=GENERATED).contains(&drawn), "{drawn}");
}

/// A declared identity travels as itself on a lite-06 wire, and each qualified key
/// as its own hop id within the bits a generated one keeps, so a relay recognizes
/// only its own routes coming back.
#[test]
fn an_identity_travels_as_its_own_hop() {
	assert_eq!(hop(5).id(), 5);
	assert_eq!(hop(GENERATED + 1).id(), GENERATED + 1);
	let hops: BTreeSet<u64> = (0..256).map(|key| hop(QUALIFIED | key).id()).collect();
	assert_eq!(hops.len(), 256, "one hop per key");
	assert!(hops.iter().all(|id| (1..=GENERATED).contains(id)), "{hops:?}");
}

/// A claim held back for confirmation when a DEAD seals its batch goes after the DEAD
/// once confirmed, so the sealed batch's floor certifies nothing, since it cannot cover
/// a claim read only later, and the release closes with that floor instead.
#[test]
fn a_floor_sealed_while_a_claim_is_held_back_certifies_nothing() {
	let offer = Offer {
		path: [super::super::path::Hop { node: 1, session: 0 }].into(),
		draining: false,
		priority: 0,
		metric: 1,
		down_only: false,
		rtt: None,
	};
	let instance = Instance::random();
	let mut outbox = Outbox::default();
	let mut waiting = Waiting::default();
	let key = (1, Path::empty());
	outbox.route(7, key.clone(), Some(Reach::Finite(offer)), &mut waiting);
	let changes = vec![
		(p("live/x"), Some(claim(&instance, 0))),
		(p("live/y"), Some(claim(&instance, 0))),
	];
	let awaits = |path: &Path, _: &Claim| Some(*path == p("live/y"));
	outbox.batch(7, key.clone(), (changes, Some(5)), awaits, &mut waiting);
	outbox.dead(7, Dead::Draining { node: 1 }, &mut waiting);
	outbox.release(&key, &p("live/y"));

	let batches: Vec<(Vec<Path>, Option<Rev>)> = (std::iter::from_fn(|| outbox.pop()))
		.filter_map(|output| match output {
			Output::Batch { changes, floor, .. } => Some((changes.into_iter().map(|(path, _)| path).collect(), floor)),
			_ => None,
		})
		.collect();
	assert_eq!(batches.len(), 2, "{batches:?}");
	assert_eq!(
		batches[0],
		(vec![p("live/x")], None),
		"sealed before live/y was confirmed"
	);
	assert_eq!(
		batches[1],
		(vec![p("live/y")], Some(5)),
		"the release closes with the sealed floor"
	);
}
