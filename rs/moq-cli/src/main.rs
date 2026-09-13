//! moq-cli: a media router that wires endpoints onto a shared MoQ Origin.
//!
//! The binary is `moq`. See [`args`] for the `import`/`export`/`play` command
//! grammar; this module orchestrates the shared Origin and spawns the MoQ side
//! plus every stage's endpoint.

mod args;
mod complete;
#[cfg(feature = "capture")]
mod devices;
mod hls;
mod moq;
mod play;
mod publish;
mod rtc;
mod rtmp;
mod srt;
mod subscribe;
#[cfg(test)]
mod test_env;
#[cfg(feature = "transcode")]
mod transcode;
mod web;

use args::{Command, Export, ExportSink, Import, ImportSource, Invocation, MoqSide};
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

	fn server(&self, config: moq_tokio::listen::Config) -> anyhow::Result<moq_tokio::Server> {
		let server = config.init(self.quic.clone())?;
		#[cfg(feature = "iroh")]
		let server = match self.iroh.clone() {
			Some(iroh) => server.with_iroh(iroh),
			None => server,
		};
		Ok(server)
	}
}

/// Bind the MoQ listener and spawn everything that serves on it: ordinary
/// clients, the LAN mesh when `--cluster-lan` is on, and the certificate
/// endpoint for an explicit `--listen`. A no-op with no listener configured.
async fn spawn_server(
	tasks: &mut JoinSet<anyhow::Result<()>>,
	moq: &MoqSide,
	cluster: &moq_relay::Cluster,
	net: &Net,
	directions: Directions,
) -> anyhow::Result<moq_relay::Started> {
	if !moq.serves() {
		return cluster.clone().start().await.context("cluster failed to start");
	}

	let server = net.server(moq.server_config())?;
	let certificates = server.certificates();
	let cluster = attach_lan(cluster.clone(), moq, &server)?;
	// Advertise before accepting, so a `/.cluster` dial is verified against a
	// live credential rather than refused as "LAN discovery is not enabled".
	let started = cluster.clone().start().await.context("cluster failed to start")?;

	let origin = &cluster.origin;
	let server = if moq.lan() {
		server
	} else {
		route_server(server, origin, directions)
	};

	// Stream sockets bind asynchronously in `listen`, so this must finish before
	// `spawn_moq` reports readiness. The serve task only owns an already-bound
	// listener and cannot discover a late bind failure.
	let listener = server.listen().await.context("failed to bind listeners")?;
	if moq.lan() {
		spawn_cluster_serve(
			tasks,
			listener,
			cluster.clone(),
			origin.clone(),
			directions,
			moq.server.bind.is_some(),
		);
	} else {
		spawn_serve(tasks, listener);
	}

	// The certificate endpoint is for clients dialing a URL, so it follows the
	// explicit listener rather than the mesh's ephemeral one.
	if let Some(web_bind) = moq.server.bind.clone() {
		tasks.spawn(async move { web::run_web(&web_bind, certificates).await });
	}

	Ok(started)
}

/// Advertise the bound listener on the LAN when `--cluster-lan` is on.
fn attach_lan(
	cluster: moq_relay::Cluster,
	moq: &MoqSide,
	server: &moq_tokio::Server,
) -> anyhow::Result<moq_relay::Cluster> {
	if !moq.lan() {
		return Ok(cluster);
	}
	let port = server
		.local_addr()
		.context("--cluster-lan needs a QUIC listener")?
		.port();
	let mut advertise = moq_relay::LanAdvertise::new(port);
	if !moq.server_config().tls.generate.is_empty()
		&& let Some(fingerprint) = server.certificates().fingerprints().into_iter().next()
	{
		advertise = advertise.with_fingerprint(fingerprint);
	}
	Ok(cluster.with_advertise(advertise))
}

/// Accept inbound sessions, splitting LAN mesh peers off from ordinary clients.
///
/// Both share one listener; only the request path tells them apart. A listener
/// `--cluster-lan` invented is for the mesh, not for viewers.
fn spawn_cluster_serve(
	tasks: &mut JoinSet<anyhow::Result<()>>,
	mut listener: moq_tokio::Listener,
	cluster: moq_relay::Cluster,
	origin: moq_net::origin::Producer,
	directions: Directions,
	public_quic: bool,
) {
	if let Ok(addr) = listener.local_addr() {
		tracing::info!(%addr, "listening");
	}
	tasks.spawn(async move {
		let mut sessions = tokio::task::JoinSet::new();
		while let Some(request) = listener.accept().await {
			while sessions.try_join_next().is_some() {}
			if moq_relay::Cluster::is_lan_path(request.path()) {
				let conn = moq_relay::Connection::new(request, cluster.clone(), moq_relay::Auth::default())
					.with_id(cluster.next_connection_id());
				sessions.spawn(async move {
					if let Err(err) = conn.run().await {
						tracing::warn!(%err, "LAN peer session ended");
					}
				});
				continue;
			}
			if !is_public_transport(request.transport(), public_quic) {
				tracing::debug!(path = %request.path(), "refusing a non-peer request on the LAN mesh listener");
				request.close(404).await.ok();
				continue;
			}
			let mut request = request;
			if directions.publish {
				request = request.with_publisher(origin.consume());
			}
			if directions.consume {
				request = request.with_subscriber(origin.clone());
			}
			sessions.spawn(async move {
				let err = match request.ok().await {
					Ok(session) => session.closed().await.into(),
					Err(err) => err,
				};
				tracing::warn!(%err, "session ended with error");
			});
		}
		anyhow::bail!("the MoQ listener stopped accepting")
	});
}

/// Whether ordinary clients may use this transport on the shared LAN server.
fn is_public_transport(transport: moq_tokio::Transport, public_quic: bool) -> bool {
	match transport {
		moq_tokio::Transport::Tcp | moq_tokio::Transport::Unix => true,
		_ => public_quic,
	}
}

/// Attach the requested directions before stream accept loops capture the server.
fn route_server(
	server: moq_tokio::Server,
	origin: &moq_net::origin::Producer,
	directions: Directions,
) -> moq_tokio::Server {
	match directions {
		Directions {
			publish: true,
			consume: true,
		} => server.with_publisher(origin.consume()).with_subscriber(origin.clone()),
		Directions {
			publish: true,
			consume: false,
		} => server.with_publisher(origin.consume()),
		Directions {
			publish: false,
			consume: true,
		} => server.with_subscriber(origin.clone()),
		Directions {
			publish: false,
			consume: false,
		} => unreachable!("a stage always needs a direction"),
	}
}

/// Serve ordinary clients from an already-bound listener.
fn spawn_serve(tasks: &mut JoinSet<anyhow::Result<()>>, mut listener: moq_tokio::Listener) {
	if let Ok(addr) = listener.local_addr() {
		tracing::info!(%addr, "listening");
	}
	tasks.spawn(async move {
		while let Some(request) = listener.accept().await {
			tokio::spawn(async move {
				let err = match request.ok().await {
					Ok(session) => session.closed().await.into(),
					Err(err) => err,
				};
				tracing::warn!(%err, "session ended with error");
			});
		}
		Ok(())
	});
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	// TODO: It would be nice to remove this and rely on feature flags only.
	// However, some dependency is pulling in `ring` and I don't know why, so meh for now.
	rustls::crypto::aws_lc_rs::default_provider()
		.install_default()
		.expect("failed to install default crypto provider");

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
			Command::Token(token) => {
				cli.reject("token")?;
				return token.run();
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

	cli.moq.validate()?;

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
	/// Any `import`: the Origin is published outward.
	pub publish: bool,
	/// Any `export` or `play`: the Origin is filled from the network.
	pub consume: bool,
}

impl Directions {
	/// The union of what the stages need, so one attachment serves them all.
	fn of(stages: &[Command]) -> Self {
		Self {
			publish: stages.iter().any(|stage| matches!(stage, Command::Import(_))),
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
/// Returns an allocator over the uplink's bandwidth estimate, for the sources that
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
	cluster: moq_relay::Cluster,
	directions: Directions,
	tasks: &mut JoinSet<anyhow::Result<()>>,
) -> anyhow::Result<(moq_net::bandwidth::Allocator, moq_net::origin::Producer)> {
	let mut bandwidth = moq_net::bandwidth::Allocator::unlimited();
	let client = net.client(moq.client.clone())?;
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
		tasks.spawn(async move { Ok(reconnect.closed().await?) });
	}

	let started =
		notify_when_initialized(spawn_server(tasks, moq, &cluster, net, directions), moq::notify_ready).await?;
	if !started.standalone() {
		tasks.spawn(async move { started.run().await });
	}

	Ok((bandwidth, origin))
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
/// `#[tokio::main]` polls this future.
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
	let (_, origin) = spawn_moq(&moq, &net, cluster, directions, &mut tasks).await?;

	play::run(origin.consume(), name, args, tasks)
}

/// Run every stage over one Origin and one MoQ attachment.
///
/// Stages are independent: each names its own broadcast and owns its own endpoint,
/// and the first to finish (stdin EOF, Ctrl-C, or an error) ends the process.
async fn run_stages(moq: MoqSide, stages: Vec<Command>, net: Net) -> anyhow::Result<()> {
	let cluster = moq.cluster()?;
	let mut tasks: JoinSet<anyhow::Result<()>> = JoinSet::new();
	// The stdin/capture pipelines run on this thread instead of the JoinSet: the
	// platform capture stream is not Send, so their futures cannot be spawned.
	let mut locals: Vec<Publish> = Vec::new();

	// The stage combinations were refused up front by `Invocation::validate`, before
	// anything bound a port or dialed out.
	let (bandwidth, origin) = spawn_moq(&moq, &net, cluster, Directions::of(&stages), &mut tasks).await?;

	// stdin and stdout are one resource each, so two stages can't share them.
	let mut stdin = None;
	let mut stdout = None;

	for stage in stages {
		let name = stage.broadcast(&moq);
		match stage {
			Command::Import(import) => {
				if import.source.stdin_format().is_some() {
					claim("stdin", &mut stdin, &name)?;
				}
				if let Some(publish) = spawn_import(&origin, import, name, bandwidth.clone(), &mut tasks)? {
					locals.push(publish);
				}
			}
			Command::Export(export) => {
				if export.sink.stdout().is_some() {
					claim("stdout", &mut stdout, &name)?;
				}
				spawn_export(&origin, export, name, &mut tasks)?;
			}
			other => unreachable!("`{}` is not a stage", other.name()),
		}
	}

	if locals.is_empty() {
		return drive(tasks).await;
	}

	let local = tokio::task::LocalSet::new();
	supervise(&local, locals.into_iter().map(Publish::run), &mut tasks);
	local.run_until(drive(tasks)).await
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
/// Returns the pipeline that has to run on the caller's thread, for the sources
/// that have one (the stdin containers and capture).
fn spawn_import(
	origin: &moq_net::origin::Producer,
	import: Import,
	name: String,
	bandwidth: moq_net::bandwidth::Allocator,
	tasks: &mut JoinSet<anyhow::Result<()>>,
) -> anyhow::Result<Option<Publish>> {
	if let ImportSource::Rtc(rtc) = &import.source
		&& rtc.connect.is_some()
	{
		reject_listener_cors(&rtc.cors, "import rtc")?;
	}

	let max_age = import.max_age.map(moq_tokio::Duration::into_std);
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
		let broadcast = origin.create_broadcast(&name).context("failed to create broadcast")?;
		let config = moq_mux::catalog::Config::default()
			.with_max_age(max_age)
			.with_bandwidth(bandwidth.clone());
		let publish = Publish::new(broadcast, &format, config)?;
		publish.announce()?;
		local = Some(publish);
	} else {
		match import.source {
			ImportSource::Hls(hls) => {
				warn_if_missing_format(&name);
				tasks.spawn(hls::import(target(name), hls.playlist));
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
				if let Some(addr) = srt.listen {
					let name = require_broadcast(name, "import srt --listen")?;
					tasks.spawn(srt::listen_import(target(name), addr, srt.latency.into_std()));
				} else if let Some(url) = srt.connect {
					tasks.spawn(srt::connect_import(target(name), url, srt.latency.into_std()));
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
					tasks.spawn(rtc::connect_import(target(name), url));
				}
			}
			#[cfg(feature = "capture")]
			ImportSource::Capture(capture) => {
				warn_if_missing_format(&name);
				let broadcast = origin.create_broadcast(&name).context("failed to create broadcast")?;
				let publish = Publish::capture(broadcast, &capture, bandwidth.clone(), max_age)?;
				publish.announce()?;
				local = Some(publish);
			}
			_ => unreachable!("container formats are handled by stdin_format above"),
		}
	}

	Ok(local)
}

/// Route the shared Origin OUT to one stage's sink, filling it from the MoQ network.
fn spawn_export(
	origin: &moq_net::origin::Producer,
	export: Export,
	name: String,
	tasks: &mut JoinSet<anyhow::Result<()>>,
) -> anyhow::Result<()> {
	if let ExportSink::Rtc(rtc) = &export.sink
		&& rtc.connect.is_some()
	{
		reject_listener_cors(&rtc.cors, "export rtc")?;
	}

	if let Some((format, max_age, fragment_duration)) = export.sink.stdout() {
		let args = SubscribeArgs {
			format,
			max_age,
			fragment_duration,
			catalog: export.catalog_format,
			select: export.select,
		};
		let consumer = origin.consume();
		tasks.spawn(async move { run_stdout(consumer, name, args).await });
	} else {
		match export.sink {
			ExportSink::Hls(args) => {
				let name = require_broadcast(name, "export hls")?;
				tasks.spawn(hls::export(origin.consume(), args, name));
			}
			ExportSink::Rtmp(rtmp) => {
				let max_age = rtmp.max_age.into_std();
				if let Some(addr) = rtmp.endpoint.listen {
					let name = require_broadcast(name, "export rtmp --listen")?;
					tasks.spawn(rtmp::listen_export(origin.consume(), addr, name, max_age));
				} else if let Some(url) = rtmp.endpoint.connect {
					tasks.spawn(rtmp::connect_export(origin.consume(), url, name, max_age));
				}
			}
			ExportSink::Srt(srt) => {
				if let Some(addr) = srt.listen {
					let name = require_broadcast(name, "export srt --listen")?;
					tasks.spawn(srt::listen_export(origin.consume(), addr, name, srt.latency.into_std()));
				} else if let Some(url) = srt.connect {
					tasks.spawn(srt::connect_export(origin.consume(), url, name, srt.latency.into_std()));
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
			_ => unreachable!("container formats are handled by stdout_format above"),
		}
	}

	Ok(())
}

/// Subscribe to `name` from the Origin and write it to stdout.
async fn run_stdout(consumer: moq_net::origin::Consumer, name: String, args: SubscribeArgs) -> anyhow::Result<()> {
	let catalog = args.catalog_format(&name);

	// Confirm the broadcast is reachable and wait for it to be announced; `Subscribe` then
	// resolves it (and any sibling broadcast a rendition's `broadcast` field references,
	// e.g. "./source") through the origin.
	consumer
		.routed(&name)
		.await
		.ok_or_else(|| anyhow::anyhow!("origin closed before broadcast `{name}` was announced"))?;

	let source = moq_mux::Source::new(consumer, &name);
	Subscribe::new(source, catalog, args).run().await
}

/// Run every endpoint until the first finishes (stdin EOF, Ctrl-C, or an error),
/// then drop the rest.
async fn drive(mut tasks: JoinSet<anyhow::Result<()>>) -> anyhow::Result<()> {
	tasks.spawn(async {
		let _ = tokio::signal::ctrl_c().await;
		Ok(())
	});

	while let Some(res) = tasks.join_next().await {
		match res {
			Ok(Ok(())) => return Ok(()),
			Ok(Err(err)) => return Err(err),
			Err(err) if err.is_cancelled() => continue,
			Err(err) => return Err(err.into()),
		}
	}

	Ok(())
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

	/// A local pipeline that dies takes the process with it, even while another one is
	/// still running. Reporting completion from inside the task instead would miss
	/// this: a panic skips the report, leaving the survivor to keep the process alive
	/// with one broadcast silently gone.
	#[tokio::test]
	async fn a_panicking_pipeline_ends_the_process() {
		let local = tokio::task::LocalSet::new();
		let mut tasks = JoinSet::new();

		let pipelines: Vec<Pipeline> = vec![
			Box::pin(async { panic!("pipeline died") }),
			Box::pin(std::future::pending()),
		];
		supervise(&local, pipelines, &mut tasks);

		let err = local.run_until(drive(tasks)).await.unwrap_err();
		assert!(err.to_string().contains("pipeline panicked"), "{err}");
	}

	/// The first to finish ends the process, which is how stdin EOF stops a run.
	#[tokio::test]
	async fn a_finished_pipeline_ends_the_process() {
		let local = tokio::task::LocalSet::new();
		let mut tasks = JoinSet::new();

		let pipelines: Vec<Pipeline> = vec![Box::pin(async { Ok(()) }), Box::pin(std::future::pending())];
		supervise(&local, pipelines, &mut tasks);

		local.run_until(drive(tasks)).await.unwrap();
	}

	/// A stream bind failure is part of initialization, so systemd must never see
	/// READY=1 for a process that has no listening socket.
	#[tokio::test]
	async fn a_stream_bind_failure_prevents_readiness() {
		let occupied = std::net::TcpListener::bind("127.0.0.1:0").expect("occupy a port");
		let addr = occupied.local_addr().expect("occupied address").to_string();
		let invocation = Invocation::try_parse_from(["moq", "--listen-tcp-bind", &addr, "import", "ts"])
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

	/// A raw TCP bind is a complete server side even when no QUIC bind is set.
	#[tokio::test]
	async fn tcp_only_moq_side_starts_a_server() {
		let invocation = Invocation::try_parse_from(["moq", "--listen-tcp-bind", "127.0.0.1:0", "import", "ts"])
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
		let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
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
