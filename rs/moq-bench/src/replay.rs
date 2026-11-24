//! Mesh replay endpoint: `moq-bench replay`.
//!
//! One session to one relay that publishes named broadcasts, follows named paths, or both,
//! for a harness grading a relay mesh under faults (`test/mesh-replay`). The load generator
//! reads random broadcasts and gives up on one that ends; a reader here follows its path
//! across every instance that serves it, as a viewer waiting on a stream would, and reports
//! when its groups stopped arriving and for how long.
//!
//! Every line on the output is one JSON object stamped with `ms`, Unix milliseconds:
//!
//! - `online` / `offline`: an instance took over the path, or the path went offline.
//! - `ended`: the subscription to the instance handed out ended while it still served.
//! - `gap`: a group arrived more than `--gap` after the one before it, on any instance, or
//!   skipped groups of the instance it came from. `from` is when the one before it
//!   arrived, `missed` how many groups the publisher wrote in between when both came from
//!   one instance, counted across resubscribes to that instance.
//! - `status`: every `--report`, each path's state, groups read, and last arrival.

use std::collections::BTreeMap;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use bytes::Bytes;
use kio::clock::Wall;
use moq_net::{broadcast, track};
use serde::Serialize;

/// The one track each replay broadcast carries.
const TRACK: &str = "replay";

/// Publish and follow broadcasts through one relay, writing what each reader saw as JSON lines.
#[derive(usage_rs::Args, Clone, Debug)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct Args {
	/// Publish a broadcast at this path: one single-frame group per `--interval`.
	#[usage(long)]
	publish: Vec<String>,

	/// Follow the broadcast at this path across the instances that serve it.
	#[usage(long)]
	follow: Vec<String>,

	/// Time between the groups each published broadcast writes.
	#[usage(long, default = "100ms")]
	interval: moq_tokio::cli::Duration,

	/// The shortest pause between two arrivals reported as a `gap`.
	#[usage(long, default = "500ms")]
	gap: moq_tokio::cli::Duration,

	/// How often to write each followed path's status.
	#[usage(long, default = "1s")]
	report: moq_tokio::cli::Duration,

	/// Write JSON lines to this file instead of stdout. Truncates on start.
	#[usage(long, value_hint = usage_rs::ValueHint::FilePath, extensions("jsonl", "json"))]
	output: Option<std::path::PathBuf>,

	/// The relay to dial and how.
	#[usage(flatten)]
	client: moq_tokio::connect::Config,

	/// QUIC transport tuning (`--quic-*`).
	#[usage(flatten)]
	quic: moq_tokio::quic::Config,

	/// Log configuration.
	#[usage(flatten)]
	log: moq_tokio::Log,
}

// Parsed apart from the load generator, whose flags and `MOQ_*` variables configure a
// different workload.
#[derive(usage_rs::Cli)]
#[usage(name = "moq-bench replay", version = env!("CARGO_PKG_VERSION"))]
#[usage(settings)]
#[usage(unknown_flags = "error", args_override_self = false)]
struct Standalone {
	#[usage(flatten)]
	args: Args,
}

/// Parse the arguments after `replay`.
pub fn parse(argv: &[&std::ffi::OsStr]) -> anyhow::Result<Args> {
	match Standalone::parse_from(argv) {
		Ok(standalone) => {
			// Released spellings land on hidden fields nothing honors, so refuse them naming the replacement.
			let mut deprecated = standalone.args.client.deprecated();
			deprecated.extend(standalone.args.quic.deprecated());
			anyhow::ensure!(deprecated.is_empty(), "{deprecated}");
			Ok(standalone.args)
		}
		Err(err) => {
			let answer = moq_tokio::cli::answer(Standalone::spec(), Standalone::command(), argv, err);
			if answer.is_question() {
				answer.exit();
			}
			anyhow::bail!("{}", answer.message());
		}
	}
}

/// Publish and follow until interrupted, then close the session so the relay sees the source end.
pub async fn run(args: Args) -> anyhow::Result<()> {
	args.log.init()?;
	moq_tokio::crypto::install_default();
	let url = args
		.client
		.url
		.clone()
		.ok_or_else(|| anyhow::anyhow!("--connect is required"))?;
	anyhow::ensure!(
		!args.publish.is_empty() || !args.follow.is_empty(),
		"nothing to do: give --publish or --follow"
	);
	let interval: Duration = args.interval.into();
	anyhow::ensure!(!interval.is_zero(), "--interval must be greater than 0s");
	let report: Duration = args.report.into();
	anyhow::ensure!(!report.is_zero(), "--report must be greater than 0s");

	let out = Sink::open(args.output.as_deref())?;
	let publish = moq_tokio::origin::spawn();
	let consume = moq_tokio::origin::spawn();
	let mut tasks = tokio::task::JoinSet::new();

	// Hold every route for the run so each broadcast stays announced, and a handle on each
	// track so a stop finishes it rather than dropping it unfinished, which reads as a failure.
	let mut routes = Vec::new();
	let mut tracks = Vec::new();
	for path in &args.publish {
		let route = publish.route(path.as_str(), moq_net::origin::RouteOptions::hidden())?;
		let track = route
			.broadcast()
			.create_track(
				TRACK,
				track::Info::default()
					.with_timescale(moq_net::Timescale::MILLI)
					.with_max_age(None),
			)
			.context("the replay track is already published")?;
		route.announce()?;
		routes.push(route);
		tracks.push(track.clone());
		tasks.spawn(produce(track, interval));
	}

	let readers: Vec<_> = args
		.follow
		.iter()
		.map(|_| Arc::new(Mutex::new(Reader::default())))
		.collect();
	for (path, reader) in args.follow.iter().zip(&readers) {
		let following = consume.consume().follow(path.as_str(), None)?;
		tasks.spawn(follow(
			following,
			path.clone(),
			reader.clone(),
			out.clone(),
			args.gap.into(),
		));
	}
	if !readers.is_empty() {
		let paths = args.follow.clone();
		let readers = readers.clone();
		let out = out.clone();
		tasks.spawn(async move {
			let mut ticker = kio::clock::interval(report);
			loop {
				ticker.tick().await;
				let paths = paths
					.iter()
					.zip(&readers)
					.map(|(path, reader)| (path.as_str(), reader.lock().unwrap().clone()))
					.collect();
				out.write(&Line::Status { paths })?;
			}
		});
	}

	let client = args
		.client
		.clone()
		.init(args.quic.clone())?
		.with_publisher(&publish)
		.with_subscriber(consume.clone());
	let reconnect = client.connect(url);

	let result = loop {
		tokio::select! {
			// We hold the handle, so the loop only stops by giving up.
			closed = reconnect.closed() => break Err(match closed {
				Err(err) => anyhow::Error::from(err).context("the connection gave up"),
				Ok(()) => anyhow::anyhow!("the connection stopped"),
			}),
			Some(res) = tasks.join_next() => match res {
				Ok(Ok(())) => {}
				Ok(Err(err)) => break Err(err),
				Err(err) => break Err(err.into()),
			},
			_ = tokio::signal::ctrl_c() => break Ok(()),
		}
	};

	// A stop is a publisher leaving on purpose: finish every track before the producers go.
	// After a failure, abort each with its cause rather than drop it unfinished.
	for track in tracks.drain(..) {
		// Fails only once the track already ended, which is all either wants.
		let _ = match &result {
			Ok(()) => track.finish().map(drop),
			Err(err) => track.abort(moq_net::Error::Transport(format!("{err:#}"))),
		};
	}
	// End the session with a close rather than leaving the relay to wait out its idle
	// timeout, so a stopped publisher is a source ending now.
	tasks.shutdown().await;
	drop(routes);
	drop(reconnect);
	client.close().await;
	result
}

/// Write one single-frame group per `interval`, its frame the group's number.
async fn produce(track: track::Producer, interval: Duration) -> anyhow::Result<()> {
	let mut ticker = kio::clock::interval(interval);
	ticker.set_missed_tick_behavior(kio::clock::MissedTickBehavior::Delay);
	let mut previous: Option<u64> = None;
	for sequence in 0u64.. {
		ticker.tick().await;
		// Group starts strictly increase, so one in the same millisecond starts a millisecond later.
		let start = previous.map_or(millis(), |previous| millis().max(previous + 1));
		previous = Some(start);
		let start = moq_net::Timestamp::from_millis(start)?;
		let mut group = track.append_group(start)?;
		group.write_frame(start, Bytes::copy_from_slice(&sequence.to_be_bytes()))?;
		group.finish()?;
	}
	Ok(())
}

/// Read every instance that serves `path`, recording each arrival and reporting gaps.
async fn follow(
	mut following: moq_net::origin::Following,
	path: String,
	reader: Arc<Mutex<Reader>>,
	out: Sink,
	gap: Duration,
) -> anyhow::Result<()> {
	let gap = u64::try_from(gap.as_millis()).unwrap_or(u64::MAX);
	let mut serving: Option<broadcast::Consumer> = None;
	// The newest group read from each instance, so a resubscribe to one counts what it skipped.
	let mut newest: Option<(moq_net::Instance, u64)> = None;
	loop {
		let change = match &serving {
			None => next(&mut following, &path, &out).await?,
			Some(broadcast) => {
				let instance = broadcast.info().instance.clone();
				let mut seen = newest
					.as_ref()
					.filter(|(was, _)| Some(was) == instance.as_ref())
					.map(|(_, group)| *group);
				let result = tokio::select! {
					change = following.available() => change.map_err(anyhow::Error::from),
					err = read(broadcast, &path, &reader, &out, gap, &mut seen) => Err(err),
				};
				if let (Some(instance), Some(group)) = (instance, seen) {
					newest = Some((instance, group));
				}
				match result {
					Ok(change) => change,
					Err(err) => {
						out.write(&Line::Ended {
							path: &path,
							error: err.to_string(),
						})?;
						next(&mut following, &path, &out).await?
					}
				}
			}
		};
		let online = change.is_some();
		serving = change;
		reader.lock().unwrap().online = online;
		let line = match online {
			true => Line::Online { path: &path },
			false => Line::Offline { path: &path },
		};
		out.write(&line)?;
	}
}

/// The next change to what serves `path`. A refusal ends only this path's current read, as a
/// viewer would take it: the next call waits for the serving route to change.
async fn next(
	following: &mut moq_net::origin::Following,
	path: &str,
	out: &Sink,
) -> anyhow::Result<Option<broadcast::Consumer>> {
	loop {
		match following.available().await {
			Ok(change) => return Ok(change),
			Err(err) => out.write(&Line::Ended {
				path,
				error: err.to_string(),
			})?,
		}
	}
}

/// Read groups from one instance until its track ends, returning why it ended. `newest` is the
/// newest group read from this instance, kept across resubscribes to it.
async fn read(
	broadcast: &broadcast::Consumer,
	path: &str,
	reader: &Mutex<Reader>,
	out: &Sink,
	gap: u64,
	newest: &mut Option<u64>,
) -> anyhow::Error {
	let result = async {
		let mut track = broadcast
			.track(TRACK)?
			.subscribe(track::Subscription::default())
			.await?;
		// A viewer reads on past a group it cannot finish: one the publisher gave up, reset by a
		// session that closed mid-group, or still open when a newer group arrives. The next group
		// read counts it missed. Only the track ending ends the read.
		let mut reading: Option<moq_net::group::Consumer> = None;
		loop {
			tokio::select! {
				biased;
				next = track.recv_group() => match next? {
					Some(group) if reading.as_ref().is_none_or(|current| group.start > current.start) => {
						reading = Some(group);
					}
					Some(_) => {}
					None => break,
				},
				frame = async { reading.as_mut().expect("guarded").read_frame().await }, if reading.is_some() => {
					reading = None;
					let Ok(Some(frame)) = frame else {
						continue;
					};
					let sequence = <[u8; 8]>::try_from(frame.payload.as_ref())
						.map(u64::from_be_bytes)
						.map_err(|_| anyhow::anyhow!("a replay frame carries an 8-byte group number"))?;
					let now = millis_wall();
					// Each instance numbers its groups from its own start, so `missed` compares within one,
					// and only a group past the newest shows what the publisher wrote in between.
					let missed = newest
						.filter(|&newest| sequence > newest)
						.map(|newest| sequence - newest - 1);
					let last = {
						let mut reader = reader.lock().unwrap();
						reader.groups += 1;
						reader.last.replace(now)
					};
					if let Some(from) = last
						&& (now.saturating_sub(from) > gap || missed.is_some_and(|missed| missed > 0))
					{
						out.write(&Line::Gap { path, from, missed })?;
					}
					*newest = (*newest).max(Some(sequence));
				}
			}
		}
		anyhow::Ok(anyhow::anyhow!("the track finished"))
	};
	result.await.unwrap_or_else(|err| err)
}

/// What one followed path has seen so far.
#[derive(Clone, Default, Serialize)]
struct Reader {
	/// Whether an instance serves the path now.
	online: bool,
	/// Groups read, over every instance.
	groups: u64,
	/// When the newest group arrived, in Unix milliseconds.
	last: Option<u64>,
}

#[derive(Serialize)]
#[serde(tag = "event", rename_all = "lowercase")]
enum Line<'a> {
	Online {
		path: &'a str,
	},
	Offline {
		path: &'a str,
	},
	Ended {
		path: &'a str,
		error: String,
	},
	Gap {
		path: &'a str,
		from: u64,
		missed: Option<u64>,
	},
	Status {
		paths: BTreeMap<&'a str, Reader>,
	},
}

/// The JSON lines output, shared by every task, each line stamped and flushed whole.
#[derive(Clone)]
struct Sink(Arc<Mutex<Box<dyn Write + Send>>>);

impl Sink {
	fn open(path: Option<&std::path::Path>) -> anyhow::Result<Self> {
		let out: Box<dyn Write + Send> = match path {
			Some(path) => Box::new(std::fs::File::create(path)?),
			None => Box::new(std::io::stdout()),
		};
		Ok(Self(Arc::new(Mutex::new(out))))
	}

	fn write(&self, line: &Line) -> anyhow::Result<()> {
		#[derive(Serialize)]
		struct Stamped<'a, 'b> {
			ms: u64,
			#[serde(flatten)]
			line: &'a Line<'b>,
		}
		let mut bytes = serde_json::to_vec(&Stamped {
			ms: millis_wall(),
			line,
		})?;
		bytes.push(b'\n');
		let mut out = self.0.lock().unwrap();
		out.write_all(&bytes)?;
		out.flush()?;
		Ok(())
	}
}

/// Milliseconds on this process's monotonic clock, what group starts are stamped with.
fn millis() -> u64 {
	let elapsed = kio::clock::now().saturating_duration_since(kio::clock::origin().0);
	u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}

/// Unix milliseconds, so a harness lines readers up with the faults it applied.
fn millis_wall() -> u64 {
	let since = kio::clock::wall().saturating_duration_since(Wall::UNIX_EPOCH);
	u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
}
