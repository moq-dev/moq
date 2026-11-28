#![cfg(feature = "async")]

//! Races the lite-07 route layer: a publisher, a relay, and readers over mock sessions,
//! with every task run in a seeded order.
//!
//! Each session drains its own outbox on its own task, so the route layer's correctness
//! can hang on which task runs first, an order a test scripted on one thread never
//! varies. Each round sets up a relay carrying a few broadcasts, then races these steps,
//! each its own task under a seeded [`schedule`] that holds tasks back the way
//! descheduled threads would:
//!
//! - a broadcast ending as another starts on the same session,
//! - more broadcasts announced on that session, and every reader requesting each claim
//!   the moment it hears of it, so a request arrives as its route lands,
//! - a reader's grant widening, showing it claims it could not see,
//! - another publishing session closing, and a reader's leaving, during those edits.
//!
//! Once things settle, every reader must read each broadcast still live, show none that
//! ended, and have no request left unanswered. A failure prints the seed and round that
//! replay it, the steps taken before each failed request, and the round's trace. The
//! replay runs the same schedule, though moq-net's hash maps may reorder events within
//! one batch, so its trace can differ in that order.
//!
//! The schedule reorders whole task polls, so it reaches an order inside one poll only
//! where the poll yields, as at a [`kio::coop::Budget`]. Threads can interleave
//! anywhere. A session's announce loop yields between taking its peer's claims into the
//! node and serving them into the origin, which is where moq-dev/moq#9052's reader hung;
//! the extra announcements give that yield more chances to land between the two.
//!
//! `MOQ_RACE_SEED`, `MOQ_RACE_ROUND`, and `MOQ_RACE_ROUNDS` pick the rounds. Nightly
//! sweeps a fresh seed (`just test route-race`).

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::rc::Rc;
use std::time::Duration;

use moq_net::{
	Client, Error, Prefix, Prefixes, Server, Version,
	auth::Grant,
	origin::{self, RouteKind},
	track,
};

use crate::support::harness::{self, MockConnectOptions, MockPair, connect_mock};
use crate::support::mock::create_mock_session_pair;
use crate::support::schedule::{self, Schedule};
use crate::support::{self, AppendNext, Rng};

const VERSION: &str = "moq-lite-07-wip";

/// The only track each broadcast carries, holding one frame.
const TRACK: &str = "t";

/// Long enough, in simulated time, for anything in flight to land.
const SETTLE: Duration = Duration::from_secs(10);

/// Live from the start, ended by [`Step::Replace`].
const OLD: &str = "live/old";
/// Announced by [`Step::Replace`] on the same session.
const NEW: &str = "live/new";
/// Live from the start on the second session, which [`Step::Close`] closes.
const GONE: &str = "live/gone";
/// Live from the start, outside a narrow reader's grant until [`Step::Widen`].
const STAY: &str = "wide/stay";
/// What a narrow reader may see before its grant widens.
const NARROW: &str = "live";

/// One step a round races against the others.
#[derive(Clone, Copy, Debug)]
enum Step {
	/// End [`OLD`] and announce [`NEW`] on the same session, in either order.
	Replace { drop: bool, new_first: bool },
	/// Widen a narrow reader's grant to everything.
	Widen(usize),
	/// Close the second publishing session, carrying [`GONE`].
	Close(Close),
	/// A reader's session closes.
	Leave(usize),
	/// Announce another broadcast on the publisher's session, `live/<n>`.
	Announce(usize),
}

/// How [`Step::Close`] ends the session.
#[derive(Clone, Copy, Debug)]
enum Close {
	/// The publisher aborts it.
	Publisher,
	/// The relay aborts it.
	Relay,
	/// Both ends drop their last handle.
	Drop,
}

/// What a round runs, drawn from its seed.
#[derive(Debug)]
struct Shape {
	readers: usize,
	/// Which readers start with the grant of [`NARROW`] only.
	narrow: Vec<bool>,
	/// Whether the relay has settled before the steps start.
	settled: bool,
	/// Each step, after the executor passes it waits once started.
	steps: Vec<(Step, u64)>,
	/// The schedule's chance of holding a poll back, in 1/1024ths.
	chance: u64,
	/// The schedule's longest hold, in executor passes.
	longest: u64,
}

impl Shape {
	fn draw(rng: &mut Rng) -> Self {
		let readers = rng.range(2, 8) as usize;
		let narrow: Vec<bool> = (0..readers).map(|_| rng.next().is_multiple_of(2)).collect();
		let mut steps = vec![
			Step::Replace {
				drop: rng.next().is_multiple_of(2),
				new_first: rng.next().is_multiple_of(2),
			},
			Step::Close([Close::Publisher, Close::Relay, Close::Drop][rng.pick(3)]),
		];
		steps.extend((0..readers).filter(|reader| narrow[*reader]).map(Step::Widen));
		if rng.next().is_multiple_of(2) {
			steps.push(Step::Leave(rng.pick(readers)));
		}
		steps.extend((0..rng.range(0, 16) as usize).map(Step::Announce));
		// Started in a random order, each step's task then runs when the schedule lets it.
		for index in (1..steps.len()).rev() {
			steps.swap(index, rng.pick(index + 1));
		}
		let steps = steps.into_iter().map(|step| (step, rng.range(0, 64))).collect();
		Self {
			readers,
			narrow,
			settled: rng.next().is_multiple_of(4),
			steps,
			chance: rng.range(32, 512),
			longest: rng.range(1, 32),
		}
	}
}

/// How one reader's request for one claim it was shown went.
#[derive(Clone, Debug, PartialEq)]
enum Outcome {
	/// Still waiting.
	Pending,
	/// Read the broadcast's frame.
	Read,
	/// Ended cleanly with no group.
	Ended,
	/// Failed.
	Failed(String),
}

/// One reader's request for one claim it was shown.
struct Request {
	reader: usize,
	path: String,
	outcome: Outcome,
	/// The steps already taken when it was made: their effects were still in flight.
	after: Vec<String>,
}

/// What a round saw, shared by its tasks.
#[derive(Default)]
struct Log {
	trace: Vec<String>,
	/// The steps taken so far, in order. Each acts at once, but what it sets off lands later.
	taken: Vec<String>,
	/// Every request a reader made, in order.
	requests: Vec<Request>,
	/// Each reader's view: the paths it shows, with the request made for the latest showing.
	shown: BTreeMap<(usize, String), usize>,
	/// Readers whose session closed.
	left: Vec<usize>,
}

type Shared = Rc<RefCell<Log>>;

fn note(log: &Shared, line: String) {
	log.borrow_mut().trace.push(line);
}

fn produce_origin(hop: u64) -> origin::Producer {
	let mut config = origin::Config::new(std::num::NonZeroU64::new(hop).unwrap());
	config.proxy = true;
	let (producer, driver) = origin::Producer::new(config);
	harness::spawn(driver);
	producer
}

/// A broadcast with one frame on [`TRACK`], announced.
struct Live {
	route: origin::Route,
	_track: track::Producer,
}

fn publish(origin: &origin::Producer, path: &str) -> Live {
	let route = origin.route(path, origin::RouteOptions::hidden()).unwrap();
	let info = track::Info::default().with_timescale(moq_net::Timescale::MILLI);
	let track = route.broadcast().create_track(TRACK, info).unwrap();
	let group = track.append_next(path.as_bytes()).unwrap();
	group.finish().unwrap();
	route.announce().unwrap();
	Live { route, _track: track }
}

fn grant(subscribe: &str) -> Grant {
	let mut grant = Grant::default();
	grant.subscribe = Prefixes::from(Prefix::try_from(subscribe).unwrap());
	grant
}

/// A reader's session to the relay, its grant set before the relay's side first runs.
async fn connect_reader(relay: &origin::Producer, reader: &origin::Producer, narrow: bool) -> MockPair {
	let version: Version = VERSION.parse().unwrap();
	let mut mock = moq_net::transport::memory::Config::default();
	mock.protocol = Some(version.alpn().to_string());
	let (client_transport, server_transport) = create_mock_session_pair(mock);
	let transports = (client_transport.clone(), server_transport.clone());
	let client = Client::new()
		.with_versions(version.into())
		.with_subscriber(reader.clone());
	let server = Server::new()
		.with_versions(version.into())
		.with_publisher(relay.consume());
	let (client, server) = futures::join!(
		async {
			let (session, driver) = client.connect(harness::now(), client_transport).await.unwrap();
			harness::spawn(driver);
			session
		},
		async {
			let (session, driver) = server
				.handshake(harness::now(), server_transport)
				.await
				.unwrap()
				.ok()
				.await
				.unwrap();
			session.auth().authorize(&grant(if narrow { NARROW } else { "" }));
			harness::spawn(driver);
			session
		},
	);
	MockPair {
		client,
		server,
		client_transport: transports.0,
		server_transport: transports.1,
		client_discovery: None,
	}
}

/// Request `path` and read its frame, as a reader does the moment it hears of a claim.
async fn read(consumer: origin::Consumer, path: String) -> Outcome {
	let result = async {
		let broadcast = consumer.request(path.as_str(), None).await?;
		let subscription = track::Subscription::default().with_ranges([support::keys(0..)]);
		let mut sub = broadcast.track(TRACK)?.subscribe(subscription).await?;
		let Some(mut group) = sub.recv_group().await? else {
			return Ok(Outcome::Ended);
		};
		Ok::<_, Error>(match group.read_frame().await? {
			Some(_) => Outcome::Read,
			None => Outcome::Ended,
		})
	};
	result.await.unwrap_or_else(|err| Outcome::Failed(err.to_string()))
}

/// Follow every claim `reader` shows, requesting each the moment it appears.
fn follow(index: usize, reader: &origin::Producer, log: Shared) {
	let consumer = reader.consume();
	let mut routes = consumer.routes();
	drop(schedule::spawn(async move {
		while let Some(event) = routes.next().await {
			let path = event.announce.prefix.to_string();
			note(&log, format!("reader {index} hears {:?} {path}", event.kind));
			match event.kind {
				RouteKind::Start | RouteKind::Restart => {
					let request = {
						let mut log = log.borrow_mut();
						let request = log.requests.len();
						let after = log.taken.clone();
						log.requests.push(Request {
							reader: index,
							path: path.clone(),
							outcome: Outcome::Pending,
							after,
						});
						log.shown.insert((index, path.clone()), request);
						request
					};
					let (consumer, log) = (consumer.clone(), log.clone());
					drop(schedule::spawn(async move {
						let outcome = read(consumer, path.clone()).await;
						note(
							&log,
							format!("reader {index} request {request} for {path}: {outcome:?}"),
						);
						log.borrow_mut().requests[request].outcome = outcome;
					}));
				}
				RouteKind::Update => {}
				RouteKind::End | RouteKind::Offline => {
					log.borrow_mut().shown.remove(&(index, path));
				}
				kind => panic!("unexpected route kind {kind:?}"),
			}
		}
	}));
}

/// The broadcasts and sessions the steps act on.
struct World {
	old: Option<Live>,
	new: Option<Live>,
	/// What [`Step::Announce`] published.
	more: Vec<Live>,
	publisher: origin::Producer,
	second: Option<MockPair>,
	readers: Vec<Option<MockPair>>,
}

fn run(step: Step, world: &Rc<RefCell<World>>, log: &Shared) {
	let mut world = world.borrow_mut();
	match step {
		Step::Replace { drop: ended, new_first } => {
			let old = world.old.take().expect("replaced once");
			let end = |old: Live| match ended {
				true => drop(old),
				false => old.route.withdraw(),
			};
			let new = match new_first {
				true => {
					let new = publish(&world.publisher, NEW);
					end(old);
					new
				}
				false => {
					end(old);
					publish(&world.publisher, NEW)
				}
			};
			world.new = Some(new);
		}
		Step::Widen(reader) => {
			if let Some(pair) = &world.readers[reader] {
				pair.server.auth().authorize(&grant(""));
			}
		}
		Step::Close(close) => {
			let pair = world.second.take().expect("closed once");
			match close {
				Close::Publisher => pair.client.abort(Error::Cancel),
				Close::Relay => pair.server.abort(Error::Cancel),
				Close::Drop => drop(pair),
			}
		}
		Step::Announce(index) => {
			let live = publish(&world.publisher, &format!("{NARROW}/{index}"));
			world.more.push(live);
		}
		Step::Leave(reader) => {
			drop(world.readers[reader].take());
			log.borrow_mut().left.push(reader);
		}
	}
}

/// One round: set up, race the steps, settle, and check every reader.
async fn round(shape: &Shape) -> Result<(), String> {
	let _schedule = schedule::install(Schedule {
		chance: shape.chance,
		longest: shape.longest,
	});
	let log = Shared::default();
	let version: Version = VERSION.parse().unwrap();

	let publisher = produce_origin(1);
	let second = produce_origin(1);
	let relay = produce_origin(2);
	let old = publish(&publisher, OLD);
	let stay = publish(&publisher, STAY);
	let gone = publish(&second, GONE);

	let mut options = MockConnectOptions::new(version);
	options.client_publish = Some(publisher.consume());
	options.server_subscribe = Some(relay.clone());
	let _upstream = connect_mock(options).await;
	let mut options = MockConnectOptions::new(version);
	options.client_publish = Some(second.consume());
	options.server_subscribe = Some(relay.clone());
	let second_pair = connect_mock(options).await;

	let mut readers = Vec::new();
	for index in 0..shape.readers {
		let reader = produce_origin(3);
		follow(index, &reader, log.clone());
		readers.push(Some(connect_reader(&relay, &reader, shape.narrow[index]).await));
	}
	if shape.settled {
		kio::clock::sleep(SETTLE).await;
	}

	let world = Rc::new(RefCell::new(World {
		old: Some(old),
		new: None,
		more: Vec::new(),
		publisher,
		second: Some(second_pair),
		readers,
	}));
	let mut steps = Vec::new();
	for (step, lead) in &shape.steps {
		let (step, lead, world, log) = (*step, *lead, world.clone(), log.clone());
		let name = format!("{step:?}");
		steps.push(schedule::spawn(async move {
			for _ in 0..lead {
				kio::clock::sim::yield_now().await;
			}
			note(&log, format!("step {name}"));
			run(step, &world, &log);
			log.borrow_mut().taken.push(name);
		}));
	}
	for step in steps {
		step.await.expect("a step panicked");
	}
	kio::clock::sleep(SETTLE).await;

	let log = log.borrow();
	let mut failures = Vec::new();
	for (index, request) in log.requests.iter().enumerate() {
		if request.outcome == Outcome::Pending {
			failures.push(format!(
				"reader {}'s request {index} for {} hangs, made after steps {:?}",
				request.reader, request.path, request.after
			));
		}
	}
	for reader in (0..shape.readers).filter(|reader| !log.left.contains(reader)) {
		for path in [OLD, GONE] {
			if log.shown.contains_key(&(reader, path.to_string())) {
				failures.push(format!("reader {reader} still shows {path}, which ended"));
			}
		}
		let more = shape.steps.iter().filter_map(|(step, _)| match step {
			Step::Announce(index) => Some(format!("{NARROW}/{index}")),
			_ => None,
		});
		for path in [NEW.to_string(), STAY.to_string()].into_iter().chain(more) {
			match log.shown.get(&(reader, path.clone())) {
				None => failures.push(format!("reader {reader} never shows {path}")),
				Some(index) => {
					let request = &log.requests[*index];
					if !matches!(request.outcome, Outcome::Read | Outcome::Pending) {
						failures.push(format!(
							"reader {reader}'s request {index} for {path}, a claim it shows: {:?}, made after steps {:?}",
							request.outcome, request.after
						));
					}
				}
			}
		}
	}
	drop((stay, gone));
	if failures.is_empty() {
		return Ok(());
	}
	let mut report = failures.join("\n");
	let _ = write!(report, "\n\ntrace:\n  {}", log.trace.join("\n  "));
	Err(report)
}

/// The rounds of a sweep from `seed`, each with its own seed.
fn round_seed(seed: u64, index: u64) -> u64 {
	Rng(seed ^ index.wrapping_mul(0xA24B_AED4_963E_E407)).next()
}

fn setting(name: &str, fallback: u64) -> u64 {
	std::env::var(name).map_or(fallback, |value| value.parse().expect(name))
}

/// Runs the rounds `MOQ_RACE_SEED`, `MOQ_RACE_ROUND`, and `MOQ_RACE_ROUNDS` pick.
#[test]
fn seeded_races_hold_every_claim_shown() {
	let seed = setting("MOQ_RACE_SEED", 0x726163);
	let first = setting("MOQ_RACE_ROUND", 0);
	let rounds = setting("MOQ_RACE_ROUNDS", 16);
	eprintln!("MOQ_RACE_SEED={seed} MOQ_RACE_ROUNDS={rounds}");
	for index in first..first + rounds {
		let round_seed = round_seed(seed, index);
		let mut rng = Rng(round_seed);
		let shape = Shape::draw(&mut rng);
		let replay = format!("replay it with MOQ_RACE_SEED={seed} MOQ_RACE_ROUND={index} MOQ_RACE_ROUNDS=1");
		let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
			kio::clock::sim::run_seeded(round_seed, round(&shape))
		}));
		match result {
			Ok(Ok(())) => {}
			Ok(Err(report)) => panic!("round {index} failed; {replay}\n{shape:#?}\n{report}"),
			Err(panic) => {
				eprintln!("round {index} panicked; {replay}\n{shape:#?}");
				std::panic::resume_unwind(panic);
			}
		}
	}
}
