//! The policy driven over in-memory sessions on the simulated clock, so each timing is exact.

use super::*;

const INITIAL: Duration = Duration::from_millis(100);
const MAX: Duration = Duration::from_millis(400);

#[derive(Clone, Debug, thiserror::Error)]
enum TestError {
	#[error(transparent)]
	Net(#[from] Error),
	#[error(transparent)]
	Policy(#[from] Failure),
	#[error("dial failed")]
	Dial,
}

fn url(value: &str) -> Url {
	value.parse().expect("valid url")
}

/// What the peer at an address does with each dial.
#[derive(Clone)]
enum Peer {
	/// Accept, then drop the session at once.
	Close,
	/// Accept, then send a GOAWAY naming this URI (empty for a restart) and keep serving.
	Goaway(String),
	/// Accept and keep serving.
	Serve,
	/// Fail the dial, judged as this.
	Fail(Verdict),
	/// Never answer.
	Hang,
}

/// A target that dials fixed socket addresses.
#[derive(Clone)]
struct Fixed(Url);

impl From<Url> for Fixed {
	fn from(url: Url) -> Self {
		Self(url)
	}
}

impl Target for Fixed {
	fn url(&self) -> &Url {
		&self.0
	}

	fn fixed(&self) -> bool {
		true
	}
}

fn version() -> crate::Version {
	"moq-lite-07-wip".parse().unwrap()
}

/// Run an in-memory MoQ handshake, returning the client and server sessions.
async fn handshake() -> (Session, Session) {
	let (client, server) = crate::transport::memory::pair(crate::transport::memory::Config {
		protocol: Some(version().alpn().to_string()),
		..Default::default()
	});
	let dial = crate::Client::new().with_versions(version().into());
	let serve = crate::Server::new().with_versions(version().into());
	futures::join!(
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
	)
}

/// The next action that is not a wait, sleeping on the simulated clock meanwhile.
async fn next<A: Target, E: Clone + std::fmt::Display + From<Error> + From<Failure>>(
	machine: &mut Reconnect<A, E>,
) -> Action<A, E> {
	let mut sleep: Option<kio::clock::Sleep> = None;
	kio::wait(|waiter| {
		loop {
			match machine.poll(kio::clock::now(), waiter) {
				Action::Wait(Some(at)) => {
					let timer = sleep.get_or_insert_with(|| kio::clock::sleep_until(at));
					if timer.deadline() != at {
						timer.reset(at);
					}
					if waiter.poll_future(std::pin::Pin::new(timer)).is_pending() {
						return Poll::Pending;
					}
				}
				Action::Wait(None) => return Poll::Pending,
				action => return Poll::Ready(action),
			}
		}
	})
	.await
}

/// A record of one policy run.
#[derive(Default)]
struct Run {
	/// The URL and instant of each session that came up.
	accepts: Vec<(Url, Instant)>,
	/// The URL of every dial.
	dials: Vec<Url>,
	/// Every action other than a dial, a wait, or the end, by name.
	events: Vec<&'static str>,
	/// The server sides still serving.
	servers: Vec<Session>,
}

/// Drive `machine` against `peers` until it ends, or until `stop` says so.
async fn drive<A: Target>(
	machine: &mut Reconnect<A, TestError>,
	peers: &[(Url, Peer)],
	run: &mut Run,
	mut stop: impl FnMut(&Run) -> bool,
) -> Option<Result<(), TestError>> {
	while !stop(run) {
		let attempt = match next(machine).await {
			Action::Dial(attempt) => attempt,
			Action::Done(res) => return Some(res),
			Action::Abandon => {
				run.events.push("abandon");
				continue;
			}
			Action::Migrating => {
				run.events.push("migrating");
				continue;
			}
			Action::Retired => {
				run.events.push("retired");
				continue;
			}
			Action::Offline(_) => {
				run.events.push("offline");
				continue;
			}
			Action::Retrying(_) => {
				run.events.push("retrying");
				continue;
			}
			Action::Wait(_) => unreachable!("next never returns a wait"),
		};
		let url = attempt.target().url().clone();
		run.dials.push(url.clone());
		let (_, peer) = peers.iter().find(|(peer, _)| *peer == url).expect("an unknown peer");
		match peer {
			Peer::Hang => {}
			Peer::Fail(verdict) => machine.failed(kio::clock::now(), TestError::Dial, *verdict),
			Peer::Close | Peer::Goaway(_) | Peer::Serve => {
				let (client, server) = handshake().await;
				run.accepts.push((url, kio::clock::now()));
				match peer {
					Peer::Close => drop(server),
					Peer::Goaway(uri) => {
						let goaway = match uri.is_empty() {
							true => goaway::Goaway::new(),
							false => goaway::Goaway::redirect(uri.clone()),
						};
						server.drain().send(goaway).unwrap();
						run.servers.push(server);
					}
					_ => run.servers.push(server),
				}
				machine.connected(kio::clock::now(), client);
			}
		}
	}
	None
}

fn config(initial: Duration, timeout: Duration) -> Config {
	let mut config = Config::default();
	config.backoff.initial = initial;
	config.backoff.max = MAX;
	config.backoff.timeout = timeout;
	config
}

/// Check each gap between consecutive sessions against the jittered delay it should
/// have waited: between half of `delay` and all of it, starting at `initial` and
/// doubling up to `max`.
fn assert_paced(accepts: &[(Url, Instant)], initial: Duration, max: Duration) {
	let mut delay = initial;
	for (index, pair) in accepts.windows(2).enumerate() {
		let gap = pair[1].1 - pair[0].1;
		assert!(
			gap >= delay / 2 && gap <= delay,
			"retry {index} waited {gap:?}, outside the jittered {delay:?}"
		);
		delay = (delay * 2).min(max);
	}
}

/// A peer that accepts and then closes every session at once never earns the healthy
/// reset: the delay escalates to the cap, and the timeout ends the policy.
#[kio::test]
async fn an_immediate_close_loop_escalates_until_the_timeout() {
	const TIMEOUT: Duration = Duration::from_secs(2);
	let peer = url("tcp://peer:1/");
	let started = kio::clock::now();
	let mut machine = Reconnect::new(config(INITIAL, TIMEOUT), vec![peer.clone()], started).unwrap();
	let mut run = Run::default();

	let err = drive(&mut machine, &[(peer, Peer::Close)], &mut run, |_| false)
		.await
		.unwrap()
		.expect_err("a flapping peer kept the connection");
	assert!(
		matches!(err, TestError::Policy(Failure::Exhausted { last: Some(_), .. })),
		"{err}"
	);
	assert_eq!(kio::clock::now() - started, TIMEOUT, "the timeout did not end the policy");
	assert!(run.accepts.len() >= 6, "only {} sessions", run.accepts.len());
	assert_paced(&run.accepts, INITIAL, MAX);
}

/// A peer restarting with an empty-URI GOAWAY on every session paces the redials like
/// failures, yet never spends the timeout: each drained session still served.
#[kio::test]
async fn an_immediate_restart_loop_escalates_without_giving_up() {
	const TIMEOUT: Duration = Duration::from_millis(500);
	let peer = url("tcp://peer:1/");
	let mut machine = Reconnect::new(config(INITIAL, TIMEOUT), vec![peer.clone()], kio::clock::now()).unwrap();
	let mut run = Run::default();

	let ended = drive(&mut machine, &[(peer, Peer::Goaway(String::new()))], &mut run, |run| {
		run.accepts.len() == 10
	})
	.await;
	assert!(ended.is_none(), "a restarting peer ended the connection");
	assert!(
		run.accepts[9].1 - run.accepts[0].1 > TIMEOUT * 2,
		"the redials outran the backoff"
	);
	assert!(!run.events.contains(&"retrying"), "a restart is no failure");
	assert_paced(&run.accepts, INITIAL, MAX);
}

/// Two peers that redirect to each other the moment a session is up are failed attempts:
/// the delay escalates across both, and the timeout ends the policy.
#[kio::test]
async fn an_immediate_redirect_loop_escalates_until_the_timeout() {
	const TIMEOUT: Duration = Duration::from_secs(2);
	let a = url("tcp://peer:1/");
	let b = url("tcp://peer:2/");
	let peers = [
		(a.clone(), Peer::Goaway(b.to_string())),
		(b.clone(), Peer::Goaway(a.to_string())),
	];
	let started = kio::clock::now();
	let mut machine = Reconnect::new(config(INITIAL, TIMEOUT), vec![a.clone()], started).unwrap();
	let mut run = Run::default();

	let err = drive(&mut machine, &peers, &mut run, |_| false)
		.await
		.unwrap()
		.expect_err("a redirect loop kept the connection");
	assert!(err.to_string().contains("redirect"), "{err}");
	assert_eq!(kio::clock::now() - started, TIMEOUT, "the timeout did not end the policy");
	assert!(run.accepts.len() >= 6, "only {} sessions", run.accepts.len());
	for (index, (peer, _)) in run.accepts.iter().enumerate() {
		assert_eq!(*peer, if index % 2 == 0 { a.clone() } else { b.clone() }, "session {index}");
	}
	assert_paced(&run.accepts, INITIAL, MAX);
}

/// A zero `initial` turns retry waiting off: every redial follows the close at once.
#[kio::test]
async fn a_zero_initial_redials_at_once() {
	let peer = url("tcp://peer:1/");
	let first = kio::clock::now();
	let mut machine = Reconnect::new(
		config(Duration::ZERO, Duration::from_secs(1)),
		vec![peer.clone()],
		first,
	)
	.unwrap();
	let mut run = Run::default();

	drive(&mut machine, &[(peer, Peer::Close)], &mut run, |run| run.accepts.len() == 11).await;
	for (_, at) in &run.accepts {
		assert_eq!(*at, first, "a zero initial waited before a retry");
	}
}

/// The first address that answers wins, however many dead ones precede it.
#[kio::test]
async fn the_walk_lands_on_the_first_address_that_answers() {
	let targets = vec![url("tcp://a:1/"), url("tcp://b:1/"), url("tcp://c:1/")];
	let peers = [
		(targets[0].clone(), Peer::Fail(Verdict::Retry)),
		(targets[1].clone(), Peer::Fail(Verdict::Unusable)),
		(targets[2].clone(), Peer::Serve),
	];
	let started = kio::clock::now();
	let mut machine = Reconnect::new(Config::default(), targets.clone(), started).unwrap();
	let mut run = Run::default();

	drive(&mut machine, &peers, &mut run, |run| !run.accepts.is_empty()).await;
	assert_eq!(run.dials, targets, "every address in order");
	assert_eq!(run.accepts, [(targets[2].clone(), started)], "no wait between addresses");
	assert!(machine.session().is_some());
}

/// A black-holed address with a fallback gets the fixed bound; the last one runs to the
/// connect timeout.
#[kio::test]
async fn only_an_address_with_a_fallback_is_bounded() {
	let targets = vec![url("tcp://a:1/"), url("tcp://b:1/")];
	let peers = [(targets[0].clone(), Peer::Hang), (targets[1].clone(), Peer::Hang)];
	let mut config = Config::default();
	config.backoff.timeout = Duration::from_secs(1);
	let started = kio::clock::now();
	let mut machine = Reconnect::new(config, targets.clone(), started).unwrap();
	let mut run = Run::default();

	let err = drive(&mut machine, &peers, &mut run, |_| false)
		.await
		.unwrap()
		.expect_err("nothing answered");
	assert_eq!(run.events, ["abandon", "abandon", "retrying"]);
	assert_eq!(run.dials, targets, "a walk under way finishes past the retry window");
	assert_eq!(kio::clock::now() - started, ATTEMPT + config.timeout);
	// The give-up names the last address's connect timeout as its cause.
	let last = Failure::Timeout(config.timeout).to_string();
	assert!(
		matches!(&err, TestError::Policy(Failure::Exhausted { last: Some(cause), .. }) if *cause == last),
		"{err}"
	);
}

/// A settled answer from one address ends the walk instead of being buried by another's
/// transport failure, and the policy retries after a backoff.
#[kio::test]
async fn a_settled_answer_stops_the_walk() {
	let targets = vec![url("tcp://a:1/"), url("tcp://b:1/")];
	let peers = [
		(targets[0].clone(), Peer::Fail(Verdict::Settled)),
		(targets[1].clone(), Peer::Serve),
	];
	let mut machine = Reconnect::new(config(INITIAL, Duration::from_secs(1)), targets.clone(), kio::clock::now()).unwrap();
	let mut run = Run::default();

	drive(&mut machine, &peers, &mut run, |run| run.dials.len() == 2).await;
	assert_eq!(run.dials, [targets[0].clone(), targets[0].clone()], "the walk stopped at the answer");
	assert_eq!(run.events, ["retrying"]);
}

/// One unusable address must not turn another's transient failure terminal, but a walk
/// with nothing else ends on it.
#[kio::test]
async fn an_unusable_address_ends_only_a_walk_without_hope() {
	let targets = vec![url("tcp://a:1/"), url("tcp://b:1/")];
	let peers = [
		(targets[0].clone(), Peer::Fail(Verdict::Retry)),
		(targets[1].clone(), Peer::Fail(Verdict::Unusable)),
	];
	let mut machine = Reconnect::new(config(INITIAL, Duration::ZERO), targets.clone(), kio::clock::now()).unwrap();
	let mut run = Run::default();
	drive(&mut machine, &peers, &mut run, |run| run.dials.len() == 4).await;
	assert_eq!(run.events, ["retrying"], "an unlimited window keeps retrying");

	let lone = url("tcp://c:1/");
	let started = kio::clock::now();
	let mut machine = Reconnect::new(config(INITIAL, Duration::ZERO), vec![lone.clone()], started).unwrap();
	let err = drive(&mut machine, &[(lone, Peer::Fail(Verdict::Unusable))], &mut Run::default(), |_| false)
		.await
		.unwrap()
		.expect_err("an unusable address connected");
	assert!(matches!(err, TestError::Dial), "{err}");
	assert_eq!(kio::clock::now(), started, "ended without waiting");
}

/// A refused credential is terminal, unless the tokens changed since the dial read them.
#[kio::test]
async fn a_refusal_retries_only_new_tokens() {
	let peer = url("tcp://peer:1/");
	let tokens = auth::Tokens::new(vec!["old".parse().unwrap()]);
	let mut machine = Reconnect::new(config(INITIAL, Duration::ZERO), vec![peer.clone()], kio::clock::now())
		.unwrap()
		.with_tokens(tokens.clone(), version().into());

	let Action::Dial(_) = next(&mut machine).await else {
		panic!("expected a dial")
	};
	tokens.set(vec!["new".parse().unwrap()]);
	machine.failed(kio::clock::now(), TestError::Dial, Verdict::Refused);
	assert!(matches!(next(&mut machine).await, Action::Retrying(_)));

	let Action::Dial(_) = next(&mut machine).await else {
		panic!("expected a redial")
	};
	machine.failed(kio::clock::now(), TestError::Dial, Verdict::Refused);
	assert!(
		matches!(next(&mut machine).await, Action::Done(Err(TestError::Dial))),
		"an unchanged credential is refused for good"
	);
}

/// Ending the tokens' handle ends the policy at its next walk, rather than redialing
/// sessions the handle refuses.
#[kio::test]
async fn an_ended_auth_handle_ends_the_policy() {
	let peer = url("tcp://peer:1/");
	let tokens = auth::Tokens::new(Vec::new());
	let mut machine = Reconnect::new(config(INITIAL, Duration::ZERO), vec![peer.clone()], kio::clock::now())
		.unwrap()
		.with_tokens(tokens.clone(), version().into());
	let mut run = Run::default();

	drive(&mut machine, &[(peer.clone(), Peer::Serve)], &mut run, |run| !run.accepts.is_empty()).await;
	tokens.handle().abort(Error::App(9));
	run.servers.clear();
	let ended = drive(&mut machine, &[(peer, Peer::Serve)], &mut run, |_| false).await;
	assert!(
		matches!(ended, Some(Err(TestError::Net(Error::App(9))))),
		"the policy kept redialing on an ended auth handle"
	);
	assert_eq!(run.accepts.len(), 1);
}

/// Connecting once, a GOAWAY ends the policy cleanly and a close ends it with its reason.
#[kio::test]
async fn connecting_once_ends_with_the_session() {
	let peer = url("tcp://peer:1/");
	let config = Config {
		once: true,
		..Default::default()
	};

	let mut machine = Reconnect::new(config, vec![peer.clone()], kio::clock::now()).unwrap();
	let ended = drive(&mut machine, &[(peer.clone(), Peer::Goaway(String::new()))], &mut Run::default(), |_| false).await;
	assert!(matches!(ended, Some(Ok(()))), "a GOAWAY is the cue to leave");

	let mut machine = Reconnect::new(config, vec![peer.clone()], kio::clock::now()).unwrap();
	let ended = drive(&mut machine, &[(peer.clone(), Peer::Close)], &mut Run::default(), |_| false).await;
	assert!(matches!(ended, Some(Err(TestError::Net(_)))), "the close reason ends it");

	let mut machine = Reconnect::new(config, vec![peer.clone()], kio::clock::now()).unwrap();
	let ended = drive(&mut machine, &[(peer, Peer::Fail(Verdict::Retry))], &mut Run::default(), |_| false).await;
	assert!(matches!(ended, Some(Err(TestError::Dial))), "a failed dial is not retried");
}

/// A target dialing fixed socket addresses cannot follow a redirect.
#[kio::test]
async fn a_fixed_target_refuses_a_redirect() {
	let peer = url("moqt://relay.example:4443/");
	let moved = url("moqt://relay.example:5443/");
	let mut machine = Reconnect::new(Config::default(), vec![Fixed(peer.clone())], kio::clock::now()).unwrap();
	let ended = drive(&mut machine, &[(peer, Peer::Goaway(moved.to_string()))], &mut Run::default(), |_| false).await;
	assert!(matches!(ended, Some(Err(TestError::Policy(Failure::Fixed)))));
}

/// A redirect the policy refuses ends it rather than redialing the address the peer leaves.
#[kio::test]
async fn a_refused_redirect_is_terminal() {
	let peer = url("moqt://relay.example:4443/");
	let mut machine = Reconnect::new(Config::default(), vec![peer.clone()], kio::clock::now()).unwrap();
	let ended = drive(
		&mut machine,
		&[(peer, Peer::Goaway("moqt://elsewhere.example/".into()))],
		&mut Run::default(),
		|_| false,
	)
	.await;
	assert!(matches!(ended, Some(Err(TestError::Policy(Failure::Refused(_))))));
}

/// A healthy session's GOAWAY migrates at once to the named address, and the drained
/// predecessor closes as soon as the replacement serves and it carries nothing.
#[kio::test]
async fn a_healthy_goaway_hands_over_to_the_redirect() {
	let a = url("tcp://peer:1/");
	let b = url("tcp://peer:2/");
	let mut machine = Reconnect::new(Config::default(), vec![a.clone()], kio::clock::now()).unwrap();
	let mut run = Run::default();
	drive(&mut machine, &[(a.clone(), Peer::Serve)], &mut run, |run| !run.accepts.is_empty()).await;

	// Past the healthy bar, the GOAWAY is a handover, not a failure.
	kio::clock::sleep(Duration::from_secs(2)).await;
	run.servers[0].drain().send(goaway::Goaway::redirect(b.to_string())).unwrap();
	let peers = [(a.clone(), Peer::Serve), (b.clone(), Peer::Serve)];
	let migrated = kio::clock::now();
	drive(&mut machine, &peers, &mut run, |run| run.events.contains(&"retired")).await;
	assert_eq!(run.dials, [a, b.clone()]);
	assert_eq!(run.accepts[1], (b, migrated), "migrated without a backoff");
	assert_eq!(run.events, ["migrating", "retired"], "a replacement serves, so nothing went offline");
	assert!(machine.predecessor().is_none());
	assert!(machine.session().is_some());
}

/// Our handover window caps the peer's GOAWAY deadline, and a predecessor retiring while
/// no replacement serves leaves the connection offline.
#[kio::test]
async fn the_handover_takes_the_earlier_deadline() {
	const HANDOVER: Duration = Duration::from_millis(500);
	let peer = url("tcp://peer:1/");
	let mut config = Config::default();
	config.goaway.handover = HANDOVER;
	let mut machine = Reconnect::new(config, vec![peer.clone()], kio::clock::now()).unwrap();
	let mut run = Run::default();
	drive(&mut machine, &[(peer.clone(), Peer::Serve)], &mut run, |run| !run.accepts.is_empty()).await;

	kio::clock::sleep(Duration::from_secs(2)).await;
	run.servers[0]
		.drain()
		.send(goaway::Goaway::new().with_timeout(Duration::from_secs(3600)))
		.unwrap();
	let drained = kio::clock::now();
	drive(&mut machine, &[(peer, Peer::Hang)], &mut run, |run| run.events.contains(&"offline")).await;
	assert_eq!(run.events, ["migrating", "retired", "offline"]);
	assert_eq!(kio::clock::now() - drained, HANDOVER, "a longer peer deadline must not extend our cap");

	let goaway = Goaway::default();
	assert_eq!(goaway.handover(None), Duration::from_secs(10), "no deadline: ours");
	assert_eq!(
		goaway.handover(Some(Duration::from_secs(3))),
		Duration::from_secs(3),
		"a shorter peer deadline wins: it force-closes then anyway"
	);
}

/// Under [`Restart::Wait`] an empty-URI GOAWAY changes nothing while the peer keeps the
/// session: no migration and no redial. Its close then reconnects like any drop.
#[kio::test]
async fn a_waited_restart_redials_only_once_the_session_closes() {
	let peer = url("tcp://peer:1/");
	let mut config = config(INITIAL, Duration::from_secs(1));
	config.restart = Restart::Wait;
	let mut machine = Reconnect::new(config, vec![peer.clone()], kio::clock::now()).unwrap();
	let mut run = Run::default();
	drive(&mut machine, &[(peer.clone(), Peer::Serve)], &mut run, |run| !run.accepts.is_empty()).await;

	kio::clock::sleep(Duration::from_secs(2)).await;
	run.servers[0].drain().send(goaway::Goaway::new()).unwrap();
	kio::clock::sim::idle().await;
	assert!(
		matches!(machine.poll(kio::clock::now(), &kio::Waiter::noop()), Action::Wait(None)),
		"the policy left the session"
	);

	run.servers.remove(0).abort(Error::GoawayTimeout);
	drive(&mut machine, &[(peer, Peer::Serve)], &mut run, |run| run.accepts.len() == 2).await;
	assert_eq!(run.events, ["offline"]);
}

/// The policy must not log or report a credential riding in a dial URL.
///
/// A LAN mesh's membership proof rides as a URL path segment, since raw QUIC has no
/// headers to put it in, and a dial URL logged verbatim would hand a replayable credential
/// to anyone who can read logs. This drives the real `tracing` events rather than the
/// formatter alone, so it also catches a log site that forgot to use it.
#[test]
fn dialing_never_logs_the_credential() {
	const SECRET: &str = "b91d7fe20c4a";
	let targets = vec![
		url(&format!("moqt://user:{SECRET}@a.example/.cluster/{SECRET}?jwt={SECRET}")),
		url(&format!("moqt://b.example/.cluster/{SECRET}")),
	];
	let started = Instant::now();
	let mut machine = Reconnect::<Url, TestError>::new(Config::default(), targets, started).unwrap();
	let waiter = kio::Waiter::noop();

	let mut reported = Vec::new();
	let events = crate::test_tracing::capture(tracing::Level::DEBUG, || {
		assert!(matches!(machine.poll(started, &waiter), Action::Dial(_)));
		// The first address has a fallback, so it times out with an error naming it.
		assert!(matches!(machine.poll(started + ATTEMPT, &waiter), Action::Abandon));
		assert!(matches!(machine.poll(started + ATTEMPT, &waiter), Action::Dial(_)));
		machine.failed(started + ATTEMPT, TestError::Dial, Verdict::Retry);
		loop {
			match machine.poll(started + ATTEMPT, &waiter) {
				Action::Retrying(err) => reported.push(err.to_string()),
				Action::Wait(_) => break,
				action => panic!("unexpected {action:?}"),
			}
		}
	});

	// The log lines did happen, so the assertion below isn't vacuously true.
	assert!(events.iter().any(|event| event.contains("connecting")), "{events:?}");
	assert!(events.iter().any(|event| event.contains("a.example")), "{events:?}");
	for line in events.iter().chain(&reported) {
		assert!(!line.contains(SECRET), "leaked the credential: {line}");
	}
}

#[test]
fn invalid_configs_are_refused() {
	assert!(matches!(
		Reconnect::<Url, TestError>::new(Config::default(), Vec::new(), Instant::now()),
		Err(ConfigError::Empty)
	));
	let mut config = Config::default();
	config.backoff.healthy = Duration::ZERO;
	assert!(matches!(
		Reconnect::<Url, TestError>::new(config, vec![url("tcp://peer:1/")], Instant::now()),
		Err(ConfigError::Healthy)
	));
}

/// Each knob means one thing at zero: no wait, no cap, no growth. None of them touches
/// the healthy bar, which is what keeps an immediate close a failed attempt.
#[test]
fn zero_turns_each_pacing_knob_off() {
	let defaults = Backoff::default();

	let pacing = Pacing::new(&Backoff {
		initial: Duration::ZERO,
		..defaults
	});
	assert_eq!(pacing.initial, Duration::ZERO);
	assert_eq!(pacing.next(pacing.initial), Duration::ZERO, "zero initial never waits");
	assert_eq!(pacing.healthy, defaults.healthy);

	let pacing = Pacing::new(&Backoff {
		max: Duration::ZERO,
		..defaults
	});
	assert_eq!(pacing.max, None);
	let mut delay = pacing.initial;
	for _ in 0..10 {
		delay = pacing.next(delay);
	}
	assert_eq!(delay, defaults.initial * 1024, "zero max leaves the delay uncapped");
	assert_eq!(pacing.settled(), defaults.healthy);

	let pacing = Pacing::new(&Backoff {
		multiplier: 0,
		..defaults
	});
	assert_eq!(pacing.next(pacing.initial), pacing.initial, "zero multiplier never grows");

	// A cap below the first delay caps it too.
	let pacing = Pacing::new(&Backoff {
		max: Duration::from_millis(300),
		..defaults
	});
	assert_eq!(pacing.initial, Duration::from_millis(300));
}

/// A sane config is left alone: the delay doubles up to the cap and stays there.
#[test]
fn pacing_grows_to_the_cap() {
	let pacing = Pacing::new(&Backoff {
		initial: Duration::from_millis(100),
		multiplier: 2,
		max: Duration::from_millis(400),
		..Default::default()
	});

	assert_eq!(pacing.initial, Duration::from_millis(100));
	assert_eq!(pacing.settled(), Duration::from_millis(400));
	assert_eq!(pacing.next(Duration::from_millis(100)), Duration::from_millis(200));
	assert_eq!(pacing.next(Duration::from_millis(200)), Duration::from_millis(400));
	assert_eq!(pacing.next(Duration::from_millis(400)), Duration::from_millis(400));
}

/// Refusals and malformed input settle; everything else from MoQ is worth a retry.
#[test]
fn a_moq_error_is_judged_by_its_session_code() {
	assert_eq!(Verdict::from(&Error::Unauthorized), Verdict::Refused);
	assert_eq!(Verdict::from(&Error::Expired), Verdict::Refused);
	assert_eq!(Verdict::from(&Error::ProtocolViolation), Verdict::Fatal);
	assert_eq!(Verdict::from(&Error::Closed), Verdict::Retry);
	assert_eq!(Verdict::from(&Error::Transport("reset".into())), Verdict::Retry);
}
