//! Internal (ops) listener.
//!
//! A tiny plain-HTTP server, separate from the customer-facing [`Web`](crate::web::Web)
//! server, for endpoints that should never touch the public port. It's a
//! trusted-plane surface named for WHO may reach it, not for any single
//! endpoint, so it's the home for operational endpoints in general.
//!
//! Today it serves:
//! - `/metrics` - this node's own traffic counters as OpenMetrics text
//!   exposition, plus the accept-loop health of its TCP listeners
//!   ([`with_listeners`](Internal::with_listeners)) and the per-worker health of
//!   its io_uring runtime ([`with_uring`](Internal::with_uring)), and the
//!   progress of a shutdown drain ([`with_shutdown`](Internal::with_shutdown)). A distinct plane
//!   from both the customer `web` surface and the MoQ `.stats` broadcast: the same
//!   atomics, but a different transport and audience (an ops scraper, not a
//!   customer or the dashboard/billing aggregators). The runtime counters are
//!   Prometheus-only on purpose: they describe this process, not the traffic
//!   crossing it, so they have no place on the `moq-stats` wire.
//! - `/health` - a liveness mirror of the public probe, for internal checks
//!   that don't want to hit the customer port.
//! - `/nodes` - the cluster peers this relay dialed and holds a session with.
//! - `/sessions` and `/sessions/revalidate` - list or nudge live sessions on
//!   this node. A push only causes a re-check, so a caller on this trusted
//!   plane gains nothing a scheduled cadence would not do.
//!
//! Everything here is unauthenticated, so bind it only to a trusted plane -
//! loopback for a co-located scraper/agent, or a private overlay address; see
//! [`Config::listen`]. Unset by default (opt-in). A push can only cause
//! re-checks, never close a session on its own, so it belongs on this plane.

use std::net;

use anyhow::Context as _;
use axum::{
	Json, Router,
	extract::{RawQuery, State},
	http::{self, StatusCode},
	response::{IntoResponse, Response},
	routing::{get, post},
};
use axum_server::accept::DefaultAcceptor;
use prometheus_client::{
	collector::Collector,
	encoding::{
		DescriptorEncoder, EncodeGaugeValue, EncodeLabelSet, EncodeLabelValue, EncodeMetric, LabelValueEncoder,
	},
	metrics::{MetricType, counter::ConstCounter, gauge::ConstGauge},
};

/// One io_uring worker's counters, as [`with_uring`](Internal::with_uring) takes
/// them.
///
/// Uninhabited when the relay is built without the io_uring listener, so the
/// list is always empty there and the renderer skips itself, rather than
/// `cfg`-ing the two fields, the two struct literals that fill them, the
/// builder and the renderer's signature separately.
#[cfg(all(target_os = "linux", feature = "_uring"))]
type UringWorker = moq_uring::metrics::Metrics;

/// One io_uring worker's counters. See the io_uring build's alias.
#[cfg(not(all(target_os = "linux", feature = "_uring")))]
type UringWorker = std::convert::Infallible;

/// Configuration for the internal (ops) listener.
#[derive(usage_rs::Args, Clone, Debug, serde::Deserialize, serde::Serialize, Default)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[serde(deny_unknown_fields, default)]
#[non_exhaustive]
pub struct Config {
	/// Socket address for the internal listener (plain HTTP), serving the ops
	/// endpoints (`/metrics`, `/health`, `/nodes`, `/sessions`, and
	/// `/sessions/revalidate`).
	///
	/// These endpoints are unauthenticated, so bind it only to a trusted plane:
	/// loopback (e.g. `127.0.0.1:9101`) for a co-located scraper/agent, or a
	/// private overlay address. Never the public internet. Plain HTTP is
	/// intentional: on loopback there's nothing to encrypt, and a private
	/// overlay (e.g. a mesh VPN) already provides transport encryption and peer
	/// identity. Unset (the default) disables the listener entirely.
	#[usage(
		name = "internal-listen",
		long = "internal-listen",
		env = "MOQ_INTERNAL_LISTEN",
		setting = "internal.listen"
	)]
	pub listen: Option<net::SocketAddr>,
}

/// The internal (ops) service: a plain-HTTP server over the node's stats
/// registry ([`moq_net::stats::Registry`]).
pub struct Internal {
	timeout: Option<std::time::Duration>,
	config: Config,
	stats: moq_net::stats::Registry,
	nodes: Option<crate::nodes::Nodes>,
	refusals: Option<crate::refusals::Refusals>,
	sessions: crate::session::Registry,
	health: moq_tokio::accept::Health,
	listeners: Vec<moq_tokio::accept::Health>,
	uring: Vec<UringWorker>,
	#[cfg(any(feature = "stun", test))]
	stun: Option<crate::stun::Counters>,
	shutdown: Option<crate::shutdown::Observer>,
	listener: Option<net::TcpListener>,
	addr: Option<net::SocketAddr>,
}

#[derive(Clone)]
struct InternalState {
	stats: moq_net::stats::Registry,
	nodes: Option<crate::nodes::Nodes>,
	refusals: Option<crate::refusals::Refusals>,
	sessions: crate::session::Registry,
	listeners: Vec<moq_tokio::accept::Health>,
	uring: Vec<UringWorker>,
	#[cfg(any(feature = "stun", test))]
	stun: Option<crate::stun::Counters>,
	shutdown: Option<crate::shutdown::Observer>,
}

impl Internal {
	/// Create the service from its config and the node's stats registry.
	pub fn new(config: Config, stats: moq_net::stats::Registry) -> Self {
		// This listener registers itself: it is the one rendering the counters, and
		// its own are evidence about the node (the resources it can run out of are
		// process-wide) rather than about this socket. Only when it will actually run,
		// though: `routes()` is public, so an embedder can merge this surface onto its
		// own listener while `serve` stays disabled, and a zero series for a socket
		// nobody opened is a watch that can never fire.
		let health = moq_tokio::accept::Health::new("internal");
		let listeners = match config.listen {
			Some(_) => vec![health.clone()],
			None => Vec::new(),
		};
		Self {
			timeout: moq_tokio::listen::Config::default().resolved_timeout(),
			config,
			stats,
			nodes: None,
			refusals: None,
			sessions: crate::session::Registry::new(),
			health,
			listeners,
			uring: Vec::new(),
			#[cfg(any(feature = "stun", test))]
			stun: None,
			shutdown: None,
			listener: None,
			addr: None,
		}
	}

	#[cfg(feature = "stun")]
	pub(crate) fn with_stun(mut self, counters: Option<crate::stun::Counters>) -> Self {
		self.stun = counters;
		self
	}

	pub(crate) fn with_timeout(mut self, timeout: Option<std::time::Duration>) -> Self {
		self.timeout = timeout;
		self
	}

	/// Bind the configured listener now, so [`addr`](Self::addr) reports an ephemeral port before serving.
	pub fn bind(mut self) -> anyhow::Result<Self> {
		if let Some(listen) = self.config.listen
			&& self.listener.is_none()
		{
			let listener = moq_tokio::bind::tcp(listen).context("failed to bind internal listener")?;
			let addr = listener
				.local_addr()
				.context("failed to resolve internal bind address")?;
			self.addr = Some(addr);
			self.listener = Some(listener);
		}
		Ok(self)
	}

	/// The bound address after [`bind`](Self::bind) or [`crate::Relay::load`], if configured.
	pub fn addr(&self) -> Option<net::SocketAddr> {
		self.addr
	}

	/// Report other listeners' accept health at `/metrics`.
	///
	/// Takes an iterator so the accessors feed it directly, however many listeners
	/// they turn out to describe: [`Web::accept_health`](crate::web::Web::accept_health)
	/// yields an `Option`, [`moq_tokio::Server::accept_health`] a `Vec`, and an
	/// embedder can pass its own. Register every socket on the node, so a scrape
	/// covers the one that actually went quiet.
	///
	/// Register nothing for a listener that isn't running. A permanently-zero series
	/// is worse than an absent one: it reads as a watch that is passing. See
	/// [`moq_tokio::accept`] for that argument, and for why no QUIC backend has
	/// anything to register.
	pub fn with_listeners(mut self, health: impl IntoIterator<Item = moq_tokio::accept::Health>) -> Self {
		for health in health {
			// Two handles under one name would emit the same `listener` label twice,
			// which is a malformed exposition: a scraper is entitled to reject the whole
			// payload, taking the node's traffic counters down with it. Drop the
			// duplicate loudly instead, so one mis-registered listener cannot blind the
			// rest of the endpoint.
			if self.listeners.iter().any(|seen| seen.listener() == health.listener()) {
				tracing::warn!(
					listener = health.listener(),
					"ignoring a second listener registered under a name already in use; \
					 its accept failures will not be reported"
				);
				continue;
			}
			self.listeners.push(health);
		}
		self
	}

	/// Report the io_uring QUIC workers' runtime counters at `/metrics`.
	///
	/// Takes the counter sets `uring::Workers::metrics` hands out at bind time,
	/// in shard order: the position in the iterator is the `worker` label.
	/// Register them before the threads start, so a worker that never came up
	/// (or has died) still has a series that has stopped moving rather than no
	/// series at all.
	///
	/// Off the io_uring build the item type is uninhabited, so there is nothing
	/// to register and nothing is rendered.
	pub fn with_uring(mut self, workers: impl IntoIterator<Item = UringWorker>) -> Self {
		self.uring.extend(workers);
		self
	}

	/// Report the sessions a shutdown drain is still waiting on at `/metrics`.
	pub fn with_shutdown(mut self, shutdown: crate::shutdown::Observer) -> Self {
		self.shutdown = Some(shutdown);
		self
	}

	/// Attach the relay cluster used to serve the `/nodes` topology snapshot and
	/// the refused-session counts at `/metrics`.
	pub fn with_cluster(mut self, cluster: &crate::cluster::Cluster) -> Self {
		self.nodes = Some(cluster.nodes.clone());
		self.refusals = Some(cluster.refusals.clone());
		self
	}

	/// Attach the live session table served at `/sessions` and nudged at
	/// `/sessions/revalidate`.
	pub fn with_sessions(mut self, sessions: crate::session::Registry) -> Self {
		self.sessions = sessions;
		self
	}

	/// Build the ops router (`/metrics`, `/health`, `/nodes`, `/sessions`),
	/// returning a state-erased [`Router`] an embedder can extend (`merge`/`nest`
	/// its own ops routes) before handing it to [`crate::Relay::with_internal`] or
	/// [`serve`](Self::serve).
	///
	/// Anything merged in inherits this listener's "unauthenticated,
	/// trusted-plane-only" contract; see the module docs.
	pub fn routes(&self) -> Router {
		Router::new()
			.route("/metrics", get(serve_metrics))
			.route("/health", get(serve_health))
			.route("/nodes", get(serve_nodes))
			.route("/sessions", get(serve_sessions))
			.route("/sessions/revalidate", post(revalidate_sessions))
			.with_state(InternalState {
				stats: self.stats.clone(),
				nodes: self.nodes.clone(),
				refusals: self.refusals.clone(),
				sessions: self.sessions.clone(),
				listeners: self.listeners.clone(),
				uring: self.uring.clone(),
				#[cfg(any(feature = "stun", test))]
				stun: self.stun.clone(),
				shutdown: self.shutdown.clone(),
			})
	}

	/// Serve `app` on [`Config::listen`] until it shuts down.
	///
	/// The mirror of [`Web::serve`](crate::web::Web::serve): the caller builds `app`
	/// from [`routes`](Self::routes) plus whatever extra ops routes it merged in,
	/// and this owns the listener. An embedder that binds the socket itself
	/// instead would fork both the socket options and the disabled-listener
	/// contract below.
	///
	/// When no listen address is configured the future stays pending (never
	/// resolves), so it drops cleanly into a `select!` as a disabled no-op -
	/// mirroring how the relay treats other optional services.
	pub async fn serve(self, app: Router) -> anyhow::Result<()> {
		let Internal {
			listener,
			health,
			timeout,
			..
		} = self.bind()?;
		let Some(listener) = listener else {
			std::future::pending::<()>().await;
			return Ok(());
		};

		// No blanket "…server failed" context here: the caller (main.rs) adds
		// that single top-level layer, matching `Web::serve` / `Cluster::run`.
		// No accept-time work: the ops router never hands a connection to qmux, so
		// capturing a descriptor per health check would spend one for nothing.
		crate::listener::server(
			listener,
			crate::listener::Config {
				health,
				timeout,
				activity: None,
			},
			DefaultAcceptor::new(),
		)?
		.serve(app.into_make_service())
		.await?;
		Ok(())
	}

	/// Serves the default ops router on the configured listener until it shuts
	/// down. Convenience for the standalone binary; equivalent to
	/// `internal.serve(internal.routes())`.
	pub async fn run(self) -> anyhow::Result<()> {
		let app = self.routes();
		self.serve(app).await
	}
}

/// Liveness probe mirror for the internal listener. Always `200 ok`. The
/// customer-facing [`Web`](crate::web::Web) server serves its own public `/health`;
/// this one lets an internal prober check the process over the trusted plane
/// without touching the public port.
async fn serve_health() -> Response {
	(StatusCode::OK, "ok\n").into_response()
}

/// OpenMetrics text metrics for this node's own MoQ traffic counters
/// (bytes/frames/groups, announces, subscriptions, viewers, and connected sessions),
/// summed across broadcasts and split by `tier`/`role`.
///
/// Unauthenticated, which is why it lives on the internal listener rather than
/// the public web one; a scraper needs no JWT. Host system metrics
/// (CPU/memory/disk/network) are deliberately out of scope: run a dedicated node
/// exporter for those, per the relay's separation of concerns. Returns the
/// current cumulative snapshot; a downstream scraper derives rates and live
/// counts (`open - closed`).
async fn serve_metrics(State(state): State<InternalState>) -> Response {
	let mut scrape = Scrape::new(state.stats.snapshot(), &state.listeners, &state.uring);
	#[cfg(any(feature = "stun", test))]
	{
		scrape.stun = state.stun;
	}
	scrape.draining = state.shutdown.map(|shutdown| shutdown.tally().draining);
	scrape.refusals = state.refusals.map(|refusals| refusals.snapshot());
	(
		[(
			http::header::CONTENT_TYPE,
			"application/openmetrics-text; version=1.0.0; charset=utf-8",
		)],
		scrape.render(),
	)
		.into_response()
}

/// Cluster peers this relay dialed and currently holds a session with. Accepted
/// peer sessions are omitted, since a peer declares no URL to list it under.
async fn serve_nodes(State(state): State<InternalState>) -> Json<crate::nodes::Snapshot> {
	Json(state.nodes.map(|nodes| nodes.snapshot()).unwrap_or_default())
}

/// Live sessions matching the query filter: the dry run for a selector.
/// `query` is omitted from each entry so a jwt on the plane cannot be replayed.
async fn serve_sessions(RawQuery(query): RawQuery, State(state): State<InternalState>) -> Response {
	match crate::session::Filter::from_query(query.as_deref()) {
		Ok(filter) => Json(crate::session::List {
			sessions: state.sessions.list(&filter),
		})
		.into_response(),
		Err(err) => err.into_response(),
	}
}

/// Nudge every matching session to re-check now. The re-checks run in the
/// background; their outcome arrives as `end` events. No match is 200 with an
/// empty list, not a 404.
async fn revalidate_sessions(RawQuery(query): RawQuery, State(state): State<InternalState>) -> Response {
	match crate::session::Filter::from_query(query.as_deref()) {
		Ok(filter) => {
			let ids = state.sessions.revalidate(&filter);
			let status = if ids.is_empty() {
				StatusCode::OK
			} else {
				StatusCode::ACCEPTED
			};
			(status, Json(crate::session::Nudged { ids })).into_response()
		}
		Err(err) => err.into_response(),
	}
}

/// A Prometheus label value, escaped per the text exposition format so no value
/// can end its label or line early. `prometheus-client` writes label values
/// verbatim, so a tier or listener name carrying a quote would otherwise forge a
/// series.
pub(crate) struct Label<'a>(pub(crate) &'a str);

impl EncodeLabelValue for Label<'_> {
	fn encode(&self, encoder: &mut LabelValueEncoder) -> std::fmt::Result {
		use std::fmt::Write as _;
		for c in self.0.chars() {
			match c {
				'\\' => encoder.write_str("\\\\")?,
				'"' => encoder.write_str("\\\"")?,
				'\n' => encoder.write_str("\\n")?,
				c => encoder.write_char(c)?,
			}
		}
		Ok(())
	}
}

/// Encode whatever `encode` writes as one OpenMetrics text exposition.
pub(crate) fn render(encode: impl Fn(&mut DescriptorEncoder) -> std::fmt::Result + Send + Sync + 'static) -> String {
	/// The registry only takes collectors, so this adapts a closure into one.
	struct Encode<F>(F);

	impl<F> std::fmt::Debug for Encode<F> {
		fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
			f.debug_struct("Encode").finish_non_exhaustive()
		}
	}

	impl<F: Fn(&mut DescriptorEncoder) -> std::fmt::Result + Send + Sync + 'static> Collector for Encode<F> {
		fn encode(&self, mut encoder: DescriptorEncoder) -> std::fmt::Result {
			(self.0)(&mut encoder)
		}
	}

	let mut registry = prometheus_client::registry::Registry::default();
	registry.register_collector(Box::new(Encode(encode)));
	let mut body = String::new();
	// Writing to a String cannot fail, and nothing here fails on its own.
	let _ = prometheus_client::encoding::text::encode(&mut body, &registry);
	body
}

/// One metric family: its HELP/TYPE header, then a sample per label set.
pub(crate) fn family<L: EncodeLabelSet, M: EncodeMetric>(
	encoder: &mut DescriptorEncoder,
	name: &str,
	help: &str,
	kind: MetricType,
	rows: impl IntoIterator<Item = (L, M)>,
) -> std::fmt::Result {
	let mut family = encoder.encode_descriptor(name, help, None, kind)?;
	for (labels, metric) in rows {
		metric.encode(family.encode_family(&labels)?)?;
	}
	Ok(())
}

/// A labeled counter family. The name leaves off `_total`, which the encoder
/// appends to every sample.
pub(crate) fn counter<L: EncodeLabelSet>(
	encoder: &mut DescriptorEncoder,
	name: &str,
	help: &str,
	rows: impl IntoIterator<Item = (L, u64)>,
) -> std::fmt::Result {
	let rows = rows
		.into_iter()
		.map(|(labels, value)| (labels, ConstCounter::new(value)));
	family(encoder, name, help, MetricType::Counter, rows)
}

/// A labeled gauge family.
fn gauge<L: EncodeLabelSet, V: EncodeGaugeValue>(
	encoder: &mut DescriptorEncoder,
	name: &str,
	help: &str,
	rows: impl IntoIterator<Item = (L, V)>,
) -> std::fmt::Result {
	let rows = rows.into_iter().map(|(labels, value)| (labels, ConstGauge::new(value)));
	family(encoder, name, help, MetricType::Gauge, rows)
}

/// An unlabeled family with its one sample. A counter's name leaves off `_total`.
pub(crate) fn single(
	encoder: &mut DescriptorEncoder,
	name: &str,
	help: &str,
	metric: impl EncodeMetric,
) -> std::fmt::Result {
	metric.encode(encoder.encode_descriptor(name, help, None, metric.metric_type())?)
}

/// Everything one `/metrics` scrape reports, read when the scrape arrives.
///
/// The counters already live in atomics behind [`moq_net::stats::Registry`] and
/// the other sources, so a scrape reads them into a fresh `prometheus-client`
/// registry rather than mirroring them into live metrics on the hot path.
struct Scrape {
	stats: moq_net::stats::Snapshot,
	listeners: Vec<moq_tokio::accept::Health>,
	uring: Vec<UringWorker>,
	#[cfg(any(feature = "stun", test))]
	stun: Option<crate::stun::Counters>,
	draining: Option<usize>,
	refusals: Option<moq_net::stats::Refusals>,
	panics: u64,
}

impl Scrape {
	fn new(stats: moq_net::stats::Snapshot, listeners: &[moq_tokio::accept::Health], uring: &[UringWorker]) -> Self {
		Self {
			stats,
			listeners: listeners.to_vec(),
			uring: uring.to_vec(),
			#[cfg(any(feature = "stun", test))]
			stun: None,
			draining: None,
			refusals: None,
			panics: moq_tokio::panic::count(),
		}
	}

	fn render(self) -> String {
		render(move |encoder| self.encode(encoder))
	}

	fn encode(&self, encoder: &mut DescriptorEncoder) -> std::fmt::Result {
		self.encode_traffic(encoder)?;
		self.encode_sessions(encoder)?;
		encode_accepts(encoder, &self.listeners)?;
		encode_uring(encoder, &self.uring)?;
		#[cfg(any(feature = "stun", test))]
		if let Some(stun) = &self.stun {
			stun.encode(encoder)?;
		}

		// The sessions a shutdown drain is still waiting on: 0 until the drain
		// starts, and back to 0 when every session has left, which is when the
		// relay exits. A scrape that last saw it above 0 shortly before the
		// deadline means the deadline force-closed the rest; the exit log records
		// how many.
		if let Some(draining) = self.draining {
			single(
				encoder,
				"moq_relay_draining_sessions",
				"Sessions sent a shutdown GOAWAY that have not left yet.",
				ConstGauge::new(draining as u64),
			)?;
		}

		// Sessions admission turned away, one row per reason, every reason present
		// from zero so a rate over it is defined from the first scrape. A refused
		// session never reaches the traffic counters above.
		if let Some(refusals) = &self.refusals {
			use crate::refusals::RefusalKind;
			counter(
				encoder,
				"moq_relay_sessions_refused",
				"Sessions admission refused, by reason.",
				RefusalKind::ALL
					.iter()
					.map(|&refusal| ([("reason", Label(refusal.as_str()))], refusals.count(refusal))),
			)?;
		}

		// Panics contained to one session's task, process-wide. Each one is a bug
		// that closed a session instead of the process; the error log carries its
		// message and the session's span.
		single(
			encoder,
			"moq_relay_panics",
			"Panics that ended a single session instead of the process.",
			ConstCounter::new(self.panics),
		)
	}

	/// The `(tier, role)` traffic counters, summed across broadcasts.
	fn encode_traffic(&self, encoder: &mut DescriptorEncoder) -> std::fmt::Result {
		use moq_net::stats::Traffic;

		let traffic = self.stats.traffic();
		for (name, help, field) in [
			(
				"moq_relay_bytes",
				"Media payload bytes transferred.",
				(|c: &Traffic| c.bytes) as fn(&Traffic) -> u64,
			),
			("moq_relay_frames", "Media frames transferred.", |c| c.frames),
			("moq_relay_groups", "Media groups transferred.", |c| c.groups),
			(
				"moq_relay_datagrams",
				"Single-frame groups carried over unreliable QUIC datagrams; a subset of groups.",
				|c| c.datagrams,
			),
			(
				"moq_relay_inflated_bytes",
				"Payload bytes inflated from compressed groups for peers that cannot carry compression, once per group.",
				|c| c.inflated,
			),
			(
				"moq_relay_stale_bytes",
				"Media payload bytes skipped after drifting beyond a subscriber's latency budget.",
				|c| c.stale.bytes,
			),
			(
				"moq_relay_stale_frames",
				"Media frames skipped after drifting beyond a subscriber's latency budget.",
				|c| c.stale.frames,
			),
			(
				"moq_relay_stale_groups",
				"Media groups skipped after drifting beyond a subscriber's latency budget.",
				|c| c.stale.groups,
			),
			(
				"moq_relay_stale_datagrams",
				"Media datagrams skipped after drifting beyond a subscriber's latency budget.",
				|c| c.stale.datagrams,
			),
			("moq_relay_fetches", "One-shot group fetches requested.", |c| c.fetches),
			("moq_relay_announces_started", "Broadcast announces started.", |c| {
				c.announces_started
			}),
			(
				"moq_relay_announces_ended",
				"Broadcast announces ended; subtract from started for live announces.",
				|c| c.announces_ended,
			),
			("moq_relay_subscriptions_opened", "Track subscriptions opened.", |c| {
				c.subscriptions_started
			}),
			(
				"moq_relay_subscriptions_closed",
				"Track subscriptions closed; subtract from opened for live subscriptions.",
				|c| c.subscriptions_ended,
			),
			(
				"moq_relay_viewers_opened",
				"Distinct (broadcast, session) subscriptions opened.",
				|c| c.broadcasts_started,
			),
			(
				"moq_relay_viewers_closed",
				"Distinct (broadcast, session) subscriptions closed; subtract from opened for live viewers.",
				|c| c.broadcasts_ended,
			),
		] {
			let rows = traffic.iter().map(|(tier, role, totals)| {
				(
					[("tier", Label(tier.as_str())), ("role", Label(role.as_str()))],
					field(totals),
				)
			});
			counter(encoder, name, help, rows)?;
		}
		Ok(())
	}

	/// The per-tier session counters, plus control bytes split by message class.
	fn encode_sessions(&self, encoder: &mut DescriptorEncoder) -> std::fmt::Result {
		use moq_net::stats::{Presence, Totals};

		let sessions = self.stats.sessions();
		for (name, help, field) in [
			(
				"moq_relay_wire_tx_bytes",
				"Transport bytes sent, including framing and retransmissions, excluding IP, UDP, and TCP headers.",
				(|p: &Presence| p.wire_tx) as fn(&Presence) -> u64,
			),
			(
				"moq_relay_wire_rx_bytes",
				"Transport bytes received, including framing, excluding IP, UDP, and TCP headers.",
				|p| p.wire_rx,
			),
			("moq_relay_requests", "Admitted request/response exchanges.", |p| {
				p.requests
			}),
			("moq_relay_sessions_opened", "Connected sessions opened.", |p| {
				p.sessions_started
			}),
			(
				"moq_relay_sessions_closed",
				"Connected sessions closed; subtract from opened for live sessions.",
				|p| p.sessions_ended,
			),
		] {
			let rows = sessions
				.iter()
				.map(|(tier, presence)| ([("tier", Label(tier.as_str()))], field(presence)));
			counter(encoder, name, help, rows)?;
		}

		counter(
			encoder,
			"moq_relay_sessions_failed",
			"Admitted sessions ending abnormally, by kind.",
			sessions.iter().flat_map(|(tier, presence)| {
				moq_net::stats::Failure::ALL.iter().map(|&kind| {
					(
						[("tier", Label(tier.as_str())), ("kind", Label(kind.as_str()))],
						presence.sessions_failed.count(kind),
					)
				})
			}),
		)?;

		let control = self.stats.control();
		for (name, help, field) in [
			(
				"moq_relay_control_tx_bytes",
				"Control-message bytes encoded for sending, including codec prefixes; excludes transport overhead.",
				(|b: &Totals| b.tx) as fn(&Totals) -> u64,
			),
			(
				"moq_relay_control_rx_bytes",
				"Control-message bytes decoded, including codec prefixes; excludes transport overhead.",
				|b| b.rx,
			),
		] {
			let rows = control.iter().map(|(tier, class, bytes)| {
				(
					[("tier", Label(tier.as_str())), ("class", Label(class.as_str()))],
					field(bytes),
				)
			});
			counter(encoder, name, help, rows)?;
		}
		Ok(())
	}
}

/// The accept-loop health of every listener on the node.
///
/// The counters are the load-bearing half: a process out of descriptors cannot
/// answer this scrape either, so an episode is often only readable once it is over,
/// and the gauge below has gone back to zero by then. Only `exhausted` is worth
/// paging on; `connection` is junk traffic the node is fielding, and `unknown` is an
/// errno the classifier has never seen (worth a dashboard, never an escalation, since
/// a remote peer could drive it).
fn encode_accepts(encoder: &mut DescriptorEncoder, listeners: &[moq_tokio::accept::Health]) -> std::fmt::Result {
	use moq_tokio::accept::Failure;

	counter(
		encoder,
		"moq_relay_accept_failures",
		"Failed accept() calls on a listener, by class.",
		listeners.iter().flat_map(|health| {
			Failure::ALL.iter().map(|&failure| {
				(
					[
						("listener", Label(health.listener())),
						("class", Label(failure.as_str())),
					],
					health.failures(failure),
				)
			})
		}),
	)?;
	gauge(
		encoder,
		"moq_relay_accept_stalled_seconds",
		"How long a listener has been unable to accept a connection; 0 when it is serving.",
		listeners.iter().map(|health| {
			(
				[("listener", Label(health.listener()))],
				health.stalled().unwrap_or_default().as_secs_f64(),
			)
		}),
	)
}

/// The io_uring runtime's per-worker counters, one `worker` label per bound
/// worker.
///
/// These describe the runtime rather than the traffic: how well the receive
/// pool, the batching mechanisms, and the ring itself are holding up on each
/// pinned thread. A worker is a thread that never yields to anything else on the
/// node, so nothing else here can report that one of them has gone sick, and the
/// failure modes are quiet ones: a receive pool running dry leaves datagrams
/// queued in the kernel with no error anywhere, and batching that has collapsed
/// just costs syscalls.
///
/// The ratios are what to watch, not the raw counts: `rx_datagrams / rx_receives`
/// is the GRO coalescing actually achieved, `tx_datagrams / tx_sends` the GSO
/// batching, and datagrams over `enters` the syscall amortization the runtime
/// exists for.
#[cfg(all(target_os = "linux", feature = "_uring"))]
fn encode_uring(encoder: &mut DescriptorEncoder, workers: &[UringWorker]) -> std::fmt::Result {
	use moq_uring::metrics::Snapshot;

	if workers.is_empty() {
		return Ok(());
	}
	let snaps: Vec<Snapshot> = workers.iter().map(|worker| worker.snapshot()).collect();
	let rows = |field: fn(&Snapshot) -> u64| {
		snaps
			.iter()
			.enumerate()
			.map(move |(worker, snap)| ([("worker", worker)], field(snap)))
	};

	for (name, help, field) in [
		(
			"moq_relay_uring_rx_datagrams",
			"UDP datagrams received by an io_uring worker, counting each UDP_GRO segment.",
			(|snap: &Snapshot| snap.rx_datagrams) as fn(&Snapshot) -> u64,
		),
		(
			"moq_relay_uring_rx_receives",
			"Receive completions that delivered datagrams; datagrams over this is the GRO coalescing achieved.",
			|snap| snap.rx_receives,
		),
		(
			"moq_relay_uring_rx_enobufs",
			"Receives the kernel ended with ENOBUFS: no provided buffer was free, so the receive never ran. Backpressure, not a confirmed loss.",
			|snap| snap.rx_enobufs,
		),
		(
			"moq_relay_uring_rx_exhausted",
			"Receive re-arms that found no free buffer, leaving the socket unarmed until a packet was read.",
			|snap| snap.rx_exhausted,
		),
		(
			"moq_relay_uring_tx_datagrams",
			"UDP datagrams sent by an io_uring worker, counting each UDP_SEGMENT segment.",
			|snap| snap.tx_datagrams,
		),
		(
			"moq_relay_uring_tx_sends",
			"sendmsg operations staged; datagrams over this is the GSO batching achieved.",
			|snap| snap.tx_sends,
		),
		(
			"moq_relay_uring_tx_stalls",
			"Times the send-buffer pool became drained at its ceiling and an acquisition had to wait.",
			|snap| snap.tx_stalls,
		),
		(
			"moq_relay_uring_submissions",
			"Submission queue entries the kernel accepted.",
			|snap| snap.submissions,
		),
		(
			"moq_relay_uring_completions",
			"Completion queue entries dispatched.",
			|snap| snap.completions,
		),
		(
			"moq_relay_uring_enters",
			"io_uring_enter calls; datagrams over this is the syscall amortization.",
			|snap| snap.enters,
		),
		(
			"moq_relay_uring_turns",
			"Worker drive-loop turns, including the final poll that completes the root future.",
			|snap| snap.turns,
		),
		(
			"moq_relay_uring_sq_full_enters",
			"io_uring_enter calls forced by a full submission queue.",
			|snap| snap.sq_full_enters,
		),
		(
			"moq_relay_uring_parks",
			"Times a worker parked in io_uring_enter with nothing left to poll.",
			|snap| snap.parks,
		),
		(
			"moq_relay_uring_wakes",
			"futex wakes another thread had to issue because the worker was parked.",
			|snap| snap.wakes,
		),
		(
			"moq_relay_uring_timers_armed",
			"Timers armed, re-arms included.",
			|snap| snap.timers_armed,
		),
		(
			"moq_relay_uring_timers_fired",
			"Timers that reached their deadline.",
			|snap| snap.timers_fired,
		),
		(
			"moq_relay_uring_timers_cancelled",
			"Timers dropped or re-armed before their deadline.",
			|snap| snap.timers_cancelled,
		),
	] {
		counter(encoder, name, help, rows(field))?;
	}
	gauge(
		encoder,
		"moq_relay_uring_timers_active",
		"Timers currently in a worker's heap.",
		rows(Snapshot::timers_active),
	)?;

	for (name, help, field) in [
		(
			"moq_relay_uring_sqes_per_enter",
			"Submission entries accepted by each enter, including zero.",
			(|snap: &Snapshot| snap.sqes) as fn(&Snapshot) -> moq_uring::metrics::Histogram,
		),
		(
			"moq_relay_uring_cqes_per_enter",
			"Completion entries made visible by each enter, including zero.",
			|snap| snap.cqes,
		),
	] {
		let rows = snaps
			.iter()
			.enumerate()
			.map(|(worker, snap)| ([("worker", worker)], Distribution(field(snap))));
		family(encoder, name, help, MetricType::Histogram, rows)?;
	}
	Ok(())
}

/// Off the io_uring path there are no workers to describe, and [`UringWorker`]
/// is uninhabited, so the list handed here is always empty.
#[cfg(not(all(target_os = "linux", feature = "_uring")))]
fn encode_uring(_encoder: &mut DescriptorEncoder, _workers: &[UringWorker]) -> std::fmt::Result {
	Ok(())
}

/// An io_uring worker's histogram, whose cumulative buckets the encoder takes as
/// per-bucket counts.
#[cfg(all(target_os = "linux", feature = "_uring"))]
struct Distribution(moq_uring::metrics::Histogram);

#[cfg(all(target_os = "linux", feature = "_uring"))]
impl EncodeMetric for Distribution {
	fn encode(&self, mut encoder: prometheus_client::encoding::MetricEncoder) -> std::fmt::Result {
		let mut below = 0;
		let buckets: Vec<(f64, u64)> = moq_uring::metrics::Histogram::BOUNDS
			.into_iter()
			.zip(self.0.buckets)
			.map(|(bound, cumulative)| {
				// The encoder writes `f64::MAX` as `+Inf`. Bucket reads may be a few
				// operations apart, so a later bucket can read below an earlier one.
				let bucket = (
					bound.map_or(f64::MAX, |bound| bound as f64),
					cumulative.saturating_sub(below),
				);
				below = below.max(cumulative);
				bucket
			})
			.collect();
		encoder.encode_histogram::<prometheus_client::encoding::NoLabelSet>(
			self.0.sum as f64,
			self.0.count(),
			&buckets,
			None,
		)
	}

	fn metric_type(&self) -> MetricType {
		MetricType::Histogram
	}
}

#[cfg(test)]
#[expect(clippy::disallowed_methods, reason = "tests read the host clock directly")]
mod tests {
	use super::*;

	/// The traffic, listener, and io_uring series of one scrape.
	fn render_metrics(
		stats: &moq_net::stats::Snapshot,
		listeners: &[moq_tokio::accept::Health],
		uring: &[UringWorker],
	) -> String {
		Scrape::new(stats.clone(), listeners, uring).render()
	}

	#[tokio::test]
	async fn stun_metrics_are_present_only_for_a_configured_listener() {
		for stun in [None, Some(crate::stun::Counters::default())] {
			let configured = stun.is_some();
			let state = InternalState {
				stats: moq_net::stats::Registry::disabled(),
				nodes: None,
				refusals: None,
				sessions: crate::session::Registry::new(),
				listeners: Vec::new(),
				uring: Vec::new(),
				stun,
				shutdown: None,
			};
			let response = serve_metrics(State(state)).await;
			let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
			let body = std::str::from_utf8(&body).unwrap();
			assert_eq!(body.contains("moq_relay_stun_replies_total"), configured);
			assert_eq!(body.contains("moq_relay_stun_dropped_total"), configured);
		}
	}

	/// A millisecond track: the frames written here carry timestamps.
	fn timed() -> moq_net::track::Info {
		moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
	}

	/// The next route and whether it is active.
	async fn next_update(announced: &mut moq_net::announce::Consumer) -> Option<(moq_net::announce::Announce, bool)> {
		match announced.next().await? {
			moq_net::announce::Event::Start(route)
			| moq_net::announce::Event::Update(route)
			| moq_net::announce::Event::Restart(route) => Some((route, true)),
			moq_net::announce::Event::End(route) => Some((route, false)),
		}
	}

	/// An `Config` whose listener is enabled, so `Internal` registers its own.
	fn listening() -> Config {
		Config {
			listen: Some("127.0.0.1:0".parse().unwrap()),
		}
	}

	/// A listener that isn't running must not appear at all, and one that is must
	/// appear even though nothing else registered it.
	///
	/// The failure this guards is a stream-only node: it has no web listener, so a
	/// `web` series there would be a permanent zero standing in for a socket that was
	/// never opened, while the `tcp` listener that can actually fail goes unreported.
	/// Both halves are silent in exactly the way that reads as healthy.
	#[test]
	fn absent_listeners_are_not_reported_as_healthy() {
		let disabled = Internal::new(Config::default(), moq_net::stats::Registry::disabled());
		let body = render_metrics(
			&moq_net::stats::Registry::disabled().snapshot(),
			&disabled.listeners,
			&[],
		);
		assert!(
			!body.contains("listener=\"internal\""),
			"a disabled internal listener must not publish counters:\n{body}"
		);

		// What a stream-only relay looks like: no web, one tcp.
		let stream_only = Internal::new(listening(), moq_net::stats::Registry::disabled())
			.with_listeners(None)
			.with_listeners([moq_tokio::accept::Health::new("tcp")]);
		let body = render_metrics(
			&moq_net::stats::Registry::disabled().snapshot(),
			&stream_only.listeners,
			&[],
		);
		assert!(
			body.contains("moq_relay_accept_failures_total{listener=\"tcp\",class=\"exhausted\"} 0"),
			"the tcp listener must be reported:\n{body}"
		);
		assert!(
			!body.contains("listener=\"web\""),
			"a relay with no web listener must not publish a web series:\n{body}"
		);
	}

	/// Every registered listener has to appear in the exposition, at zero, before it
	/// has ever failed.
	///
	/// A counter that only springs into existence on the first failure is the shape
	/// that makes an alert unwritable: `rate(...)` over a series that does not exist
	/// yet is empty, not zero, so the dashboard is blank on a healthy node and there
	/// is nothing to notice going missing.
	#[cfg(unix)]
	#[test]
	fn accept_metrics_list_every_listener_from_zero() {
		let web = moq_tokio::accept::Health::new("web");
		let internal = Internal::new(listening(), moq_net::stats::Registry::disabled()).with_listeners([web.clone()]);

		let body = render_metrics(
			&moq_net::stats::Registry::disabled().snapshot(),
			&internal.listeners,
			&[],
		);

		for listener in ["internal", "web"] {
			for class in ["connection", "exhausted", "unknown"] {
				let line = format!("moq_relay_accept_failures_total{{listener=\"{listener}\",class=\"{class}\"}} 0");
				assert!(body.contains(&line), "missing {line} in:\n{body}");
			}
			assert!(body.contains(&format!(
				"moq_relay_accept_stalled_seconds{{listener=\"{listener}\"}} 0.0"
			)));
		}

		// An exhaustion stall is what an operator pages on, so it has to be readable
		// as a growing count and a non-zero gauge, not just a log line the node may
		// not have had the descriptors to ship.
		let emfile = std::io::Error::from_raw_os_error(24);
		// Asserted rather than assumed: 24 is EMFILE on Linux and macOS, and the
		// classification is what makes the rest of this meaningful.
		assert_eq!(
			moq_tokio::accept::Failure::classify(&emfile),
			moq_tokio::accept::Failure::Exhausted
		);
		let _ = web.failed(&emfile);
		let body = render_metrics(
			&moq_net::stats::Registry::disabled().snapshot(),
			&internal.listeners,
			&[],
		);
		assert!(body.contains("moq_relay_accept_failures_total{listener=\"web\",class=\"exhausted\"} 1"));

		let prefix = "moq_relay_accept_stalled_seconds{listener=\"web\"} ";
		let stalled: f64 = body
			.lines()
			.find_map(|line| line.strip_prefix(prefix))
			.expect("stall gauge for the web listener")
			.parse()
			.expect("stall gauge is a number");
		assert!(stalled > 0.0, "a stalled listener must not report zero seconds");
	}

	/// Every io_uring worker gets a full row of series from zero, for the same
	/// reason the accept counters do: `rate()` over a series that does not exist
	/// yet is empty rather than zero, so an alert on a worker whose receive pool
	/// has started running dry is unwritable until it has already happened. A
	/// worker that never started has to publish stuck zeros, not nothing.
	#[cfg(all(target_os = "linux", feature = "_uring"))]
	#[test]
	fn uring_metrics_list_every_worker_from_zero() {
		let workers = vec![
			moq_uring::metrics::Metrics::default(),
			moq_uring::metrics::Metrics::default(),
		];
		let internal = Internal::new(listening(), moq_net::stats::Registry::disabled()).with_uring(workers);
		let body = render_metrics(
			&moq_net::stats::Registry::disabled().snapshot(),
			&internal.listeners,
			&internal.uring,
		);

		// Every counter the module documents, for every worker, plus the derived
		// gauge. A name that drifts out of the renderer silently deletes a
		// dashboard, so the list is spelled out rather than derived from it.
		let counters = [
			"rx_datagrams",
			"rx_receives",
			"rx_enobufs",
			"rx_exhausted",
			"tx_datagrams",
			"tx_sends",
			"tx_stalls",
			"submissions",
			"completions",
			"enters",
			"turns",
			"sq_full_enters",
			"parks",
			"wakes",
			"timers_armed",
			"timers_fired",
			"timers_cancelled",
		];
		for counter in counters {
			let name = format!("moq_relay_uring_{counter}");
			assert!(
				body.contains(&format!("# TYPE {name} counter")),
				"missing type for {name}"
			);
			for worker in 0..2 {
				let line = format!("{name}_total{{worker=\"{worker}\"}} 0");
				assert!(body.contains(&line), "missing {line} in:\n{body}");
			}
		}
		for name in ["sqes_per_enter", "cqes_per_enter"] {
			let name = format!("moq_relay_uring_{name}");
			assert!(body.contains(&format!("# TYPE {name} histogram")));
			for worker in 0..2 {
				for bound in [
					"0.0", "1.0", "2.0", "4.0", "8.0", "16.0", "32.0", "64.0", "128.0", "256.0", "+Inf",
				] {
					assert!(body.contains(&format!("{name}_bucket{{le=\"{bound}\",worker=\"{worker}\"}} 0")));
				}
				assert!(body.contains(&format!("{name}_count{{worker=\"{worker}\"}} 0")));
				assert!(body.contains(&format!("{name}_sum{{worker=\"{worker}\"}} 0.0")));
			}
		}
		assert!(body.contains("# TYPE moq_relay_uring_timers_active gauge"));
		assert!(body.contains("moq_relay_uring_timers_active{worker=\"1\"} 0"));
	}

	/// A worker's cumulative buckets come out cumulative again, not summed twice.
	#[cfg(all(target_os = "linux", feature = "_uring"))]
	#[test]
	fn uring_histograms_keep_cumulative_buckets() {
		let histogram = moq_uring::metrics::Histogram {
			buckets: [1, 3, 3, 6, 6, 6, 6, 6, 6, 6, 7],
			sum: 40,
		};
		let body = render(move |encoder| {
			let rows = [([("worker", 0)], Distribution(histogram))];
			family(encoder, "h", "A histogram.", MetricType::Histogram, rows)
		});
		for line in [
			"h_bucket{le=\"0.0\",worker=\"0\"} 1",
			"h_bucket{le=\"1.0\",worker=\"0\"} 3",
			"h_bucket{le=\"2.0\",worker=\"0\"} 3",
			"h_bucket{le=\"4.0\",worker=\"0\"} 6",
			"h_bucket{le=\"256.0\",worker=\"0\"} 6",
			"h_bucket{le=\"+Inf\",worker=\"0\"} 7",
			"h_count{worker=\"0\"} 7",
			"h_sum{worker=\"0\"} 40.0",
		] {
			assert!(body.lines().any(|l| l == line), "missing {line} in:\n{body}");
		}
	}

	/// A relay with no io_uring workers publishes no worker series at all,
	/// rather than a `worker="0"` row that is permanently zero because nothing
	/// is behind it.
	#[test]
	fn absent_uring_workers_are_not_reported() {
		let internal = Internal::new(listening(), moq_net::stats::Registry::disabled());
		let body = render_metrics(
			&moq_net::stats::Registry::disabled().snapshot(),
			&internal.listeners,
			&internal.uring,
		);
		assert!(!body.contains("moq_relay_uring_"), "unexpected worker series:\n{body}");
	}

	/// `serve` hosts the router it is HANDED, so an embedder's extra ops routes
	/// answer on the same internal listener as the built-in ones. That is what
	/// lets an embedder extend this surface without binding a socket of its own
	/// and forking both the socket options and the disabled-listener contract.
	#[tokio::test]
	async fn serve_hosts_merged_routes_alongside_the_defaults() {
		let listen = Some("127.0.0.1:0".parse().unwrap());
		let internal = Internal::new(Config { listen }, moq_net::stats::Registry::disabled())
			.bind()
			.expect("bind internal listener");
		let addr = internal.addr().expect("internal listener is configured");
		let app = internal
			.routes()
			.merge(Router::new().route("/embedder", get(async || "embedded\n")));
		let server = tokio::spawn(internal.serve(app));

		let client = reqwest::Client::new();
		let url = format!("http://{addr}");
		let embedder = client
			.get(format!("{url}/embedder"))
			.send()
			.await
			.expect("embedder request");

		assert_eq!(embedder.status(), reqwest::StatusCode::OK);
		assert_eq!(embedder.text().await.expect("embedder body"), "embedded\n");

		let health = client
			.get(format!("{url}/health"))
			.send()
			.await
			.expect("health request");
		assert_eq!(health.status(), reqwest::StatusCode::OK);

		server.abort();
	}

	/// An unconfigured listener leaves `serve` pending rather than resolving, so
	/// dropping it into a `select!` disables the listener instead of completing
	/// an arm immediately and tearing the rest of the process down with it.
	#[tokio::test(start_paused = true)]
	async fn serve_stays_pending_without_a_listen_address() {
		let internal = Internal::new(Config::default(), moq_net::stats::Registry::disabled());
		let app = internal.routes();

		let elapsed = tokio::time::timeout(std::time::Duration::from_secs(60), internal.serve(app)).await;
		assert!(elapsed.is_err(), "serve resolved with no listen address configured");
	}

	#[tokio::test]
	async fn nodes_endpoint_is_empty_without_an_attached_cluster() {
		let state = InternalState {
			stats: moq_net::stats::Registry::disabled(),
			nodes: None,
			refusals: None,
			sessions: crate::session::Registry::new(),
			listeners: Vec::new(),
			uring: Vec::new(),
			#[cfg(any(feature = "stun", test))]
			stun: None,
			shutdown: None,
		};

		let Json(snapshot) = serve_nodes(State(state)).await;
		assert!(snapshot.nodes.is_empty());
	}

	#[tokio::test]
	async fn nodes_endpoint_uses_the_attached_cluster_registry() {
		let nodes = crate::nodes::Nodes::default();
		let _connection = nodes.connect_outbound(0, "https://relay-b.example/");
		let state = InternalState {
			stats: moq_net::stats::Registry::disabled(),
			nodes: Some(nodes),
			refusals: None,
			sessions: crate::session::Registry::new(),
			listeners: Vec::new(),
			uring: Vec::new(),
			#[cfg(any(feature = "stun", test))]
			stun: None,
			shutdown: None,
		};

		let Json(snapshot) = serve_nodes(State(state)).await;
		assert_eq!(snapshot.nodes[0].node, "https://relay-b.example/");
	}

	/// Every refusal reason renders from zero, so a rate over any of them is
	/// defined before the first refusal.
	#[test]
	fn refusal_metrics_list_every_reason_from_zero() {
		use crate::refusals::{RefusalKind, Refusals};

		let refusals = Refusals::default();
		refusals.record(RefusalKind::Unavailable);
		let mut scrape = Scrape::new(Default::default(), &[], &[]);
		scrape.refusals = Some(refusals.snapshot());
		let body = scrape.render();

		assert!(
			body.contains("# TYPE moq_relay_sessions_refused counter"),
			"type header:\n{body}"
		);
		for &refusal in RefusalKind::ALL {
			let expected = u64::from(refusal == RefusalKind::Unavailable);
			let row = format!(
				"moq_relay_sessions_refused_total{{reason=\"{}\"}} {expected}",
				refusal.as_str()
			);
			assert!(body.contains(&row), "missing {row}:\n{body}");
		}
	}

	/// A label value carrying a quote, backslash, or newline stays inside its label, so
	/// it can neither break the exposition nor forge another series.
	#[test]
	fn label_values_are_escaped() {
		let stats = moq_net::stats::Registry::new(Default::default());
		let tier = "a\\\\b\"} 9\nmoq_relay_requests_total{tier=\"x";
		drop(
			stats
				.tier(moq_net::stats::Tier::new(moq_net::Path::from_static(tier)))
				.request(moq_net::Path::from_static("root")),
		);
		let body = render_metrics(&stats.snapshot(), &[], &[]);
		assert!(
			body.contains(r#"moq_relay_requests_total{tier="a\\\\b\"} 9\nmoq_relay_requests_total{tier=\"x"} 1"#),
			"{body}"
		);
		assert!(
			body.lines()
				.all(|line| !line.starts_with("moq_relay_requests_total{tier=\"x")),
			"{body}"
		);
	}

	#[test]
	fn request_metrics_never_open_a_session() {
		let stats = moq_net::stats::Registry::new(Default::default());
		let request = stats
			.tier(moq_net::stats::Tier::new(moq_net::Path::from_static("http")))
			.request(moq_net::Path::from_static("root"));
		drop(request);
		let body = render_metrics(&stats.snapshot(), &[], &[]);
		assert!(body.contains("# TYPE moq_relay_requests counter"), "{body}");
		assert!(body.contains("moq_relay_requests_total{tier=\"http\"} 1"), "{body}");
		assert!(
			body.contains("moq_relay_sessions_opened_total{tier=\"http\"} 0"),
			"{body}"
		);
		assert!(
			body.contains("moq_relay_sessions_closed_total{tier=\"http\"} 0"),
			"{body}"
		);
	}

	#[test]
	fn wire_metrics_include_internal_sessions_and_keep_final_totals() {
		let registry = moq_net::stats::Registry::new(Default::default());
		for tier in ["internal", "edge"] {
			let session = registry
				.tier(moq_net::stats::Tier::new(moq_net::Path::from_static(tier)))
				.session(moq_net::Path::empty());
			let source = moq_net::transport::Wire::new();
			let guard = session.wire(source.clone());
			source.set_sent_bytes(123);
			source.set_received_bytes(456);
			let check = || {
				let body = render_metrics(&registry.snapshot(), &[], &[]);
				for (direction, value) in [("tx", 123), ("rx", 456)] {
					let metric = format!("moq_relay_wire_{direction}_bytes");
					assert!(body.contains(&format!("# TYPE {metric} counter")), "{body}");
					assert!(
						body.contains(&format!("{metric}_total{{tier=\"{tier}\"}} {value}")),
						"{body}"
					);
				}
			};
			check();
			check();
			drop(guard);
			drop(session);
			registry.report(&mut Default::default());
			check();
		}
	}

	#[tokio::test]
	async fn announce_metrics_keep_retired_totals() {
		let stats = moq_net::stats::Registry::new(Default::default());
		let producer = moq_tokio::origin::spawn().with_stats(
			stats
				.tier(moq_net::stats::Tier::new(moq_net::Path::from_static("edge")))
				.session(moq_net::Path::from_static("root")),
		);
		let broadcast = producer.create_broadcast("demo/retired").unwrap();
		broadcast.announce(Default::default()).unwrap();

		let body = render_metrics(&stats.snapshot(), &[], &[]);
		assert!(
			body.contains("moq_relay_announces_started_total{tier=\"edge\",role=\"subscriber\"} 1"),
			"live announce start:\n{body}"
		);
		assert!(
			body.contains("moq_relay_announces_ended_total{tier=\"edge\",role=\"subscriber\"} 0"),
			"live announce end:\n{body}"
		);

		broadcast.unannounce();
		drop(broadcast);
		drop(producer);
		let mut report = moq_net::stats::Report::default();
		stats.report(&mut report);

		let body = render_metrics(&stats.snapshot(), &[], &[]);
		assert!(
			body.contains("moq_relay_announces_started_total{tier=\"edge\",role=\"subscriber\"} 1"),
			"retired announce start:\n{body}"
		);
		assert!(
			body.contains("moq_relay_announces_ended_total{tier=\"edge\",role=\"subscriber\"} 1"),
			"retired announce end:\n{body}"
		);
	}

	/// The `/metrics` renderer emits well-formed Prometheus exposition: a
	/// HELP/TYPE header per metric and a labeled line carrying the live counter
	/// value, summed across broadcasts.
	#[tokio::test(start_paused = true)]
	async fn metrics_render_exposition() {
		use moq_net::Timestamp;
		use moq_net::stats::{Registry, Tier};

		let stats = Registry::new(Default::default());

		// Default-tier egress: an untagged local publisher writes, a tagged egress
		// consumer reads it out, so publisher `bytes` advance on the default tier.
		let default_ctx = stats.tier(Tier::default()).session(moq_net::Path::from_static("acme"));
		let pub_origin = moq_tokio::origin::spawn();
		let egress = pub_origin.consume().with_stats(default_ctx.clone());
		let mut announced = egress.announced();
		let pub_source = pub_origin.create_broadcast("demo/x").unwrap();
		pub_source.announce(Default::default()).unwrap();
		let pub_track = pub_source.create_track("video", timed()).unwrap();

		// Named-tier ingress: a tagged ingress producer writes, so subscriber
		// `bytes` advance on the regional tier.
		let regional_ctx = stats
			.tier(Tier::new(moq_net::Path::from_static("region/sjc")))
			.session(moq_net::Path::from_static("peer"));
		let sub_origin = moq_tokio::origin::spawn().with_stats(regional_ctx.clone());
		let sub_source = sub_origin.create_broadcast("demo/x").unwrap();
		sub_source.announce(Default::default()).unwrap();
		let sub_track = sub_source.create_track("audio", timed()).unwrap();

		tokio::time::sleep(std::time::Duration::from_millis(1)).await;
		tokio::time::sleep(std::time::Duration::from_millis(1)).await;

		// Leave 46 bytes across two frames behind the live edge, then read 1234
		// egress bytes out of the default-tier broadcast.
		let (update, active) = next_update(&mut announced).await.unwrap();
		assert!(active);
		let bc = egress.request(update.prefix.clone(), None).await.unwrap();
		let mut egress_sub = bc.track("video").unwrap().subscribe(None).await.unwrap();
		{
			let mut group = pub_track.append_group().unwrap();
			group.write_frame(Timestamp::ZERO, vec![0u8; 12]).unwrap();
			group.write_frame(Timestamp::ZERO, vec![0u8; 34]).unwrap();
			group.finish().unwrap();
		}
		{
			let mut group = pub_track.append_group().unwrap();
			group
				.write_frame(Timestamp::from_millis(10_000).unwrap(), vec![0u8; 1234])
				.unwrap();
			group.finish().unwrap();
		}
		let mut group = egress_sub.recv_group().await.unwrap().unwrap();
		while group.read_frame().await.unwrap().is_some() {}

		// Write 56 ingress bytes into the regional-tier broadcast.
		{
			let mut group = sub_track.append_group().unwrap();
			group.write_frame(Timestamp::ZERO, vec![0u8; 56]).unwrap();
			group.finish().unwrap();
		}

		let body = render_metrics(&stats.snapshot(), &[], &[]);

		assert!(body.contains("# TYPE moq_relay_bytes counter"), "type header:\n{body}");
		assert!(
			body.contains("moq_relay_bytes_total{tier=\"\",role=\"publisher\"} 1234"),
			"default-tier egress bytes (empty tier label):\n{body}"
		);
		assert!(
			body.contains("moq_relay_bytes_total{tier=\"region/sjc\",role=\"subscriber\"} 56"),
			"named tier gets its own row:\n{body}"
		);
		assert!(
			body.contains("moq_relay_stale_bytes_total{tier=\"\",role=\"publisher\"} 46"),
			"stale egress bytes:\n{body}"
		);
		assert!(
			body.contains("moq_relay_stale_frames_total{tier=\"\",role=\"publisher\"} 2"),
			"stale egress frames:\n{body}"
		);
		assert!(
			body.contains("moq_relay_stale_groups_total{tier=\"\",role=\"publisher\"} 1"),
			"stale egress groups:\n{body}"
		);
		assert!(
			body.contains("moq_relay_stale_datagrams_total{tier=\"\",role=\"publisher\"} 0"),
			"stale egress datagrams:\n{body}"
		);
		assert!(
			body.contains("moq_relay_sessions_opened_total{tier=\"\"} 1"),
			"session presence:\n{body}"
		);
	}

	/// Two plain egress sessions reading one compressed group share a single inflate.
	#[tokio::test(start_paused = true)]
	async fn metrics_count_one_inflate_per_group() {
		use moq_net::Timestamp;
		use moq_net::stats::{Registry, Tier};

		let stats = Registry::new(Default::default());
		let origin = moq_tokio::origin::spawn();
		let source = origin.create_broadcast("demo/x").unwrap();
		source.announce(Default::default()).unwrap();
		let track = source
			.create_track("catalog.json", timed().with_compress(true))
			.unwrap();
		let payload = format!("{{\"pad\":\"{}\"}}", "abc".repeat(300));

		let mut readers = Vec::new();
		for session in ["a", "b"] {
			let ctx = stats
				.tier(Tier::default())
				.session(moq_net::Path::new(session).unwrap());
			let egress = origin.consume().with_stats(ctx);
			let bc = egress.request("demo/x", None).await.unwrap();
			readers.push(bc.track("catalog.json").unwrap().subscribe(None).await.unwrap());
		}
		let mut group = track.append_group().unwrap();
		group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
		group.finish().unwrap();
		for reader in &mut readers {
			let mut group = reader.recv_group().await.unwrap().unwrap();
			assert_eq!(group.read_frame().await.unwrap().unwrap().payload, payload.as_bytes());
		}

		let body = render_metrics(&stats.snapshot(), &[], &[]);
		assert!(
			body.contains(&format!(
				"moq_relay_inflated_bytes_total{{tier=\"\",role=\"publisher\"}} {}",
				payload.len()
			)),
			"one inflated copy:\n{body}"
		);
	}

	/// Every family header and sample in an exposition, as `# TYPE family kind` and
	/// `kind name{labels} value`, sorted, so the comparison holds across text formats:
	/// only a renamed, relabeled, retyped, or revalued series changes it. Labels
	/// compare as a set, and bucket bounds and values as numbers.
	fn series(body: &str) -> Vec<String> {
		let mut types = std::collections::HashMap::new();
		let mut rows = Vec::new();
		for line in body.lines() {
			if let Some(header) = line.strip_prefix("# TYPE ") {
				let (family, kind) = header.split_once(' ').unwrap();
				types.insert(family.to_string(), kind.to_string());
				// A counter family may be named with or without its `_total` sample suffix.
				let family = match kind {
					"counter" => format!("{}_total", family.strip_suffix("_total").unwrap_or(family)),
					_ => family.to_string(),
				};
				rows.push(format!("# TYPE {family} {kind}"));
				continue;
			}
			if line.starts_with('#') || line.is_empty() {
				continue;
			}
			let (sample, value) = line.rsplit_once(' ').unwrap();
			let (name, labels) = sample.split_once('{').unwrap_or((sample, ""));
			let kind = std::iter::once(name)
				.chain(["_total", "_bucket", "_sum", "_count"].map(|suffix| name.strip_suffix(suffix).unwrap_or(name)))
				.find_map(|family| types.get(family))
				.unwrap_or_else(|| panic!("no TYPE for {name}"));

			let mut normalized = Vec::new();
			let mut rest = labels.strip_suffix('}').unwrap_or(labels);
			while let Some((key, tail)) = rest.split_once("=\"") {
				let mut end = 0;
				let mut escaped = false;
				for (i, c) in tail.char_indices() {
					match c {
						_ if escaped => escaped = false,
						'\\' => escaped = true,
						'"' => {
							end = i;
							break;
						}
						_ => {}
					}
				}
				let value = &tail[..end];
				let value = match (key, value.parse::<f64>()) {
					("le", Ok(bound)) => bound.to_string(),
					_ => value.to_string(),
				};
				normalized.push(format!("{key}=\"{value}\""));
				rest = tail[end + 1..].trim_start_matches(',');
			}

			// A label set is unordered: Prometheus sorts it on ingest.
			normalized.sort();
			let value: f64 = value.parse().unwrap();
			rows.push(format!("{kind} {name}{{{}}} {value}", normalized.join(",")));
		}
		rows.sort();
		rows
	}

	/// The whole `/metrics` series set for one fixture of every source, pinned so a
	/// renamed, relabeled, or retyped series fails here rather than on a dashboard.
	#[tokio::test]
	async fn metrics_series_are_pinned() {
		let stats = moq_net::stats::Registry::new(Default::default());
		let edge = stats.tier(moq_net::stats::Tier::new(moq_net::Path::from_static("edge")));
		let origin = moq_tokio::origin::spawn().with_stats(edge.session(moq_net::Path::from_static("root")));
		let broadcast = origin.create_broadcast("demo").unwrap();
		broadcast.announce(Default::default()).unwrap();
		drop(edge.request(moq_net::Path::from_static("root")));
		let refusals = crate::refusals::Refusals::default();
		refusals.record(crate::refusals::RefusalKind::Unavailable);
		let (_trigger, shutdown) = crate::shutdown::Observer::new(std::time::Duration::from_secs(1));

		let state = InternalState {
			stats,
			nodes: None,
			refusals: Some(refusals),
			sessions: crate::session::Registry::new(),
			listeners: vec![moq_tokio::accept::Health::new("tcp")],
			#[cfg(all(target_os = "linux", feature = "_uring"))]
			uring: vec![moq_uring::metrics::Metrics::default()],
			#[cfg(not(all(target_os = "linux", feature = "_uring")))]
			uring: Vec::new(),
			stun: Some(crate::stun::Counters::default()),
			shutdown: Some(shutdown),
		};
		let response = serve_metrics(State(state)).await;
		let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
		let body = std::str::from_utf8(&body).unwrap();

		// Panics are counted process-wide, so only the series is pinned, not its value.
		let mut actual = series(body);
		let panics = actual
			.iter()
			.position(|row| row.starts_with("counter moq_relay_panics_total{} "))
			.unwrap_or_else(|| panic!("no panic counter in:\n{body}"));
		actual.remove(panics);

		let mut expected: Vec<String> = EXPECTED_SERIES.trim().lines().map(str::to_string).collect();
		#[cfg(all(target_os = "linux", feature = "_uring"))]
		expected.extend(EXPECTED_URING_SERIES.trim().lines().map(str::to_string));
		expected.sort();
		assert_eq!(actual, expected, "{body}");
	}

	const EXPECTED_SERIES: &str = r#"
# TYPE moq_relay_accept_failures_total counter
# TYPE moq_relay_accept_stalled_seconds gauge
# TYPE moq_relay_announces_ended_total counter
# TYPE moq_relay_announces_started_total counter
# TYPE moq_relay_bytes_total counter
# TYPE moq_relay_control_rx_bytes_total counter
# TYPE moq_relay_control_tx_bytes_total counter
# TYPE moq_relay_datagrams_total counter
# TYPE moq_relay_draining_sessions gauge
# TYPE moq_relay_fetches_total counter
# TYPE moq_relay_frames_total counter
# TYPE moq_relay_groups_total counter
# TYPE moq_relay_inflated_bytes_total counter
# TYPE moq_relay_panics_total counter
# TYPE moq_relay_requests_total counter
# TYPE moq_relay_sessions_closed_total counter
# TYPE moq_relay_sessions_failed_total counter
# TYPE moq_relay_sessions_opened_total counter
# TYPE moq_relay_sessions_refused_total counter
# TYPE moq_relay_stale_bytes_total counter
# TYPE moq_relay_stale_datagrams_total counter
# TYPE moq_relay_stale_frames_total counter
# TYPE moq_relay_stale_groups_total counter
# TYPE moq_relay_stun_dropped_total counter
# TYPE moq_relay_stun_io_errors_total counter
# TYPE moq_relay_stun_replies_total counter
# TYPE moq_relay_subscriptions_closed_total counter
# TYPE moq_relay_subscriptions_opened_total counter
# TYPE moq_relay_viewers_closed_total counter
# TYPE moq_relay_viewers_opened_total counter
# TYPE moq_relay_wire_rx_bytes_total counter
# TYPE moq_relay_wire_tx_bytes_total counter
counter moq_relay_accept_failures_total{class="connection",listener="tcp"} 0
counter moq_relay_accept_failures_total{class="exhausted",listener="tcp"} 0
counter moq_relay_accept_failures_total{class="unknown",listener="tcp"} 0
counter moq_relay_announces_ended_total{role="publisher",tier="edge"} 0
counter moq_relay_announces_ended_total{role="subscriber",tier="edge"} 0
counter moq_relay_announces_started_total{role="publisher",tier="edge"} 0
counter moq_relay_announces_started_total{role="subscriber",tier="edge"} 1
counter moq_relay_bytes_total{role="publisher",tier="edge"} 0
counter moq_relay_bytes_total{role="subscriber",tier="edge"} 0
counter moq_relay_datagrams_total{role="publisher",tier="edge"} 0
counter moq_relay_datagrams_total{role="subscriber",tier="edge"} 0
counter moq_relay_fetches_total{role="publisher",tier="edge"} 0
counter moq_relay_fetches_total{role="subscriber",tier="edge"} 0
counter moq_relay_frames_total{role="publisher",tier="edge"} 0
counter moq_relay_frames_total{role="subscriber",tier="edge"} 0
counter moq_relay_groups_total{role="publisher",tier="edge"} 0
counter moq_relay_groups_total{role="subscriber",tier="edge"} 0
counter moq_relay_inflated_bytes_total{role="publisher",tier="edge"} 0
counter moq_relay_inflated_bytes_total{role="subscriber",tier="edge"} 0
counter moq_relay_requests_total{tier="edge"} 1
counter moq_relay_sessions_closed_total{tier="edge"} 0
counter moq_relay_sessions_failed_total{kind="application",tier="edge"} 0
counter moq_relay_sessions_failed_total{kind="expired",tier="edge"} 0
counter moq_relay_sessions_failed_total{kind="idle_timeout",tier="edge"} 0
counter moq_relay_sessions_failed_total{kind="transport",tier="edge"} 0
counter moq_relay_sessions_opened_total{tier="edge"} 1
counter moq_relay_sessions_refused_total{reason="expired"} 0
counter moq_relay_sessions_refused_total{reason="forbidden"} 0
counter moq_relay_sessions_refused_total{reason="lan"} 0
counter moq_relay_sessions_refused_total{reason="refused"} 0
counter moq_relay_sessions_refused_total{reason="request"} 0
counter moq_relay_sessions_refused_total{reason="unavailable"} 1
counter moq_relay_stale_bytes_total{role="publisher",tier="edge"} 0
counter moq_relay_stale_bytes_total{role="subscriber",tier="edge"} 0
counter moq_relay_stale_datagrams_total{role="publisher",tier="edge"} 0
counter moq_relay_stale_datagrams_total{role="subscriber",tier="edge"} 0
counter moq_relay_stale_frames_total{role="publisher",tier="edge"} 0
counter moq_relay_stale_frames_total{role="subscriber",tier="edge"} 0
counter moq_relay_stale_groups_total{role="publisher",tier="edge"} 0
counter moq_relay_stale_groups_total{role="subscriber",tier="edge"} 0
counter moq_relay_stun_dropped_total{reason="capacity"} 0
counter moq_relay_stun_dropped_total{reason="global"} 0
counter moq_relay_stun_dropped_total{reason="invalid"} 0
counter moq_relay_stun_dropped_total{reason="io"} 0
counter moq_relay_stun_dropped_total{reason="source"} 0
counter moq_relay_stun_io_errors_total{} 0
counter moq_relay_stun_replies_total{} 0
counter moq_relay_subscriptions_closed_total{role="publisher",tier="edge"} 0
counter moq_relay_subscriptions_closed_total{role="subscriber",tier="edge"} 0
counter moq_relay_subscriptions_opened_total{role="publisher",tier="edge"} 0
counter moq_relay_subscriptions_opened_total{role="subscriber",tier="edge"} 0
counter moq_relay_viewers_closed_total{role="publisher",tier="edge"} 0
counter moq_relay_viewers_closed_total{role="subscriber",tier="edge"} 0
counter moq_relay_viewers_opened_total{role="publisher",tier="edge"} 0
counter moq_relay_viewers_opened_total{role="subscriber",tier="edge"} 0
counter moq_relay_wire_rx_bytes_total{tier="edge"} 0
counter moq_relay_wire_tx_bytes_total{tier="edge"} 0
gauge moq_relay_accept_stalled_seconds{listener="tcp"} 0
gauge moq_relay_draining_sessions{} 0
"#;

	#[cfg(all(target_os = "linux", feature = "_uring"))]
	const EXPECTED_URING_SERIES: &str = r#"
# TYPE moq_relay_uring_completions_total counter
# TYPE moq_relay_uring_cqes_per_enter histogram
# TYPE moq_relay_uring_enters_total counter
# TYPE moq_relay_uring_parks_total counter
# TYPE moq_relay_uring_rx_datagrams_total counter
# TYPE moq_relay_uring_rx_enobufs_total counter
# TYPE moq_relay_uring_rx_exhausted_total counter
# TYPE moq_relay_uring_rx_receives_total counter
# TYPE moq_relay_uring_sq_full_enters_total counter
# TYPE moq_relay_uring_sqes_per_enter histogram
# TYPE moq_relay_uring_submissions_total counter
# TYPE moq_relay_uring_timers_active gauge
# TYPE moq_relay_uring_timers_armed_total counter
# TYPE moq_relay_uring_timers_cancelled_total counter
# TYPE moq_relay_uring_timers_fired_total counter
# TYPE moq_relay_uring_turns_total counter
# TYPE moq_relay_uring_tx_datagrams_total counter
# TYPE moq_relay_uring_tx_sends_total counter
# TYPE moq_relay_uring_tx_stalls_total counter
# TYPE moq_relay_uring_wakes_total counter
counter moq_relay_uring_completions_total{worker="0"} 0
counter moq_relay_uring_enters_total{worker="0"} 0
counter moq_relay_uring_parks_total{worker="0"} 0
counter moq_relay_uring_rx_datagrams_total{worker="0"} 0
counter moq_relay_uring_rx_enobufs_total{worker="0"} 0
counter moq_relay_uring_rx_exhausted_total{worker="0"} 0
counter moq_relay_uring_rx_receives_total{worker="0"} 0
counter moq_relay_uring_sq_full_enters_total{worker="0"} 0
counter moq_relay_uring_submissions_total{worker="0"} 0
counter moq_relay_uring_timers_armed_total{worker="0"} 0
counter moq_relay_uring_timers_cancelled_total{worker="0"} 0
counter moq_relay_uring_timers_fired_total{worker="0"} 0
counter moq_relay_uring_turns_total{worker="0"} 0
counter moq_relay_uring_tx_datagrams_total{worker="0"} 0
counter moq_relay_uring_tx_sends_total{worker="0"} 0
counter moq_relay_uring_tx_stalls_total{worker="0"} 0
counter moq_relay_uring_wakes_total{worker="0"} 0
gauge moq_relay_uring_timers_active{worker="0"} 0
histogram moq_relay_uring_cqes_per_enter_bucket{le="0",worker="0"} 0
histogram moq_relay_uring_cqes_per_enter_bucket{le="1",worker="0"} 0
histogram moq_relay_uring_cqes_per_enter_bucket{le="128",worker="0"} 0
histogram moq_relay_uring_cqes_per_enter_bucket{le="16",worker="0"} 0
histogram moq_relay_uring_cqes_per_enter_bucket{le="2",worker="0"} 0
histogram moq_relay_uring_cqes_per_enter_bucket{le="256",worker="0"} 0
histogram moq_relay_uring_cqes_per_enter_bucket{le="32",worker="0"} 0
histogram moq_relay_uring_cqes_per_enter_bucket{le="4",worker="0"} 0
histogram moq_relay_uring_cqes_per_enter_bucket{le="64",worker="0"} 0
histogram moq_relay_uring_cqes_per_enter_bucket{le="8",worker="0"} 0
histogram moq_relay_uring_cqes_per_enter_bucket{le="inf",worker="0"} 0
histogram moq_relay_uring_cqes_per_enter_count{worker="0"} 0
histogram moq_relay_uring_cqes_per_enter_sum{worker="0"} 0
histogram moq_relay_uring_sqes_per_enter_bucket{le="0",worker="0"} 0
histogram moq_relay_uring_sqes_per_enter_bucket{le="1",worker="0"} 0
histogram moq_relay_uring_sqes_per_enter_bucket{le="128",worker="0"} 0
histogram moq_relay_uring_sqes_per_enter_bucket{le="16",worker="0"} 0
histogram moq_relay_uring_sqes_per_enter_bucket{le="2",worker="0"} 0
histogram moq_relay_uring_sqes_per_enter_bucket{le="256",worker="0"} 0
histogram moq_relay_uring_sqes_per_enter_bucket{le="32",worker="0"} 0
histogram moq_relay_uring_sqes_per_enter_bucket{le="4",worker="0"} 0
histogram moq_relay_uring_sqes_per_enter_bucket{le="64",worker="0"} 0
histogram moq_relay_uring_sqes_per_enter_bucket{le="8",worker="0"} 0
histogram moq_relay_uring_sqes_per_enter_bucket{le="inf",worker="0"} 0
histogram moq_relay_uring_sqes_per_enter_count{worker="0"} 0
histogram moq_relay_uring_sqes_per_enter_sum{worker="0"} 0
"#;
}
