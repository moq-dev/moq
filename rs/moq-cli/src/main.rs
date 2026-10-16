//! moq-cli: a media router that wires endpoints onto a shared MoQ Origin.
//!
//! The binary is `moq`. See [`args`] for the `import`/`export`/`play` command
//! grammar; this module orchestrates the shared Origin and spawns the MoQ side
//! plus every stage's endpoint.

mod announced;
mod archive;
mod args;
mod auth;
mod complete;
#[cfg(feature = "capture")]
mod devices;
mod duration;
mod fetch;
mod hls;
mod moq;
mod play;
mod publish;
mod report;
mod rtc;
mod rtmp;
mod srt;
mod subscribe;
#[cfg(test)]
mod test_env;
#[cfg(feature = "transcode")]
mod transcode;
mod web;

use args::{Command, Export, ExportSink, Import, ImportSource, Invocation, MoqSide, TsImport, TsProgram};
use hang::moq_net;
use publish::Publish;
use subscribe::{Subscribe, SubscribeArgs};

use anyhow::Context;
use tokio::task::JoinSet;

#[cfg(feature = "jemalloc")]
#[global_allocator]
static ALLOC: moq_tokio::jemalloc::tikv_jemallocator::Jemalloc = moq_tokio::jemalloc::tikv_jemallocator::Jemalloc;

/// Everything needed to build MoQ clients/servers, encapsulating the optional
/// iroh endpoint so the rest of the code is feature-agnostic.
#[derive(Clone)]
struct Net {
	/// The shared QUIC tuning, handed to whichever roles this process builds.
	quic: moq_tokio::quic::Config,
	#[cfg(feature = "iroh")]
	iroh: Option<moq_tokio::iroh::Endpoint>,
}

impl Net {
	fn client(&self, config: moq_tokio::connect::Config) -> anyhow::Result<moq_tokio::Client> {
		let client = config.init(self.quic.clone())?;
		#[cfg(feature = "iroh")]
		let client = match self.iroh.clone() {
			Some(iroh) => client.with_iroh(iroh),
			None => client,
		};
		Ok(client)
	}

	fn server(
		&self,
		config: moq_tokio::listen::Config,
		activity: moq_net::server::Activity,
	) -> anyhow::Result<moq_tokio::Server> {
		let mut server = moq_tokio::server::Config::default();
		server.listen = config;
		server.activity = activity;
		server.quic = self.quic.clone();
		#[cfg(feature = "iroh")]
		{
			server.iroh = self.iroh.clone();
		}
		Ok(server.init()?)
	}
}

/// Bind the MoQ listener and spawn everything that serves on it: ordinary
/// clients, the LAN mesh when `--cluster-lan` is on, and the certificate
/// endpoint for an explicit `--listen`. A no-op with no listener configured.
async fn spawn_server(
	tasks: &mut JoinSet<anyhow::Result<()>>,
	moq: &MoqSide,
	cluster: &moq_relay::cluster::Cluster,
	net: &Net,
	directions: Directions,
) -> anyhow::Result<(moq_relay::cluster::Started, Option<Shutdown>)> {
	if !moq.serves() {
		let started = cluster.clone().start().await.context("cluster failed to start")?;
		return Ok((started, None));
	}

	let window = moq
		.drain_timeout
		.map(crate::duration::Duration::into_std)
		.unwrap_or(moq_relay::DEFAULT_DRAIN_TIMEOUT);
	let (trigger, shutdown) = moq_relay::shutdown::Observer::new(window);
	let server = net.server(moq.server_config(), shutdown.activity().clone())?;
	let certificates = server.certificates();
	let cluster = attach_lan(cluster.clone(), moq, &server)?;
	// The auth server or public grant a `--listen` endpoint admits through. A
	// LAN-mesh listener with neither admits its peers alone; any other config
	// `init` refuses stops startup.
	let node = moq.cluster.node.clone().unwrap_or_default();
	let auth = match moq.auth.is_empty() && !moq.server.has_explicit_bind() {
		true => moq_relay::auth::Auth::refuse(node),
		false => moq.auth.init(node, &moq.client.tls, moq.client_ca())?,
	};
	// Advertise before accepting, so a `/.cluster` dial is verified against a
	// live credential rather than refused as "LAN discovery is not enabled".
	let started = cluster.clone().start().await.context("cluster failed to start")?;

	// Stream sockets bind asynchronously in `listen`, so this must finish before
	// `spawn_moq` reports readiness. The serve task only owns an already-bound
	// listener and cannot discover a late bind failure.
	let listener = server.listen().await.context("failed to bind listeners")?;
	spawn_serve(
		tasks,
		listener,
		Serving {
			cluster,
			auth,
			directions,
			shutdown: shutdown.clone(),
			lan: moq.lan(),
			public_quic: moq.server.bind.is_some(),
		},
	);

	// The certificate endpoint is for clients dialing a URL, so it follows the
	// explicit listener rather than the mesh's ephemeral one.
	if let Some(web_bind) = moq.server.bind.clone() {
		tasks.spawn(async move { web::run_web(web_bind, certificates).await });
	}

	Ok((
		started,
		Some(Shutdown {
			trigger,
			observer: shutdown,
		}),
	))
}

/// Advertise the bound listener on the LAN when `--cluster-lan` is on.
fn attach_lan(
	cluster: moq_relay::cluster::Cluster,
	moq: &MoqSide,
	server: &moq_tokio::Server,
) -> anyhow::Result<moq_relay::cluster::Cluster> {
	if !moq.lan() {
		return Ok(cluster);
	}
	let port = server
		.local_addr()
		.context("--cluster-lan needs a QUIC listener")?
		.port();
	let mut advertise = moq_relay::cluster::LanAdvertise::new(port);
	if !moq.server_config().tls.generate.is_empty()
		&& let Some(fingerprint) = server.certificates().fingerprints().into_iter().next()
	{
		advertise = advertise.with_fingerprint(fingerprint);
	}
	Ok(cluster.with_advertise(advertise))
}

/// The admission, accounting and shutdown context shared by one listener.
#[derive(Clone)]
struct Serving {
	cluster: moq_relay::cluster::Cluster,
	auth: moq_relay::auth::Auth,
	directions: Directions,
	shutdown: moq_relay::shutdown::Observer,
	lan: bool,
	public_quic: bool,
}

/// Admit one ordinary client through its lease and serve it until it closes.
///
/// The grant scopes the process's origin to what the session may see, then the
/// stage's directions prune the side it does not use, so a subscribe-only export
/// never announces what a viewer could not have had anyway.
impl Serving {
	async fn serve_client(&self, request: moq_tokio::server::Request) -> anyhow::Result<()> {
		let origin = &self.cluster.origin;
		let directions = self.directions;
		let auth_request = moq_relay::auth::request_for(&self.auth, &request);
		let lease = match self.auth.admit(auth_request).await {
			Ok(lease) => lease,
			Err(err) => {
				let status = axum::http::StatusCode::from(&err);
				let reject = match status {
					axum::http::StatusCode::UNAUTHORIZED => moq_tokio::server::Reject::Unauthorized,
					axum::http::StatusCode::FORBIDDEN => moq_tokio::server::Reject::Forbidden,
					status => moq_tokio::server::Reject::App(status.as_u16()),
				};
				request.reject(reject).await.ok();
				return Err(anyhow::Error::new(err).context("session refused"));
			}
		};

		// What the grant allows, as origin handles rooted where the session dialed.
		let token = lease.token();
		let publish = directions
			.publish
			.then(|| origin.scope(&token.root, &token.subscribe).ok())
			.flatten();
		let subscribe = directions
			.consume
			.then(|| origin.scope(&token.root, &token.publish).ok())
			.flatten();
		if publish.is_none() && subscribe.is_none() {
			request.reject(moq_tokio::server::Reject::Forbidden).await.ok();
			anyhow::bail!("grant allows nothing this endpoint serves at {}", token.root);
		}

		let stats = self.cluster.stats.tier(token.tier.clone()).session(&token.root);
		let lease = lease.with_stats(stats.clone());
		let mut request = request.with_stats(stats);
		if let Some(publish) = publish {
			request = request.with_publisher(publish.consume());
		}
		if let Some(subscribe) = subscribe {
			request = request.with_subscriber(subscribe);
		}
		let (session, driver) = request.ok().await?;
		let result = moq_relay::supervise(session, lease, self.shutdown.clone(), None).await;
		driver.await??;
		result
	}
}

/// Whether ordinary clients may use this transport on the shared LAN server.
fn is_public_transport(transport: moq_tokio::Transport, public_quic: bool) -> bool {
	match transport {
		moq_tokio::Transport::Tcp | moq_tokio::Transport::Unix => true,
		_ => public_quic,
	}
}

/// Serve ordinary clients from an already-bound listener, each through its lease.
fn spawn_serve(tasks: &mut JoinSet<anyhow::Result<()>>, mut listener: moq_tokio::Listener, serving: Serving) {
	if let Ok(addr) = listener.local_addr() {
		tracing::info!(%addr, "listening");
	}
	tasks.spawn(async move {
		let mut sessions = JoinSet::new();
		while let Some(request) = listener.accept().await {
			while sessions.try_join_next().is_some() {}
			let serving = serving.clone();
			sessions.spawn(async move {
				let result = if serving.lan && moq_relay::cluster::Cluster::is_lan_path(request.path()) {
					// Logs its own close.
					moq_relay::Connection::new(request, serving.cluster.clone(), serving.auth.clone())
						.with_id(serving.cluster.next_connection_id())
						.with_shutdown(serving.shutdown.clone())
						.run()
						.await;
					return;
				} else if serving.lan && !is_public_transport(request.transport(), serving.public_quic) {
					tracing::debug!(path = %request.path(), "refusing a non-peer request on the LAN mesh listener");
					request.reject(moq_tokio::server::Reject::App(404)).await.ok();
					return;
				} else {
					match moq_tokio::panic::isolate(serving.serve_client(request)).await {
						Some(result) => result,
						None => return,
					}
				};
				if let Err(err) = result {
					tracing::warn!(%err, "session ended with error");
				}
			});
		}
		anyhow::bail!("the MoQ listener stopped accepting")
	});
}

/// The listener's shared drain window, owned by the process driver.
struct Shutdown {
	trigger: moq_relay::shutdown::Trigger,
	observer: moq_relay::shutdown::Observer,
}

/// Keep all stages and listener tasks alive while admitted sessions drain.
async fn drain(shutdown: Shutdown, signals: &mut Signals) {
	let deadline = tokio::time::Instant::now() + shutdown.observer.drain_timeout;
	shutdown.trigger.start();
	tracing::info!(window = ?shutdown.observer.drain_timeout, "shutdown requested; draining sessions (signal again to exit immediately)");
	tokio::select! {
		() = signals.recv() => {},
		() = shutdown.observer.activity().close() => {},
		() = tokio::time::sleep_until(deadline) => {},
	}
}

fn main() -> anyhow::Result<()> {
	let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
	let result = runtime.block_on(run());
	// Tokio's stdin read cannot be cancelled. Async cleanup has finished, so
	// runtime teardown must not wait for a pipe its producer still holds open.
	runtime.shutdown_background();
	result
}

async fn run() -> anyhow::Result<()> {
	moq_tokio::crypto::install_default().expect("failed to install default crypto provider");

	let mut cli = Invocation::parse().await;
	cli.log.init()?;
	cli.validate()?;

	// The local verbs never touch the network, so answer them before binding any
	// transport. `validate` has already refused to pair them with another stage, so
	// the single stage here is the whole invocation.
	// Taken rather than moved out: the local verbs below still ask `cli` whether the
	// command line named a MoQ side.
	let mut stages = std::mem::take(&mut cli.stages);
	if stages.len() == 1 {
		match stages.remove(0) {
			Command::Auth(auth) => {
				cli.reject("auth")?;
				return auth.run().await;
			}
			Command::Completion(completion) => {
				cli.reject("completion")?;
				return completion.run();
			}
			#[cfg(feature = "capture")]
			Command::Devices => {
				cli.reject("devices")?;
				return devices::run().await;
			}
			// Put it back: it needs the transport bound below.
			other => stages.push(other),
		}
	}

	// `fetch` and `announced` only dial, so an ambient listener or cluster setting they
	// never use is not validated either.
	if let [Command::Fetch(_)] = stages.as_slice() {
		cli.dial_only("fetch", &["--broadcast"])?;
	} else if let [Command::Announced(_)] = stages.as_slice() {
		cli.dial_only("announced", &[])?;
	} else {
		cli.unserved()?;
		cli.moq.validate()?;
	}

	let net = Net {
		quic: cli.moq.quic.clone(),
		#[cfg(feature = "iroh")]
		iroh: cli.moq.iroh.clone().bind(&cli.moq.quic).await?,
	};

	#[cfg(feature = "jemalloc")]
	let jemalloc = moq_tokio::jemalloc::run();
	#[cfg(not(feature = "jemalloc"))]
	let jemalloc = std::future::pending::<anyhow::Result<()>>();

	let run = async move {
		// The verbs that own the process were refused alongside another stage, so a
		// lone one of those runs by itself; everything else is a list of stages.
		if stages.len() == 1 && !stages[0].is_stageable() {
			match stages.remove(0) {
				Command::Fetch(args) => return fetch::run(cli.moq, args, net).await,
				Command::Announced(args) => return announced::run(cli.moq, args, net).await,
				#[cfg(feature = "play")]
				Command::Play(args) => return run_play(cli.moq, args, net).await,
				#[cfg(feature = "transcode")]
				Command::Transcode(args) => return transcode::run(cli.moq, args, net).await,
				_ => unreachable!("the local verbs returned before the transport was bound"),
			}
		}

		run_stages(cli.moq, stages, net).await
	};

	tokio::select! {
		result = run => result,
		Err(err) = jemalloc => Err(err).context("jemalloc profiler failed"),
	}
}

/// Which directions the stages need on the shared MoQ attachment.
#[derive(Clone, Copy, Default)]
pub struct Directions {
	/// Any `import`, or an `export --echo`: the Origin is published outward.
	pub publish: bool,
	/// Any `export` or `play`: the Origin is filled from the network.
	pub consume: bool,
}

impl Directions {
	/// The union of what the stages need, so one attachment serves them all.
	fn of(stages: &[Command]) -> Self {
		let publishes = |stage: &Command| match stage {
			Command::Import(_) => true,
			Command::Export(export) => export.echo.is_some(),
			_ => false,
		};
		Self {
			publish: stages.iter().any(publishes),
			consume: stages.iter().any(|stage| matches!(stage, Command::Export(_))),
		}
	}
}

/// Attach the shared Origin to the MoQ network: dial a relay, accept inbound
/// sessions, mesh with the LAN, or any combination.
///
/// An invocation that both imports and exports attaches both directions to the
/// same session rather than opening two, which is how a relay peers with another
/// relay. Loops are the network's problem, not ours: an announcement carries the
/// hops it crossed, and our own Hop ID is one of them, so a broadcast we
/// publish is never announced back to us.
///
/// Returns the dialed [`Connection`](moq_tokio::Connection), if any, for a graceful
/// close, and an allocator over the uplink's bandwidth estimate, for the sources that
/// share it. Capture encoders follow their slice; passthrough imports reserve
/// their peak-hold bitrate so the encoder sees what is left. Only an outbound
/// client has an estimate: a `--listen` publisher's sessions are inbound and
/// never surfaced here, so it gets an
/// [`unlimited`](moq_net::bandwidth::Allocator::unlimited) allocator and those
/// sources encode at their configured rate.
///
/// One allocator per connection, minted here rather than per stage, since dividing
/// the estimate is only meaningful across everything sharing it. A stage that built
/// its own would split its own tracks correctly and still oversubscribe every other
/// stage on the same connection, which is the whole problem.
async fn spawn_moq(
	moq: &MoqSide,
	net: &Net,
	client: moq_tokio::Client,
	cluster: moq_relay::cluster::Cluster,
	directions: Directions,
	tasks: &mut JoinSet<anyhow::Result<()>>,
) -> anyhow::Result<Attached> {
	let mut bandwidth = moq_net::bandwidth::Allocator::unlimited();
	let mut connection = None;
	let cluster = cluster
		.with_client(client.clone())
		.with_client_tls(moq.client.tls.build()?)
		.with_connect(moq.client.clone(), moq.quic.clone());
	let origin = cluster.origin.clone();

	if let Some(url) = moq.client.url.clone() {
		let mut client = client;
		if directions.publish {
			client = client.with_publisher(origin.consume());
		}
		if directions.consume {
			client = client.with_subscriber(origin.clone());
		}

		// Both directions ride one connection, so this dials directly rather than
		// through `Client::publish` / `Client::consume`, which each attach a single
		// direction and dial the same URL.
		let reconnect = client.connect(url);
		// Read before the handle moves into the task. This consumer is persistent: it
		// survives reconnects, reading `None` while down, so it can be wired up before
		// anything connects.
		bandwidth = moq_net::bandwidth::Allocator::new(reconnect.send_bandwidth());
		let closed = reconnect.clone();
		tasks.spawn(async move { Ok(closed.closed().await?) });
		connection = Some(reconnect);
	}

	let (started, shutdown) =
		notify_when_initialized(spawn_server(tasks, moq, &cluster, net, directions), moq::notify_ready).await?;
	// Even standalone: it is what reports a panic in the origin driver.
	tasks.spawn(async move { started.run().await });

	Ok(Attached {
		bandwidth,
		origin,
		connection,
		shutdown,
	})
}

/// What [`spawn_moq`] attached to the MoQ network.
struct Attached {
	bandwidth: moq_net::bandwidth::Allocator,
	origin: moq_net::origin::Producer,
	/// The relay connection, when `--connect` dialed one.
	connection: Option<moq_tokio::Connection>,
	shutdown: Option<Shutdown>,
}

/// Report readiness only after every configured MoQ attachment initializes.
async fn notify_when_initialized<T>(
	initialization: impl std::future::Future<Output = anyhow::Result<T>>,
	notify_ready: impl FnOnce(),
) -> anyhow::Result<T> {
	let value = initialization.await?;
	notify_ready();
	Ok(value)
}

/// Fill the shared Origin from MoQ, then play one broadcast locally.
///
/// The playback event loop runs on this task's thread rather than a spawned one:
/// winit can only build an event loop on the process main thread, which is where
/// the runtime polls this future.
#[cfg(feature = "play")]
async fn run_play(moq: MoqSide, args: play::Args, net: Net) -> anyhow::Result<()> {
	// Before anything dials: a codec we can't decode is a blank window otherwise.
	args.validate()?;

	let cluster = moq.cluster()?;
	let name = moq.broadcast.clone().unwrap_or_default();
	let mut tasks: JoinSet<anyhow::Result<()>> = JoinSet::new();

	let directions = Directions {
		consume: true,
		..Default::default()
	};
	let client = net.client(moq.client.clone())?;
	let Attached { origin, .. } = spawn_moq(&moq, &net, client, cluster, directions, &mut tasks).await?;

	play::run(origin.consume(), name, args, tasks).await
}

/// Run every stage over one Origin and one MoQ attachment.
///
/// Stages are independent: each names its own broadcast and owns its own endpoint,
/// and the first endpoint to finish ends the process. A shutdown signal keeps
/// accepted sessions serving through the configured drain window.
async fn run_stages(moq: MoqSide, stages: Vec<Command>, net: Net) -> anyhow::Result<()> {
	let cluster = moq.cluster()?;
	let mut tasks: JoinSet<anyhow::Result<()>> = JoinSet::new();
	// The stdin/capture pipelines run on this thread instead of the JoinSet: the
	// platform capture stream is not Send, so their futures cannot be spawned.
	let mut locals: Vec<Publish> = Vec::new();

	// Before anything listens: a client can count its session established as soon
	// as the listener is up, and a signal then must drain it, not kill the process.
	let signals = Signals::new()?;

	// The stage combinations were refused up front by `Invocation::validate`, before
	// anything bound a port or dialed out.
	let client = net.client(moq.client.clone())?;
	let mut connection = None;
	let result = async {
		let Attached {
			bandwidth,
			origin,
			connection: attached,
			shutdown,
		} = spawn_moq(&moq, &net, client.clone(), cluster, Directions::of(&stages), &mut tasks).await?;
		connection = attached;

		// stdin and stdout are one resource each, so two stages can't share them.
		let mut stdin = None;
		let mut stdout = None;

		// One publisher instance per run, unless redundant publishers named a shared one.
		let epoch = moq.epoch.clone().unwrap_or_else(moq_net::Epoch::mint);
		tracing::info!(%epoch, "publisher instance");

		for stage in stages {
			let name = stage.broadcast(&moq);
			match stage {
				Command::Import(import) => {
					if import.source.stdin_format().is_some() {
						claim("stdin", &mut stdin, &name)?;
					}
					let monitor = connection.as_ref().map(moq_tokio::Connection::monitor);
					if let Some(publish) = spawn_import(
						&origin,
						import,
						name,
						epoch.clone(),
						bandwidth.clone(),
						monitor,
						&mut tasks,
					)? {
						locals.push(publish);
					}
				}
				Command::Export(export) => {
					if export.sink.is_stdout() {
						claim("stdout", &mut stdout, &name)?;
					}
					let monitor = connection.as_ref().map(moq_tokio::Connection::monitor);
					spawn_export(&origin, export, name, monitor, &mut tasks)?;
				}
				other => unreachable!("`{}` is not a stage", other.name()),
			}
		}

		if locals.is_empty() {
			drive(tasks, signals, shutdown).await
		} else {
			let local = tokio::task::LocalSet::new();
			supervise(&local, locals.into_iter().map(Publish::run), &mut tasks);
			local.run_until(drive(tasks, signals, shutdown)).await
		}
	}
	.await;

	// The process exits next, even on a setup error, so the relay only hears we left
	// if the close goes out now. The connection first delivers what it queued, such
	// as the finished tracks at stdin EOF, since the client's close discards it.
	if let Some(connection) = connection
		&& let Err(err) = connection.close().await
	{
		tracing::warn!(%err, "closed before delivering everything");
	}
	client.close().await;
	result
}

/// Run the non-Send pipelines on `local`, reporting each into `tasks`.
///
/// The report is what makes a local pipeline end the process on the same terms as a
/// spawned stage: it returns on stdin EOF, and a panic surfaces as an error instead
/// of leaving the other stages running without it.
fn supervise<F>(
	local: &tokio::task::LocalSet,
	pipelines: impl IntoIterator<Item = F>,
	tasks: &mut JoinSet<anyhow::Result<()>>,
) where
	F: std::future::Future<Output = anyhow::Result<()>> + 'static,
{
	for pipeline in pipelines {
		let pipeline = local.spawn_local(pipeline);
		tasks.spawn(async move { pipeline.await.context("pipeline panicked")? });
	}
}

/// Refuse a second stage on a stream there is only one of.
fn claim(stream: &str, held: &mut Option<String>, name: &str) -> anyhow::Result<()> {
	if let Some(first) = held {
		anyhow::bail!(
			"only one stage can use {stream}, but both `{}` and `{}` do",
			display_name(first),
			display_name(name),
		);
	}

	*held = Some(name.to_string());
	Ok(())
}

/// The broadcast name for an error message; the root broadcast has none.
fn display_name(name: &str) -> &str {
	if name.is_empty() { "<root>" } else { name }
}

/// Route one stage's source INTO the shared Origin, exposing it to the MoQ network.
///
/// The sources announced once per run take `epoch`; the ingest gateways and
/// `ts --program all` mint their own per connection or program.
///
/// Returns the pipeline that has to run on the caller's thread, for the sources
/// that have one (the stdin containers and capture). `connection` supplies a
/// `--stats` report's transport measurements.
fn spawn_import(
	origin: &moq_net::origin::Producer,
	import: Import,
	name: String,
	epoch: moq_net::Epoch,
	bandwidth: moq_net::bandwidth::Allocator,
	connection: Option<moq_tokio::connection::Monitor>,
	tasks: &mut JoinSet<anyhow::Result<()>>,
) -> anyhow::Result<Option<Publish>> {
	if let ImportSource::Rtc(rtc) = &import.source
		&& rtc.connect.is_some()
	{
		reject_listener_cors(&rtc.cors, "import rtc")?;
	}

	let max_age = import.max_age.map(crate::duration::Duration::into_std);
	// The MoQ side every gateway publishes into, minted per source since each takes it
	// by value onto its own task.
	let target = |name: String| crate::moq::ImportTarget {
		origin: origin.clone(),
		name,
		max_age,
		bandwidth: bandwidth.clone(),
	};

	let mut local = None;

	if let Some(format) = import.source.stdin_format() {
		warn_if_missing_format(&name);
		let config = moq_mux::catalog::Config::default()
			.with_max_age(max_age)
			.with_bandwidth(bandwidth.clone());
		let publish = if let ImportSource::Ts(TsImport {
			program: Some(TsProgram::All),
			..
		}) = &import.source
		{
			let name = require_broadcast(name, "import ts --program all")?;
			Publish::ts_programs(origin.clone(), name, config)
		} else {
			let broadcast = origin.create_broadcast(&name).context("failed to create broadcast")?;
			Publish::new(broadcast, &format, config)?
		};
		let publish = if import.stats {
			publish.with_stats(connection)?
		} else {
			publish
		};
		// Media stays servable at EOF until it goes stale, the same max-age relays retain it for.
		let mut publish = publish.linger(max_age.unwrap_or(hang::container::MAX_AGE));
		publish.announce(epoch)?;
		local = Some(publish);
	} else {
		match import.source {
			ImportSource::Hls(hls) => {
				warn_if_missing_format(&name);
				tasks.spawn(hls::import(target(name), hls.playlist, epoch));
			}
			ImportSource::Rtmp(rtmp) => {
				if let Some(addr) = rtmp.listen {
					let name = require_broadcast(name, "import rtmp --listen")?;
					tasks.spawn(rtmp::listen_import(target(name), addr));
				} else if let Some(url) = rtmp.connect {
					tasks.spawn(rtmp::connect_import(target(name), url));
				}
			}
			ImportSource::Srt(srt) => {
				let program = srt.program();
				let srt = srt.endpoint;
				if let Some(addr) = srt.listen {
					let name = require_broadcast(name, "import srt --listen")?;
					tasks.spawn(srt::listen_import(target(name), addr, srt.latency.into_std(), program));
				} else if let Some(url) = srt.connect {
					tasks.spawn(srt::connect_import(target(name), url, srt.latency.into_std(), program));
				}
			}
			ImportSource::Rtc(rtc) => {
				if let Some(addr) = rtc.listen {
					let name = require_broadcast(name, "import rtc --listen")?;
					tasks.spawn(rtc::listen_import(
						target(name),
						rtc::Listen {
							addr,
							udp_bind: rtc.udp_bind,
							public_addr: rtc.public_addr,
							cors: rtc.cors,
						},
					));
				} else if let Some(url) = rtc.connect {
					tasks.spawn(rtc::connect_import(target(name), url, epoch));
				}
			}
			ImportSource::Archive(args) => {
				// A replay serves the retention the recording was made with.
				anyhow::ensure!(max_age.is_none(), "`--max-age` does not apply to `import archive`");
				tasks.spawn(archive::import(origin.clone(), name, args, epoch));
			}
			#[cfg(feature = "capture")]
			ImportSource::Capture(capture) => {
				warn_if_missing_format(&name);
				let broadcast = origin.create_broadcast(&name).context("failed to create broadcast")?;
				let mut publish = Publish::capture(broadcast, &capture, bandwidth.clone(), max_age)?;
				publish.announce(epoch)?;
				local = Some(publish);
			}
			_ => unreachable!("container formats are handled by stdin_format above"),
		}
	}

	Ok(local)
}

/// Route the shared Origin OUT to one stage's sink, filling it from the MoQ network.
/// `connection` supplies an `--echo` report's transport measurements.
fn spawn_export(
	origin: &moq_net::origin::Producer,
	export: Export,
	name: String,
	connection: Option<moq_tokio::connection::Monitor>,
	tasks: &mut JoinSet<anyhow::Result<()>>,
) -> anyhow::Result<()> {
	if let ExportSink::Rtc(rtc) = &export.sink
		&& rtc.connect.is_some()
	{
		reject_listener_cors(&rtc.cors, "export rtc")?;
	}

	if let Some(stdout) = export.sink.stdout() {
		let args = SubscribeArgs {
			format: stdout.format,
			max_delay: stdout.max_delay,
			linger: stdout.linger,
			stitch: stdout.stitch,
			fragment_duration: stdout.fragment_duration,
			mux_rate: stdout.mux_rate,
			passthrough: stdout.passthrough,
			catalog: export.catalog_format,
			select: export.select,
		};
		let consumer = origin.consume();
		let echo = export.echo.map(|echo| (origin.clone(), echo, connection));
		tasks.spawn(async move { run_stdout(consumer, name, args, echo).await });
	} else {
		match export.sink {
			ExportSink::Hls(args) => {
				let name = require_broadcast(name, "export hls")?;
				tasks.spawn(hls::export(origin.consume(), args, name));
			}
			ExportSink::Rtmp(rtmp) => {
				let max_delay = rtmp.max_delay.into_std();
				if let Some(addr) = rtmp.endpoint.listen {
					let name = require_broadcast(name, "export rtmp --listen")?;
					tasks.spawn(rtmp::listen_export(origin.consume(), addr, name, max_delay));
				} else if let Some(url) = rtmp.endpoint.connect {
					tasks.spawn(rtmp::connect_export(origin.consume(), url, name, max_delay));
				}
			}
			ExportSink::Srt(srt) => {
				if let Some(addr) = srt.endpoint.listen {
					let name = require_broadcast(name, "export srt --listen")?;
					tasks.spawn(srt::listen_export(origin.consume(), addr, name, srt));
				} else if let Some(url) = srt.endpoint.connect.clone() {
					tasks.spawn(srt::connect_export(origin.consume(), url, name, srt));
				}
			}
			ExportSink::Rtc(rtc) => {
				if let Some(addr) = rtc.listen {
					let name = require_broadcast(name, "export rtc --listen")?;
					tasks.spawn(rtc::listen_export(
						origin.consume(),
						name,
						rtc::Listen {
							addr,
							udp_bind: rtc.udp_bind,
							public_addr: rtc.public_addr,
							cors: rtc.cors,
						},
					));
				} else if let Some(url) = rtc.connect {
					tasks.spawn(rtc::connect_export(origin.consume(), url, name));
				}
			}
			ExportSink::Archive(args) => {
				let format = export
					.catalog_format
					.map(Into::into)
					.or_else(|| moq_mux::catalog::CatalogFormat::detect(&name))
					.unwrap_or_default();
				tasks.spawn(archive::export(origin.consume(), name, format, args));
			}
			ExportSink::Stats => {
				let name = require_broadcast(name, "export stats")?;
				tasks.spawn(report::print_stats(origin.consume(), name));
			}
			ExportSink::Echo => {
				tasks.spawn(report::print_echo(origin.consume(), name));
			}
			_ => unreachable!("container formats are handled by stdout_format above"),
		}
	}

	Ok(())
}

/// Where an `export --echo` publishes its feedback, under what name, and over which connection.
type EchoTarget = (
	moq_net::origin::Producer,
	report::EchoName,
	Option<moq_tokio::connection::Monitor>,
);

/// Subscribe to `name` from the Origin and write it to stdout, publishing feedback beside it
/// where the catalog solicits it when `echo` is set.
async fn run_stdout(
	consumer: moq_net::origin::Consumer,
	name: String,
	args: SubscribeArgs,
	echo: Option<EchoTarget>,
) -> anyhow::Result<()> {
	let catalog = args.catalog_format(&name);

	// Confirm the broadcast is reachable and wait for it to be announced; `Subscribe` then
	// resolves it, and any sibling broadcast a rendition's `broadcast` field references
	// (e.g. "./source") through the same origin.
	consumer
		.routed(&name)
		.await
		.ok_or_else(|| anyhow::anyhow!("origin closed before broadcast `{name}` was announced"))?;

	let subscribe = Subscribe::new(consumer, &name, catalog, args).await?;
	let Some((origin, echo, connection)) = echo else {
		return subscribe.run().await;
	};
	let mut echo = report::Echo::new(origin, subscribe.source().clone(), catalog, echo, connection);
	let subscribe = subscribe.with_sections(echo.sections());
	let result = tokio::select! {
		result = subscribe.run() => result,
		never = echo.run() => match never {},
	};
	echo.finish();
	result
}

/// Run until an endpoint ends, keeping all tasks alive while a signalled listener drains.
async fn drive(
	mut tasks: JoinSet<anyhow::Result<()>>,
	mut signals: Signals,
	shutdown: Option<Shutdown>,
) -> anyhow::Result<()> {
	let result = loop {
		tokio::select! {
			biased;
			() = signals.recv() => {
				if let Some(shutdown) = shutdown {
					drain(shutdown, &mut signals).await;
				}
				break Ok(());
			}
			result = tasks.join_next() => match result {
				Some(Ok(result)) => break result,
				Some(Err(err)) if err.is_cancelled() => continue,
				Some(Err(err)) => break Err(err.into()),
				None => break Ok(()),
			}
		}
	};
	tasks.shutdown().await;
	result
}

/// SIGINT or, on unix, SIGTERM (what process supervisors send on stop), caught from
/// construction on. Until a handler is installed the default action kills the
/// process, so build this before anything can accept a session.
struct Signals {
	#[cfg(unix)]
	interrupt: tokio::signal::unix::Signal,
	#[cfg(unix)]
	terminate: tokio::signal::unix::Signal,
	#[cfg(not(unix))]
	ctrl_c: tokio::signal::windows::CtrlC,
}

impl Signals {
	fn new() -> anyhow::Result<Self> {
		#[cfg(unix)]
		{
			use tokio::signal::unix::{SignalKind, signal};
			Ok(Self {
				interrupt: signal(SignalKind::interrupt()).context("failed to listen for SIGINT")?,
				terminate: signal(SignalKind::terminate()).context("failed to listen for SIGTERM")?,
			})
		}
		#[cfg(not(unix))]
		{
			let ctrl_c = tokio::signal::windows::ctrl_c().context("failed to listen for Ctrl-C")?;
			Ok(Self { ctrl_c })
		}
	}

	/// Resolve on the next signal, including one caught before this was polled.
	async fn recv(&mut self) {
		#[cfg(unix)]
		tokio::select! {
			_ = self.interrupt.recv() => {}
			_ = self.terminate.recv() => {}
		}
		#[cfg(not(unix))]
		self.ctrl_c.recv().await;
	}
}

/// The listener / HTTP-serving endpoints bridge one named broadcast, so an
/// empty `--broadcast` is rejected rather than silently defaulting to the root.
fn require_broadcast(name: String, endpoint: &str) -> anyhow::Result<String> {
	anyhow::ensure!(
		!name.is_empty(),
		"`{endpoint}` requires a broadcast: pass --broadcast <name>"
	);
	Ok(name)
}

fn warn_if_missing_format(name: &str) {
	// The empty (root) broadcast has no name to suffix, so there's nothing to warn about.
	if !name.is_empty() && moq_mux::catalog::CatalogFormat::detect(name).is_none() {
		tracing::warn!(
			name,
			"You should append .hang to your broadcast name to make the catalog format explicit."
		);
	}
}

fn reject_listener_cors(cors: &crate::web::Cors, endpoint: &str) -> anyhow::Result<()> {
	anyhow::ensure!(
		cors.origin.is_empty(),
		"`--cors-origin` only applies to `{endpoint} --listen`"
	);
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::cell::Cell;
	use std::future::Future;
	use std::pin::Pin;

	type Pipeline = Pin<Box<dyn Future<Output = anyhow::Result<()>>>>;

	/// Held by every test that builds [`Signals`]: a raised signal reaches each
	/// listener in the process, and `cargo test` runs tests as threads of one.
	static SIGNALS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

	#[tokio::test(start_paused = true)]
	async fn drain_ends_when_the_last_admission_leaves() {
		let _signals = SIGNALS.lock().await;
		let (trigger, mut observer) = moq_relay::shutdown::Observer::new(std::time::Duration::from_secs(10));
		let active = observer.activity().enter().unwrap();
		let mut signals = Signals::new().unwrap();
		let shutdown = Shutdown {
			trigger,
			observer: observer.clone(),
		};
		let draining = tokio::spawn(async move { drain(shutdown, &mut signals).await });
		observer.started().await;
		assert!(!draining.is_finished());
		drop(active);
		draining.await.unwrap();
	}

	/// A signal sent before the driver first polls, such as right after a client
	/// counted its session established, still drains that session. The default
	/// action would otherwise kill the process with no GOAWAY.
	#[cfg(unix)]
	#[tokio::test(start_paused = true)]
	async fn a_signal_before_the_driver_polls_drains() {
		let _signals = SIGNALS.lock().await;
		let (trigger, mut observer) = moq_relay::shutdown::Observer::new(std::time::Duration::from_secs(10));
		let session = observer.activity().enter().unwrap();
		let signals = Signals::new().unwrap();
		// SAFETY: raise has no memory-safety preconditions.
		assert_eq!(unsafe { libc::raise(libc::SIGTERM) }, 0);
		let mut tasks = JoinSet::new();
		tasks.spawn(std::future::pending());
		let shutdown = Shutdown {
			trigger,
			observer: observer.clone(),
		};
		let driving = tokio::spawn(drive(tasks, signals, Some(shutdown)));
		observer.started().await;
		assert!(!driving.is_finished(), "drain ended under an established session");
		drop(session);
		driving.await.unwrap().unwrap();
	}

	#[tokio::test(start_paused = true)]
	async fn drain_bounds_a_pending_admission() {
		let _signals = SIGNALS.lock().await;
		let window = std::time::Duration::from_secs(10);
		let (trigger, mut observer) = moq_relay::shutdown::Observer::new(window);
		let _active = observer.activity().enter().unwrap();
		let mut signals = Signals::new().unwrap();
		let shutdown = Shutdown {
			trigger,
			observer: observer.clone(),
		};
		let draining = tokio::spawn(async move { drain(shutdown, &mut signals).await });
		observer.started().await;
		tokio::time::advance(window / 2).await;
		assert!(!draining.is_finished());
		tokio::time::advance(window / 2).await;
		draining.await.unwrap();
	}

	/// A local pipeline that dies takes the process with it, even while another one is
	/// still running. Reporting completion from inside the task instead would miss
	/// this: a panic skips the report, leaving the survivor to keep the process alive
	/// with one broadcast silently gone.
	#[tokio::test]
	async fn a_panicking_pipeline_ends_the_process() {
		let _signals = SIGNALS.lock().await;
		let local = tokio::task::LocalSet::new();
		let mut tasks = JoinSet::new();

		let pipelines: Vec<Pipeline> = vec![
			Box::pin(async { panic!("pipeline died") }),
			Box::pin(std::future::pending()),
		];
		supervise(&local, pipelines, &mut tasks);

		let err = local
			.run_until(drive(tasks, Signals::new().unwrap(), None))
			.await
			.unwrap_err();
		assert!(err.to_string().contains("pipeline panicked"), "{err}");
	}

	/// The first to finish ends the process, which is how stdin EOF stops a run.
	#[tokio::test]
	async fn a_finished_pipeline_ends_the_process() {
		let _signals = SIGNALS.lock().await;
		let local = tokio::task::LocalSet::new();
		let mut tasks = JoinSet::new();

		let pipelines: Vec<Pipeline> = vec![Box::pin(async { Ok(()) }), Box::pin(std::future::pending())];
		supervise(&local, pipelines, &mut tasks);

		local
			.run_until(drive(tasks, Signals::new().unwrap(), None))
			.await
			.unwrap();
	}

	/// A stream bind failure is part of initialization, so systemd must never see
	/// READY=1 for a process that has no listening socket.
	#[tokio::test]
	async fn a_stream_bind_failure_prevents_readiness() {
		let occupied = std::net::TcpListener::bind("127.0.0.1:0").expect("occupy a port");
		let addr = occupied.local_addr().expect("occupied address").to_string();
		let invocation =
			Invocation::try_parse_from(["moq", "--listen-tcp-bind", &addr, "--auth-public", "**", "import", "ts"])
				.expect("parse stream-only invocation");
		let cluster = invocation.moq.cluster().expect("create cluster");
		let net = Net {
			quic: invocation.moq.quic.clone(),
			#[cfg(feature = "iroh")]
			iroh: None,
		};
		let mut tasks = JoinSet::new();
		let ready = Cell::new(false);

		let err = match notify_when_initialized(
			spawn_server(
				&mut tasks,
				&invocation.moq,
				&cluster,
				&net,
				Directions {
					publish: true,
					consume: false,
				},
			),
			|| ready.set(true),
		)
		.await
		{
			Ok(_) => panic!("the occupied port must fail initialization"),
			Err(err) => err,
		};

		assert!(err.to_string().contains("failed to bind listeners"), "{err:#}");
		assert!(!ready.get(), "readiness must be withheld after a bind failure");
		assert!(tasks.is_empty(), "nothing should be spawned after a bind failure");
	}

	/// An explicit listener with no auth source stops startup instead of refusing
	/// every session, even for a caller that skipped `MoqSide::validate`.
	#[tokio::test]
	async fn a_listener_without_auth_stops_startup() {
		let invocation = Invocation::try_parse_from(["moq", "--listen-tcp-bind", "127.0.0.1:0", "import", "ts"])
			.expect("parse TCP-only invocation");
		let cluster = invocation.moq.cluster().expect("create cluster");
		let net = Net {
			quic: invocation.moq.quic.clone(),
			#[cfg(feature = "iroh")]
			iroh: None,
		};
		let mut tasks = JoinSet::new();

		let err = match spawn_server(
			&mut tasks,
			&invocation.moq,
			&cluster,
			&net,
			Directions {
				publish: true,
				consume: false,
			},
		)
		.await
		{
			Ok(_) => panic!("a listener without auth must not start"),
			Err(err) => err,
		};
		assert!(err.to_string().contains("nobody can authenticate"), "{err:#}");
		assert!(tasks.is_empty(), "nothing should be spawned without auth");
	}

	/// A raw TCP bind is a complete server side even when no QUIC bind is set.
	#[tokio::test]
	async fn tcp_only_moq_side_starts_a_server() {
		let invocation = Invocation::try_parse_from([
			"moq",
			"--listen-tcp-bind",
			"127.0.0.1:0",
			"--auth-public",
			"**",
			"import",
			"ts",
		])
		.expect("parse TCP-only invocation");
		let cluster = invocation.moq.cluster().expect("create cluster");
		let net = Net {
			quic: invocation.moq.quic.clone(),
			#[cfg(feature = "iroh")]
			iroh: None,
		};
		let mut tasks = JoinSet::new();

		if let Err(err) = spawn_server(
			&mut tasks,
			&invocation.moq,
			&cluster,
			&net,
			Directions {
				publish: true,
				consume: false,
			},
		)
		.await
		{
			panic!("start TCP-only server: {err:#}");
		}

		assert!(
			tokio::time::timeout(std::time::Duration::from_millis(50), tasks.join_next())
				.await
				.is_err(),
			"the server task should still be accepting connections"
		);
		tasks.abort_all();
	}

	/// An HTTP `--cluster-connect-api` is a MoQ side, so start-up must attach
	/// client TLS the way the relay does rather than refuse after validate.
	#[tokio::test]
	async fn cluster_connect_api_http_attaches_client_tls() {
		let _ = moq_tokio::crypto::install_default();
		let invocation = Invocation::try_parse_from([
			"moq",
			"--cluster-connect-api",
			"https://api.example/peers",
			"import",
			"ts",
		])
		.expect("parse");
		assert!(invocation.moq.validate().is_ok());
		let net = Net {
			quic: invocation.moq.quic.clone(),
			#[cfg(feature = "iroh")]
			iroh: None,
		};
		let client = net.client(invocation.moq.client.clone()).expect("client");
		let cluster = invocation
			.moq
			.cluster()
			.expect("cluster")
			.with_client(client)
			.with_connect(invocation.moq.client.clone(), invocation.moq.quic.clone());
		let err = cluster
			.clone()
			.start()
			.await
			.expect_err("http API without TLS")
			.to_string();
		assert!(err.contains("client TLS"), "{err}");

		cluster
			.with_client_tls(invocation.moq.client.tls.build().expect("tls"))
			.start()
			.await
			.expect("http API with TLS");
	}

	#[test]
	fn explicit_stream_listeners_are_public_without_exposing_mesh_quic() {
		assert!(is_public_transport(moq_tokio::Transport::Tcp, false));
		assert!(is_public_transport(moq_tokio::Transport::Unix, false));
		assert!(!is_public_transport(moq_tokio::Transport::Quic, false));
		assert!(is_public_transport(moq_tokio::Transport::Quic, true));
	}
}
