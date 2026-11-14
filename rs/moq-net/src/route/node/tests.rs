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
		let dead = sent.iter().any(|output| matches!(output, Output::Dead(Dead::Closed { .. })));
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
		.filter(|output| matches!(output, Output::Batch { changes, .. } if changes.iter().any(|(_, claim)| claim.is_some())))
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
	outbox.dead(Dead::Draining { node: 1 });
	outbox.route(7, key.clone(), Some(Reach::Finite(offer(2))), &mut waiting);
	let read: Vec<Output> = std::iter::from_fn(|| outbox.pop()).collect();
	let route = |metric| Output::Route {
		id: 1,
		prefix: Path::empty(),
		reach: Some(Reach::Finite(offer(metric))),
	};
	assert_eq!(read, [route(1), Output::Dead(Dead::Draining { node: 1 }), route(2)]);
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
	let (origin, relay) = (Node::new(1, 1, Deadlines::new(Instant::ZERO)), Node::new(2, 2, clock.clone()));
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
