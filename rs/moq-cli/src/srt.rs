//! SRT endpoints. Like RTMP, listeners are directional: an import listener
//! accepts publishes only, an export listener serves requests only.

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::Context;
use hang::moq_net;
use moq_srt::{Reject, Request, Server};
use moq_tokio::RedactedUrl;
use url::Url;

use crate::args::TsProgram;
use crate::moq::{ImportTarget, notify_ready};

/// SRT endpoint args: exactly one of `--connect` (dial) / `--listen` (bind).
#[derive(usage::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[usage(group("srt-mode", required))]
pub struct Args {
	/// Dial `srt://host:port?streamid=...`.
	#[usage(name = "srt-connect", long = "connect", value_name = "URL", group = "srt-mode")]
	pub connect: Option<Url>,

	/// Bind an SRT listener, bridging the single `--broadcast` (the SRT stream id
	/// is accepted but not used for routing).
	#[usage(name = "srt-listen", long = "listen", value_name = "ADDR", group = "srt-mode")]
	pub listen: Option<SocketAddr>,

	/// SRT receive latency: the buffering delay traded for loss-recovery headroom.
	#[usage(long, default = "500ms")]
	pub latency: crate::duration::Duration,
}

/// SRT import args: the endpoint, plus which programs of a multiplex to publish.
#[derive(usage::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct ImportArgs {
	#[usage(flatten)]
	pub endpoint: Args,

	/// Import one program of a multi-program feed, by its PAT program number, or `all` to
	/// publish each program as its own broadcast (`event.hang` becomes `event/1.hang`,
	/// `event/2.hang`, ...). Without it, a feed carrying more than one program is refused.
	#[usage(long)]
	pub program: Option<TsProgram>,
}

impl ImportArgs {
	/// The library's selection for `--program`.
	pub fn program(&self) -> Option<moq_srt::Program> {
		self.program.map(|program| match program {
			TsProgram::One(program) => moq_srt::Program::One(program),
			TsProgram::All => moq_srt::Program::All,
		})
	}
}

/// Point a multi-program refusal at the flag that resolves it.
fn suggest_program(err: moq_srt::Error) -> anyhow::Error {
	let multiplex = matches!(&err, moq_srt::Error::Mux(moq_mux::Error::Other(inner))
		if inner.is::<moq_mux::container::ts::MultipleProgramsError>());
	let err = anyhow::Error::from(err);
	if multiplex {
		err.context("choose one with `--program <n>`, or publish each with `--program all`")
	} else {
		err
	}
}

/// Accept incoming SRT publishes into the Origin as `target.name`; reject requests (import).
pub async fn listen_import(
	target: ImportTarget,
	addr: SocketAddr,
	latency: Duration,
	program: Option<moq_srt::Program>,
) -> anyhow::Result<()> {
	let ImportTarget {
		origin,
		name,
		max_age,
		bandwidth,
	} = target;
	let mut server = Server::bind(addr, latency).await?;
	tracing::info!(%addr, %name, "SRT listening (import)");
	notify_ready();

	while let Some(request) = server.accept().await {
		match request {
			Request::Publish(publish) => {
				let origin = origin.clone();
				let name = name.clone();
				let bandwidth = bandwidth.clone();
				tokio::spawn(async move {
					if let Err(err) = publish
						.with_max_age(max_age)
						.with_bandwidth(bandwidth)
						.with_program(program)
						.accept(&origin, &name)
						.await
					{
						let err = suggest_program(err);
						tracing::warn!(%name, err = format!("{err:#}"), "SRT ingest ended with error");
					}
				});
			}
			Request::Subscribe(subscribe) => {
				tokio::spawn(async move {
					let _ = subscribe.reject(Reject::Forbidden).await;
				});
			}
			_ => {}
		}
	}

	Ok(())
}

/// Serve SRT requests for `name` from the Origin; reject publishes (export).
pub async fn listen_export(
	origin: moq_net::origin::Consumer,
	addr: SocketAddr,
	name: String,
	latency: Duration,
) -> anyhow::Result<()> {
	let mut server = Server::bind(addr, latency).await?;
	tracing::info!(%addr, %name, "SRT listening (export)");
	notify_ready();

	while let Some(request) = server.accept().await {
		match request {
			Request::Subscribe(subscribe) => {
				let origin = origin.clone();
				let name = name.clone();
				tokio::spawn(async move {
					if let Err(err) = subscribe.accept(&origin, &name).await {
						tracing::warn!(%name, %err, "SRT request ended with error");
					}
				});
			}
			Request::Publish(publish) => {
				tokio::spawn(async move {
					let _ = publish.reject(Reject::Forbidden).await;
				});
			}
			_ => {}
		}
	}

	Ok(())
}

/// Dial a remote SRT server and pull its stream into the Origin under `target.name` (import).
pub async fn connect_import(
	target: ImportTarget,
	url: Url,
	latency: Duration,
	program: Option<moq_srt::Program>,
) -> anyhow::Result<()> {
	let (addr, resource) = parse_url(&url).await?;
	let name = &target.name;
	tracing::info!(url = %RedactedUrl::new(&url), %name, "SRT client pulling");
	notify_ready();

	let client = moq_srt::Client::new(addr, resource)
		.with_latency(latency)
		.with_max_age(target.max_age)
		.with_bandwidth(target.bandwidth)
		.with_program(program);
	client.pull(&target.origin, name).await.map_err(suggest_program)
}

/// Push a broadcast from the Origin to a remote SRT server (export).
pub async fn connect_export(
	origin: moq_net::origin::Consumer,
	url: Url,
	name: String,
	latency: Duration,
) -> anyhow::Result<()> {
	let (addr, resource) = parse_url(&url).await?;
	tracing::info!(url = %RedactedUrl::new(&url), %name, "SRT client pushing");
	notify_ready();

	let client = moq_srt::Client::new(addr, resource).with_latency(latency);
	Ok(client.publish(&origin, &name).await?)
}

/// Parse `srt://host:port?streamid=<resource>` into a resolved address and resource.
/// The resource falls back to the URL path when `streamid` is absent.
async fn parse_url(url: &Url) -> anyhow::Result<(SocketAddr, String)> {
	anyhow::ensure!(url.scheme() == "srt", "srt url must use the srt scheme: {url}");

	let host = url.host_str().with_context(|| format!("srt url missing host: {url}"))?;
	let port = url.port().context("srt url must include a port: srt://host:port")?;
	let addr = tokio::net::lookup_host((host, port))
		.await?
		.next()
		.with_context(|| format!("could not resolve {host}:{port}"))?;

	let resource = url
		.query_pairs()
		.find(|(key, _)| key == "streamid")
		.map(|(_, value)| value.into_owned())
		.unwrap_or_else(|| url.path().trim_matches('/').to_string());
	anyhow::ensure!(!resource.is_empty(), "srt url must include a streamid or path");

	Ok((addr, resource))
}

#[cfg(test)]
mod tests {
	use super::*;

	// Numeric hosts resolve without touching DNS, so these stay offline.
	async fn parse(url: &str) -> anyhow::Result<(SocketAddr, String)> {
		parse_url(&Url::parse(url).unwrap()).await
	}

	#[tokio::test]
	async fn resource_from_streamid() {
		let (addr, resource) = parse("srt://127.0.0.1:9000?streamid=live/cam").await.unwrap();
		assert_eq!(addr.port(), 9000);
		assert_eq!(resource, "live/cam");
	}

	#[tokio::test]
	async fn resource_from_path() {
		let (_, resource) = parse("srt://127.0.0.1:9000/live/cam").await.unwrap();
		assert_eq!(resource, "live/cam");
	}

	#[tokio::test]
	async fn rejects_non_srt_scheme() {
		assert!(parse("udp://127.0.0.1:9000").await.is_err());
	}

	/// A multiplex refusal names the flag that resolves it; other errors pass through unchanged.
	#[test]
	fn a_multiplex_suggests_the_program_flag() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
		let mut import = moq_mux::container::ts::Import::new(broadcast, catalog.reserve());
		let refused = import.decode(&crate::publish::tests::two_programs()).unwrap_err();
		let err = suggest_program(moq_srt::Error::Mux(refused.into()));
		let err = format!("{err:#}");
		assert!(err.contains("--program") && err.contains("programs (1, 2)"), "{err}");

		let err = format!("{:#}", suggest_program(moq_srt::Error::ListenerClosed));
		assert!(!err.contains("--program"), "{err}");
	}

	#[tokio::test]
	async fn requires_port_and_resource() {
		assert!(parse("srt://127.0.0.1").await.is_err());
		assert!(parse("srt://127.0.0.1:9000").await.is_err());
	}
}
