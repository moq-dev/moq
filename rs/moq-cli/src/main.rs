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
mod fetch;
mod hls;
mod moq;
mod play;
mod publish;
mod report;
mod rtc;
mod rtmp;
mod rtsp;
mod srt;
mod subscribe;
#[cfg(feature = "transcode")]
mod transcode;
mod web;

use args::{Command, Export, ExportSink, Import, ImportSource, Invocation, MoqSide, TsImport, TsProgram};
use hang::moq_net;
use publish::Publish;
use subscribe::{Subscribe, SubscribeArgs};

use anyhow::Context;
use moq_relay::shutdown::Signals;
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
		.map(Into::into)
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
	// readiness is reported. The serve task only owns an already-bound
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

/// The admission and shutdown context shared by one listener.
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
	async fn serve_client(&self, mut request: moq_tokio::server::Request) -> anyhow::Result<()> {
		let origin = &self.cluster.origin;
		let directions = self.directions;
		let auth_request = moq_relay::auth::request_for(&self.auth, &request);
		// The listener's handshake deadline covers admission; `within` closes a session
		// it gives up on with the timeout code.
		let lease = match request.within(self.auth.admit(auth_request)).await {
			Err(err) => return Err(anyhow::Error::new(err).context("admission timed out")),
			Ok(Ok(lease)) => lease,
			Ok(Err(err)) => {
				request.reject(moq_net::SessionError::from(&err)).await.ok();
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
			request.reject(moq_net::SessionError::Unauthorized).await.ok();
			anyhow::bail!("grant allows nothing this endpoint serves at {}", token.root);
		}

		if let Some(publish) = publish {
			request = request.with_publisher(publish.consume());
		}
		if let Some(subscribe) = subscribe {
			request = request.with_subscriber(subscribe);
		}
		let serving = self.shutdown.serve();
		let session = self
			.shutdown
			.until_deadline(request.ok())
			.await
			.ok_or_else(|| anyhow::anyhow!("the SETUP reply was still pending at the drain deadline"))??;

		moq_relay::supervise(session, lease, serving, None).await
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
					request.reject(moq_net::SessionError::App(404)).await.ok();
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

/// Keep all stages and listener tasks alive while admitted sessions drain, the way
/// the relay does, or until a second signal.
async fn drain(shutdown: Shutdown, signals: &mut Signals) {
	shutdown.trigger.start();
	tracing::info!(window = ?shutdown.observer.drain_timeout, "shutdown requested; draining sessions (signal again to exit immediately)");
	tokio::select! {
		() = signals.recv() => tracing::warn!("second shutdown signal; exiting immediately"),
		() = shutdown.observer.drained() => {},
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
/// Every attachment is up once this returns, so the caller reports readiness then.
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
	cluster: moq_relay::cluster::Cluster,
	directions: Directions,
	tasks: &mut JoinSet<anyhow::Result<()>>,
) -> anyhow::Result<Attached> {
	let cluster = cluster.with_connect(moq.client.clone(), moq.quic.clone());
	#[cfg(feature = "iroh")]
	let cluster = match net.iroh.clone() {
		Some(iroh) => cluster.with_iroh(iroh),
		None => cluster,
	};
	let origin = cluster.origin.clone();

	// Built only to dial `--connect`: the cluster binds its own client from the
	// same `[connect]` config, so a second one would contend for a fixed bind port.
	// Built before the listener so a bad config fails before readiness, but dialed
	// only after it: a later startup failure would drop a live dial unclosed.
	let dialer = match moq.client.url.clone() {
		Some(url) => Some((url, net.client(moq.client.clone())?)),
		None => None,
	};

	let (started, shutdown) = spawn_server(tasks, moq, &cluster, net, directions).await?;
	// Even standalone: it is what reports a panic in the origin driver.
	tasks.spawn(async move { started.run().await });

	let mut bandwidth = moq_net::bandwidth::Allocator::unlimited();
	let mut connection = None;
	let mut client = None;
	if let Some((url, mut dialer)) = dialer {
		client = Some(dialer.clone());
		if directions.publish {
			dialer = dialer.with_publisher(origin.consume());
		}
		if directions.consume {
			dialer = dialer.with_subscriber(origin.clone());
		}

		// Both directions ride one connection, so this dials directly rather than
		// through `Client::publish` / `Client::consume`, which each attach a single
		// direction and dial the same URL.
		let reconnect = dialer.connect(url);
		// Read before the handle moves into the task. This allocator is persistent: it
		// survives reconnects, granting `None` while down, so it can be wired up before
		// anything connects, and every session probes for its reservations.
		bandwidth = reconnect.allocator();
		let closed = reconnect.clone();
		tasks.spawn(async move { Ok(closed.closed().await?) });
		connection = Some(reconnect);
	}

	Ok(Attached {
		bandwidth,
		origin,
		client,
		connection,
		shutdown,
	})
}

/// What [`spawn_moq`] attached to the MoQ network.
struct Attached {
	bandwidth: moq_net::bandwidth::Allocator,
	origin: moq_net::origin::Producer,
	/// The client that dialed `--connect`, to close its endpoint on exit.
	client: Option<moq_tokio::Client>,
	/// The relay connection, when `--connect` dialed one.
	connection: Option<moq_tokio::Connection>,
	shutdown: Option<Shutdown>,
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
	let Attached {
		origin,
		client,
		connection,
		..
	} = spawn_moq(&moq, &net, cluster.clone(), directions, &mut tasks).await?;
	moq::notify_ready();

	let result = play::run(origin.consume(), name, args, tasks).await;
	detach(connection, client, &cluster).await;
	result
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
	let mut signals = Signals::new()?;

	// The stage combinations were refused up front by `Invocation::validate`, before
	// anything bound a port or dialed out.
	let mut client = None;
	let mut connection = None;
	let local = tokio::task::LocalSet::new();
	let result = async {
		let Attached {
			bandwidth,
			origin,
			client: dialer,
			connection: attached,
			shutdown,
		} = spawn_moq(&moq, &net, cluster.clone(), Directions::of(&stages), &mut tasks).await?;
		client = dialer;
		connection = attached;
		// A signal while attaching drains once `drive` takes it, with no READY first.
		if signals.pending() {
			moq::stop();
		}
		moq::notify_ready();

		// stdin and stdout are one resource each, so two stages can't share them.
		let mut stdin = None;
		let mut stdout = None;

		// Each run publishes its own content and therefore mints its own instance.
		let epoch = moq_net::Epoch::mint();
		tracing::info!(%epoch, "publisher instance");

		for stage in stages {
			let name = stage.broadcast(&moq);
			match stage {
				Command::Import(import) => {
					if import.source.stdin_format().is_some() {
						claim("stdin", &mut stdin, &name)?;
					}
					let report = import.report;
					let echo = import
						.echo_path
						.as_deref()
						.map(hang::moq_net::path::Relative::new)
						.transpose()?
						.map(hang::catalog::Echo::new);
					if let Some(mut publish) =
						spawn_import(&origin, import, name, epoch.clone(), bandwidth.clone(), &mut tasks)?
					{
						// Before `run` declares a track, so the `stats` and `echo` sections ride the
						// first catalog.
						if report {
							publish = publish.with_report(connection.as_ref().map(moq_tokio::Connection::monitor))?;
						}
						if let Some(echo) = echo {
							publish = publish.with_echo(echo)?;
						}
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
			drive(&mut tasks, signals, shutdown).await
		} else {
			supervise(&local, locals.into_iter().map(Publish::run), &mut tasks);
			local.run_until(drive(&mut tasks, signals, shutdown)).await
		}
	}
	.await;

	// Even on a setup error.
	stop(detach(connection, client, &cluster), &mut tasks, local).await;
	result
}

/// Leave the MoQ network, then stop the stages.
///
/// In this order an interrupted stage's open tracks end with the session that served them,
/// which a relay reports as a lost route and a reader follows. Stopping the stages first would
/// end them unfinished, which a reader takes for the publisher failing.
async fn stop(
	leave: impl std::future::Future<Output = ()>,
	tasks: &mut JoinSet<anyhow::Result<()>>,
	local: tokio::task::LocalSet,
) {
	leave.await;
	tasks.shutdown().await;
	drop(local);
}

/// Close everything [`spawn_moq`] attached. The process exits next, so the relay
/// and cluster peers only hear we left if the closes go out now. The connection
/// first delivers what it queued, such as the finished tracks at stdin EOF, since
/// the client's close discards it.
async fn detach(
	connection: Option<moq_tokio::Connection>,
	client: Option<moq_tokio::Client>,
	cluster: &moq_relay::cluster::Cluster,
) {
	if let Some(connection) = connection
		&& let Err(err) = connection.close().await
	{
		tracing::warn!(%err, "closed before delivering everything");
	}
	if let Some(client) = client {
		client.close().await;
	}
	cluster.close().await;
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
/// that have one (the stdin containers and capture).
fn spawn_import(
	origin: &moq_net::origin::Producer,
	import: Import,
	name: String,
	epoch: moq_net::Epoch,
	bandwidth: moq_net::bandwidth::Allocator,
	tasks: &mut JoinSet<anyhow::Result<()>>,
) -> anyhow::Result<Option<Publish>> {
	if let ImportSource::Rtc(rtc) = &import.source
		&& rtc.connect.is_some()
	{
		reject_listener_cors(&rtc.cors, "import rtc")?;
	}

	let max_age = import.max_age.map(Into::into);
	let linger = import.linger.map_or(moq_mux::Linger::DEFAULT, Into::into);
	// The MoQ side every gateway publishes into, minted per source since each takes it
	// by value onto its own task.
	let target = |name: String| crate::moq::ImportTarget {
		origin: origin.clone(),
		name,
		max_age,
		linger,
		bandwidth: bandwidth.clone(),
	};

	let mut local = None;

	if let Some(format) = import.source.stdin_format() {
		warn_if_missing_format(&name);
		let config = moq_mux::catalog::Config::default()
			.with_max_age(max_age)
			.with_bandwidth(bandwidth.clone());
		let mut publish = if let ImportSource::Ts(TsImport {
			program: Some(TsProgram::All),
			..
		}) = &import.source
		{
			let name = require_broadcast(name, "import ts --program all")?;
			Publish::ts_programs(origin.clone(), moq_net::Path::new(&name)?, config)
		} else {
			let broadcast = origin.create_broadcast(&name).context("failed to create broadcast")?;
			Publish::from_publication(broadcast, &format, config)?
		}
		.with_linger(linger);
		publish.announce(epoch)?;
		local = Some(publish);
	} else {
		match import.source {
			ImportSource::Hls(hls) => {
				warn_if_missing_format(&name);
				tasks.spawn(hls::import(target(name), hls, epoch));
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
					tasks.spawn(srt::listen_import(target(name), addr, srt.latency.into(), program));
				} else if let Some(url) = srt.connect {
					tasks.spawn(srt::connect_import(target(name), url, srt.latency.into(), program));
				}
			}
			ImportSource::Rtsp(rtsp) => {
				tasks.spawn(rtsp::connect_import(target(name), rtsp.connect));
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
			follow: stdout.follow,
			stitch: stdout.stitch,
			fragment_duration: stdout.fragment_duration,
			mux_rate: stdout.mux_rate,
			passthrough: stdout.passthrough,
			catalog: export.catalog_format,
			select: export.select,
		};
		let consumer = origin.consume();
		let echo = export.echo.map(|name| report::EchoTarget {
			origin: origin.clone(),
			name,
			connection,
		});
		tasks.spawn(async move { run_stdout(consumer, name, args, echo).await });
	} else {
		match export.sink {
			ExportSink::Hls(args) => {
				let name = require_broadcast(name, "export hls")?;
				tasks.spawn(hls::export(origin.consume(), args, name));
			}
			ExportSink::Rtmp(rtmp) => {
				let max_delay = rtmp.delay.into();
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
				tasks.spawn(archive::export(origin.consume(), name, args));
			}
			ExportSink::Report => {
				tasks.spawn(report::print_report(origin.consume(), name));
			}
			ExportSink::Echo => {
				tasks.spawn(report::print_echo(origin.consume(), name));
			}
			_ => unreachable!("container formats are handled by stdout_format above"),
		}
	}

	Ok(())
}

/// Subscribe to `name` from the Origin and write it to stdout, publishing feedback beside it
/// where the catalog solicits it when `echo` is set.
async fn run_stdout(
	consumer: moq_net::origin::Consumer,
	name: String,
	args: SubscribeArgs,
	echo: Option<report::EchoTarget>,
) -> anyhow::Result<()> {
	let catalog = args.catalog_format(&name);

	// Wait for the broadcast to be announced; the export resolves any sibling broadcast a
	// rendition's `broadcast` field references (e.g. "./source") through the same origin.
	let broadcast = consumer
		.follow(name.as_str(), None)?
		.available()
		.await
		.with_context(|| format!("failed to resolve broadcast `{name}`"))?;

	let subscribe = Subscribe::new(moq_mux::Source::new(consumer, broadcast), catalog, args);
	let Some(echo) = echo else {
		return subscribe.run().await;
	};
	let mut echo = report::Echo::new(echo, subscribe.source().clone(), catalog);
	let subscribe = subscribe.with_sections(echo.sections());
	let result = tokio::select! {
		result = subscribe.run() => result,
		never = echo.run() => match never {},
	};
	echo.finish();
	result
}

/// Run until an endpoint ends, keeping all tasks alive while a signalled listener drains.
///
/// The tasks are left for the caller to shut down once the MoQ side is detached.
async fn drive(
	tasks: &mut JoinSet<anyhow::Result<()>>,
	mut signals: Signals,
	shutdown: Option<Shutdown>,
) -> anyhow::Result<()> {
	loop {
		tokio::select! {
			biased;
			() = signals.recv() => {
				moq::stop();
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
#[expect(clippy::disallowed_methods, reason = "tests read the host clock directly")]
mod tests {
	use super::*;
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
		let driving = tokio::spawn(async move { drive(&mut tasks, signals, Some(shutdown)).await });
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

	/// A session that stops reading never takes its GOAWAY, yet the deadline still
	/// closes it, and the drain returns only once its lease recorded the shutdown.
	#[tokio::test(start_paused = true)]
	async fn drain_closes_a_stalled_session_at_the_deadline() {
		use futures::FutureExt;
		use std::sync::atomic::{AtomicBool, Ordering};

		let _signals = SIGNALS.lock().await;
		let window = std::time::Duration::from_secs(10);
		let (trigger, observer) = moq_relay::shutdown::Observer::new(window);

		let (auth, mut admissions) = moq_relay::auth::Auth::embedded("test");
		let all = moq_auth::Pattern::all;
		let (producer, consumer) = moq_auth::lease::Producer::new(moq_auth::Grant::new(all(), all()));
		let (lease, ()) = tokio::join!(auth.admit(auth.request(moq_auth::Transport::Tcp, "/room")), async {
			admissions.next().await.unwrap().grant(consumer);
		});

		let mut config = moq_net::transport::memory::Config::default();
		config.protocol = Some("moq-lite-06".into());
		let (client, server) = moq_net::transport::memory::pair(config);
		let accepting = tokio::spawn(async move {
			let (session, driver) = moq_net::Server::new()
				.accept(moq_net::time::Instant::now(), server)
				.await
				.unwrap();
			tokio::spawn(moq_net::time::run(driver));
			session
		});
		let (_client, client_driver) = moq_net::Client::new()
			.connect(moq_net::time::Instant::now(), client)
			.await
			.unwrap();
		// Never dropped, which would close the transport: polled only until the session is served.
		let reading = std::sync::Arc::new(AtomicBool::new(true));
		tokio::spawn({
			let reading = reading.clone();
			async move {
				let mut driver = std::pin::pin!(moq_net::time::run(client_driver));
				std::future::poll_fn(|cx| match reading.load(Ordering::Relaxed) {
					true => driver.as_mut().poll(cx),
					false => std::task::Poll::Pending,
				})
				.await
			}
		});
		let server = accepting.await.unwrap();
		tokio::spawn(moq_relay::supervise(server, lease.unwrap(), observer.serve(), None));
		tokio::time::sleep(std::time::Duration::from_millis(1)).await;
		reading.store(false, Ordering::Relaxed);

		let mut signals = Signals::new().unwrap();
		let shutdown = Shutdown { trigger, observer };
		tokio::time::timeout(window * 2, drain(shutdown, &mut signals))
			.await
			.expect("a stalled session held the drain past its deadline");
		let (reason, _) = producer
			.closed()
			.now_or_never()
			.expect("the drain returned before the lease recorded its end");
		assert_eq!(reason, moq_auth::lease::Reason::Shutdown);
	}

	mod listen_admission {
		use super::*;
		use futures::FutureExt;
		use std::time::Duration;

		const TIMEOUT: Duration = Duration::from_secs(10);

		struct Listening {
			tasks: JoinSet<anyhow::Result<()>>,
			admissions: moq_relay::auth::Admissions,
			connection: moq_tokio::Connection,
			consumer: moq_net::origin::Consumer,
			_shutdown: Shutdown,
			_broadcast: moq_net::origin::Publication,
			_track: moq_net::track::Producer,
		}

		impl Listening {
			async fn new(timeout: &str) -> Self {
				let _ = moq_tokio::crypto::install_default();
				let cli = Invocation::try_parse_from([
					"moq",
					"--listen-tcp-bind",
					"127.0.0.1:0",
					"--listen-timeout",
					timeout,
					"import",
					"ts",
				])
				.unwrap();
				let listener = cli.moq.server_config().init_streams().unwrap().listen().await.unwrap();
				let addr = listener.tcp_local_addr().unwrap();
				let cluster = moq_relay::cluster::Cluster::new(Default::default()).unwrap();
				let broadcast = cluster.origin.create_broadcast("probe").unwrap();
				broadcast.announce(Default::default()).unwrap();
				let info = moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI);
				let track = broadcast.create_track("status", info).unwrap();
				let mut group = track.append_group().unwrap();
				group
					.write_frame(moq_net::Timestamp::ZERO, b"admitted".as_ref())
					.unwrap();
				group.finish().unwrap();
				let (auth, admissions) = moq_relay::auth::Auth::embedded("test-cli");
				let (trigger, observer) = moq_relay::shutdown::Observer::new(TIMEOUT);
				let serving = Serving {
					cluster,
					auth,
					directions: Directions {
						publish: true,
						consume: false,
					},
					shutdown: observer.clone(),
					lan: false,
					public_quic: false,
				};
				let mut tasks = JoinSet::new();
				spawn_serve(&mut tasks, listener, serving);
				let origin = moq_tokio::origin::spawn();
				let consumer = origin.consume();
				let mut client = moq_tokio::connect::Config::default();
				// Only the listener's deadline may end the undecided admission.
				client.timeout = Some(Duration::ZERO.into());
				let client = client
					.init(Default::default())
					.unwrap()
					.with_subscriber(origin)
					.with_reconnect(false);
				let connection = client.connect(format!("tcp://{addr}/").parse::<url::Url>().unwrap());
				Self {
					tasks,
					admissions,
					connection,
					consumer,
					_shutdown: Shutdown { trigger, observer },
					_broadcast: broadcast,
					_track: track,
				}
			}
		}

		#[tokio::test]
		async fn an_unanswered_admission_closes_at_the_listen_deadline() {
			let mut listening = Listening::new("1s").await;
			let admission = listening.admissions.next().await.unwrap();
			// Loopback establishes the request first; only admission runs on controlled time.
			tokio::time::pause();
			tokio::time::advance(Duration::from_millis(500)).await;
			assert!(
				listening.connection.closed().now_or_never().is_none(),
				"admission ended before its deadline"
			);
			tokio::time::advance(Duration::from_secs(1)).await;
			tokio::time::resume();
			let error = tokio::time::timeout(TIMEOUT, listening.connection.closed())
				.await
				.unwrap()
				.unwrap_err();
			assert!(
				matches!(&error, moq_tokio::Error::MoqNet(error) if moq_net::SessionError::from(error) == moq_net::SessionError::Timeout),
				"expected the listener's timeout close, got {error:?}"
			);
			drop(admission);
			listening.tasks.shutdown().await;
		}

		#[tokio::test]
		async fn a_late_admission_within_the_deadline_or_with_zero_timeout_serves() {
			for timeout in ["12s", "0"] {
				let mut listening = Listening::new(timeout).await;
				let admission = listening.admissions.next().await.unwrap();
				tokio::time::pause();
				// Past the former ten-second admission cap, but inside the listener's bound.
				tokio::time::advance(Duration::from_secs(11)).await;
				// Poll already-woken deadlines before making the decision ready.
				tokio::task::yield_now().await;
				assert!(
					listening.connection.closed().now_or_never().is_none(),
					"{timeout}: decided before the grant"
				);
				admission.grant(moq_auth::lease::Consumer::fixed(moq_auth::Grant::new(
					moq_auth::Pattern::all(),
					moq_auth::Pattern::all(),
				)));
				tokio::time::resume();
				let connection = tokio::time::timeout(TIMEOUT, listening.connection.clone().established())
					.await
					.unwrap()
					.unwrap();
				// Media proves the CLI admitted the session, even on a transport that establishes early.
				let payload = tokio::time::timeout(TIMEOUT, async {
					let broadcast = listening
						.consumer
						.follow("probe", None)
						.unwrap()
						.available()
						.await
						.unwrap();
					let mut track = broadcast
						.track("status")
						.unwrap()
						.subscribe(None)
						.await
						.unwrap()
						.ordered();
					let mut group = track.next_group().await.unwrap().unwrap();
					group.read_frame().await.unwrap().unwrap().payload
				})
				.await
				.unwrap();
				assert_eq!(&payload[..], b"admitted", "{timeout}: grant did not serve the media");
				connection.close().await.unwrap();
				listening.tasks.shutdown().await;
			}
		}
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
			.run_until(drive(&mut tasks, Signals::new().unwrap(), None))
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
			.run_until(drive(&mut tasks, Signals::new().unwrap(), None))
			.await
			.unwrap();
	}

	/// An interrupted stage's open track reaches a remote reader as its session closing, which
	/// a TS follower follows, not as a track dropped unfinished (`Stream(Internal)`).
	#[tokio::test(start_paused = true)]
	async fn an_interrupted_stage_ends_its_tracks_as_the_session_closing() {
		let mut config = moq_net::transport::memory::Config::default();
		config.protocol = Some("moq-lite-06".into());
		let (client, server) = moq_net::transport::memory::pair(config);
		let relay = moq_tokio::origin::spawn();
		let subscriber = relay.clone();
		let accepting = tokio::spawn(async move {
			let (session, driver) = moq_net::Server::new()
				.with_subscriber(subscriber)
				.accept(moq_net::time::Instant::now(), server)
				.await
				.unwrap();
			tokio::spawn(moq_net::time::run(driver));
			session
		});
		let publisher = moq_tokio::origin::spawn();
		let (session, driver) = moq_net::Client::new()
			.with_publisher(publisher.consume())
			.connect(moq_net::time::Instant::now(), client)
			.await
			.unwrap();
		tokio::spawn(moq_net::time::run(driver));
		let _relayed = accepting.await.unwrap();

		let broadcast = publisher.create_broadcast("demo").unwrap();
		broadcast.announce(Default::default()).unwrap();
		let info = moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI);
		let track = broadcast.create_track("live", info).unwrap();
		let mut group = track.append_group().unwrap();
		group.write_frame(moq_net::Timestamp::ZERO, b"live".as_ref()).unwrap();
		group.finish().unwrap();

		let reader = relay.consume();
		let mut live = reader
			.follow("demo", None)
			.unwrap()
			.available()
			.await
			.unwrap()
			.track("live")
			.unwrap()
			.subscribe(None)
			.await
			.unwrap();
		assert!(live.recv_group().await.unwrap().is_some());

		let local = tokio::task::LocalSet::new();
		let mut tasks = JoinSet::new();
		let stage: Pipeline = Box::pin(async move {
			let _open = (broadcast, track);
			std::future::pending().await
		});
		supervise(&local, [stage], &mut tasks);
		stop(
			async move {
				let _ = session.close().await;
			},
			&mut tasks,
			local,
		)
		.await;

		let end = loop {
			match live.recv_group().await {
				Ok(Some(_)) => continue,
				end => break end.map(|_| ()),
			}
		};
		assert!(matches!(end, Err(moq_net::Error::Session(_))), "{end:?}");
	}

	/// A stream bind failure fails startup before the caller can report READY, and
	/// `--connect` dials only once the listener is up, so it leaves no live dial to
	/// drop without its close.
	#[tokio::test]
	async fn a_stream_bind_failure_starts_no_dial() {
		let _ = moq_tokio::crypto::install_default();
		let occupied = std::net::TcpListener::bind("127.0.0.1:0").expect("occupy a port");
		let addr = occupied.local_addr().expect("occupied address").to_string();
		let invocation = Invocation::try_parse_from([
			"moq",
			"--connect",
			"http://127.0.0.1:9/",
			"--listen-tcp-bind",
			&addr,
			"--auth-public",
			"**",
			"import",
			"ts",
		])
		.expect("parse");
		let cluster = invocation.moq.cluster().expect("create cluster");
		let net = Net {
			quic: invocation.moq.quic.clone(),
			#[cfg(feature = "iroh")]
			iroh: None,
		};
		let mut tasks = JoinSet::new();
		let directions = Directions {
			publish: true,
			consume: false,
		};
		let Err(err) = spawn_moq(&invocation.moq, &net, cluster, directions, &mut tasks).await else {
			panic!("the occupied port must fail startup");
		};
		assert!(err.to_string().contains("failed to bind listeners"), "{err:#}");
		assert!(tasks.is_empty(), "no dial starts before the listener is up");
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

	/// An HTTP `--cluster-api-url` is a MoQ side, so start-up must attach
	/// client TLS the way the relay does rather than refuse after validate.
	#[tokio::test]
	async fn cluster_api_url_http_attaches_client_tls() {
		let _ = moq_tokio::crypto::install_default();
		let invocation =
			Invocation::try_parse_from(["moq", "--cluster-api-url", "https://api.example/peers", "import", "ts"])
				.expect("parse");
		assert!(invocation.moq.validate().is_ok());
		let cluster = invocation.moq.cluster().expect("cluster");
		let err = cluster
			.clone()
			.start()
			.await
			.expect_err("HTTP API needs a dial template")
			.to_string();
		assert!(err.contains("Cluster::with_connect"), "{err}");
		cluster
			.with_connect(invocation.moq.client.clone(), invocation.moq.quic.clone())
			.start()
			.await
			.expect("HTTP API uses the dial template's TLS");
	}

	/// Without `--connect`, only the cluster binds `--connect-bind`, so a fixed
	/// port with a WAN cluster peer starts instead of colliding with itself.
	#[tokio::test]
	async fn a_fixed_connect_bind_with_a_cluster_peer_starts() {
		let _ = moq_tokio::crypto::install_default();
		// Borrow a free port from the OS, then hand it to `--connect-bind`.
		let bind = std::net::UdpSocket::bind("127.0.0.1:0")
			.expect("probe socket")
			.local_addr()
			.expect("probe addr")
			.to_string();
		let invocation = Invocation::try_parse_from([
			"moq",
			"--connect-bind",
			&bind,
			"--cluster-connect",
			"moql://127.0.0.1:9/",
			"import",
			"ts",
		])
		.expect("parse");
		invocation.moq.validate().expect("validate");
		let cluster = invocation.moq.cluster().expect("cluster");
		let net = Net {
			quic: invocation.moq.quic.clone(),
			#[cfg(feature = "iroh")]
			iroh: None,
		};
		let mut tasks = JoinSet::new();
		let attached = spawn_moq(
			&invocation.moq,
			&net,
			cluster,
			Directions {
				publish: true,
				consume: false,
			},
			&mut tasks,
		)
		.await
		.expect("a fixed bind with a cluster peer starts");
		assert!(attached.client.is_none(), "no --connect, so no client of its own");
		tasks.abort_all();
	}

	#[test]
	fn explicit_stream_listeners_are_public_without_exposing_mesh_quic() {
		assert!(is_public_transport(moq_tokio::Transport::Tcp, false));
		assert!(is_public_transport(moq_tokio::Transport::Unix, false));
		assert!(!is_public_transport(moq_tokio::Transport::Quic, false));
		assert!(is_public_transport(moq_tokio::Transport::Quic, true));
	}

	/// A `--connect` URL can name the broadcast itself, leaving the stage's broadcast empty: the
	/// connection root, which `export report` reads like the other point exports.
	#[tokio::test]
	async fn export_report_reads_the_connection_root() {
		let invocation =
			Invocation::try_parse_from(["moq", "--connect", "http://relay/event", "export", "report"]).unwrap();
		let Some(Command::Export(export)) = invocation.stages.into_iter().next() else {
			panic!("parsed as something other than an export");
		};
		let origin = moq_tokio::origin::spawn();
		let mut tasks = JoinSet::new();
		spawn_export(&origin, export, String::new(), None, &mut tasks).expect("the root is a broadcast");
		tasks.abort_all();
	}
}
