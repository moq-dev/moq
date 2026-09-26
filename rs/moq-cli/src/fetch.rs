//! `moq fetch`: write one group of a track to stdout, the MoQ counterpart of the
//! relay's HTTP `/fetch/<broadcast>/<track>?group=N`.

use std::time::Duration;

use anyhow::Context;
use base64::Engine;
use hang::moq_net;
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::time::{Instant, timeout_at};

use crate::args::MoqSide;

/// How long discovery, lookup, and every frame read may take in total, matching
/// the relay's HTTP `/fetch`.
const TIMEOUT: Duration = Duration::from_secs(30);

/// Read one group of a track and write it to stdout.
#[derive(usage::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct Args {
	/// The track name, taken literally: a `/` in it is part of the name.
	pub track: String,

	/// The group sequence to read; the newest group when omitted.
	#[usage(long)]
	pub group: Option<u64>,

	/// Print one JSON object per frame instead of the concatenated payloads.
	#[usage(long)]
	pub json: bool,
}

/// One frame of `--json` output.
#[derive(serde::Serialize)]
struct Frame {
	group: u64,
	frame: u64,
	size: u64,
	/// Padded standard base64.
	payload: String,
}

/// Fetch the group `args` names from the broadcast `--broadcast` names and write it to stdout.
pub async fn run(moq: MoqSide, args: Args, net: crate::Net) -> anyhow::Result<()> {
	let mut stdout = tokio::io::stdout();
	fetch(&moq, &args, &net, Instant::now() + TIMEOUT, &mut stdout).await
}

async fn fetch(
	moq: &MoqSide,
	args: &Args,
	net: &crate::Net,
	deadline: Instant,
	out: &mut (impl AsyncWrite + Unpin),
) -> anyhow::Result<()> {
	let url = moq
		.client
		.url
		.clone()
		.context("`fetch` dials a relay: pass --connect <url>")?;
	let broadcast = moq.broadcast.clone().unwrap_or_default();

	// Scoped to the broadcast, so the relay announces it by name even when a hidden
	// segment such as `.stats` would keep it out of an unscoped listing.
	let pattern =
		moq_net::Pattern::subtree(&broadcast).with_context(|| format!("invalid broadcast `{broadcast}`"))?;
	let origin = moq_tokio::origin::spawn()
		.scope("", &moq_net::Patterns::from(pattern))
		.with_context(|| format!("failed to scope to `{broadcast}`"))?;

	// Subscribe-only: this session reads and never publishes.
	let client = net
		.client(moq.client.clone())?
		.with_subscriber(origin.clone())
		.with_reconnect(false);

	let result = timeout_at(deadline, async {
		// Held until the last frame is read; dropping it closes the session.
		let _connection = client.connect(url).established().await.context("failed to connect")?;

		// Wait for a covering route rather than asking on the spot: the announcement
		// is still in flight right after connecting.
		let broadcast = origin
			.consume()
			.routed_broadcast(broadcast.as_str())
			.await
			.with_context(|| format!("broadcast `{}` not found", crate::display_name(&broadcast)))?;
		let track = broadcast.track(&args.track)?;
		let mut group = moq_relay::fetch_group(&track, args.group)
			.await
			.with_context(|| match args.group {
				Some(sequence) => format!("group {sequence} of `{}` not found", args.track),
				None => format!("no group of `{}` found", args.track),
			})?;

		let sequence = group.sequence;
		let mut index = 0;
		while let Some(frame) = group
			.read_frame()
			.await
			.with_context(|| format!("failed to read group {sequence} of `{}`", args.track))?
		{
			match args.json {
				true => {
					let mut line = serde_json::to_vec(&Frame {
						group: sequence,
						frame: index,
						size: frame.payload.len() as u64,
						payload: base64::engine::general_purpose::STANDARD.encode(&frame.payload),
					})?;
					line.push(b'\n');
					out.write_all(&line).await?;
				}
				false => out.write_all(&frame.payload).await?,
			}
			index += 1;
		}
		out.flush().await?;
		anyhow::Ok(())
	})
	.await;

	result.unwrap_or_else(|_| anyhow::bail!("fetch timed out"))
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::args::{Command, Invocation};
	use crate::test_env::EnvGuard;

	/// The frames of each finished group on the `data` track.
	fn frames(sequence: u64) -> [Vec<u8>; 2] {
		[
			format!("g{sequence}f0").into_bytes(),
			format!("g{sequence}f1").into_bytes(),
		]
	}

	/// A running relay with a publisher attached, and the flags that reach it.
	struct Fixture {
		/// `--connect` and the TLS pin for the relay's generated certificate.
		connect: [String; 4],
		/// The relay's HTTP listener, for comparing against `/fetch`.
		http: std::net::SocketAddr,
		/// Kept so the published broadcast, its tracks, and the open group stay live.
		_publisher: (
			moq_tokio::Connection,
			Vec<moq_net::broadcast::Producer>,
			Vec<moq_net::track::Producer>,
			moq_net::group::Producer,
		),
	}

	impl Fixture {
		/// Publish `demo` with three finished groups on `data`, no group on `empty`,
		/// and one frame of a group that never finishes on `live`.
		async fn new() -> Self {
			let _ = moq_tokio::crypto::install_default();
			let fixture = moq_relay::test_relay().await.expect("test relay");
			// A dot-prefixed broadcast on the relay itself, like its `.stats`, which
			// announce listings hide unless asked for by name.
			let hidden = fixture
				.relay
				.cluster()
				.origin
				.create_broadcast(".hidden")
				.expect("hidden broadcast");
			hidden.announce(Default::default()).expect("announce hidden");
			let secret = hidden.create_track("data", None).expect("hidden track");
			let mut group = secret.append_group().expect("group");
			group.write_frame(moq_net::Timestamp::ZERO, b"secret".as_ref())
				.expect("frame");
			group.finish().expect("finish");

			let ready = fixture.relay.ready();
			tokio::spawn(fixture.relay.run());
			ready.wait().await.expect("relay ready");

			let connect = [
				"--connect".to_string(),
				fixture.url.to_string(),
				"--connect-tls-fingerprint".to_string(),
				fixture.fingerprint.clone(),
			];

			let origin = moq_tokio::origin::spawn();
			let broadcast = origin.create_broadcast("demo").expect("broadcast");
			broadcast.announce(Default::default()).expect("announce");

			let data = broadcast.create_track("data", None).expect("data track");
			for sequence in 0..3 {
				let mut group = data.append_group().expect("group");
				for frame in frames(sequence) {
					group.write_frame(moq_net::Timestamp::ZERO, frame).expect("frame");
				}
				group.finish().expect("finish");
			}
			let empty = broadcast.create_track("empty", None).expect("empty track");
			let live = broadcast.create_track("live", None).expect("live track");
			let mut open = live.append_group().expect("open group");
			open.write_frame(moq_net::Timestamp::ZERO, b"first".as_ref())
				.expect("frame");

			let (moq, _) = parse(&connect, "demo", &[]);
			let connection = net()
				.client(moq.client.clone())
				.expect("client")
				.with_publisher(origin.consume())
				.with_reconnect(false)
				.connect(fixture.url.clone())
				.established()
				.await
				.expect("publisher connects");

			Self {
				connect,
				http: fixture.http,
				_publisher: (connection, vec![broadcast, hidden], vec![data, empty, live, secret], open),
			}
		}

		/// Run `moq <connect> --broadcast demo fetch <args>` with `timeout`, returning
		/// the outcome and what it wrote.
		async fn fetch(&self, args: &[&str], timeout: Duration) -> (anyhow::Result<()>, Vec<u8>) {
			self.fetch_from("demo", args, timeout).await
		}

		/// [`Self::fetch`] from another broadcast.
		async fn fetch_from(&self, broadcast: &str, args: &[&str], timeout: Duration) -> (anyhow::Result<()>, Vec<u8>) {
			let (moq, args) = parse(&self.connect, broadcast, args);
			let mut out = Vec::new();
			let result = super::fetch(&moq, &args, &net(), Instant::now() + timeout, &mut out).await;
			(result, out)
		}

		/// The relay's HTTP `/fetch` status and body for the same group.
		async fn curl(&self, query: &str) -> (u16, Vec<u8>) {
			let response = reqwest::get(format!("http://{}/fetch/demo/data{query}", self.http))
				.await
				.expect("HTTP fetch");
			let status = response.status().as_u16();
			(status, response.bytes().await.expect("HTTP body").to_vec())
		}
	}

	/// Parse a fetch invocation the way `main` does.
	fn parse(connect: &[String], broadcast: &str, args: &[&str]) -> (MoqSide, Args) {
		let argv = ["moq"]
			.into_iter()
			.chain(connect.iter().map(String::as_str))
			.chain(["--broadcast", broadcast, "fetch"])
			.chain(args.iter().copied())
			.chain(args.is_empty().then_some("data"));
		let mut cli = Invocation::try_parse_from(argv).expect("parse");
		cli.dial_only("fetch", &["--broadcast"]).expect("only the dial");
		match cli.stages.remove(0) {
			Command::Fetch(args) => (cli.moq, args),
			_ => unreachable!("parsed a fetch"),
		}
	}

	fn net() -> crate::Net {
		crate::Net {
			quic: Default::default(),
			#[cfg(feature = "iroh")]
			iroh: None,
		}
	}

	const ENV: &[&str] = &["MOQ_CONNECT", "MOQ_HOP"];

	/// A known sequence prints its frames back to back, the same bytes as `/fetch`.
	#[tokio::test]
	async fn a_sequence_prints_its_exact_bytes() {
		let _env = EnvGuard::clear(ENV);
		let fixture = Fixture::new().await;

		let (result, out) = fixture.fetch(&["data", "--group", "1"], TIMEOUT).await;
		result.expect("fetch");
		assert_eq!(out, frames(1).concat());
		assert_eq!(fixture.curl("?group=1").await, (200, out));
	}

	/// No `--group` reads the newest group, as `/fetch` does by default.
	#[tokio::test]
	async fn the_default_is_the_newest_group() {
		let _env = EnvGuard::clear(ENV);
		let fixture = Fixture::new().await;

		let (result, out) = fixture.fetch(&["data"], TIMEOUT).await;
		result.expect("fetch");
		assert_eq!(out, frames(2).concat());
		assert_eq!(fixture.curl("").await, (200, out));
	}

	/// A hidden broadcast such as `.stats` is fetched by name, as `/fetch` serves it.
	#[tokio::test]
	async fn a_hidden_broadcast_is_fetched_by_name() {
		let _env = EnvGuard::clear(ENV);
		let fixture = Fixture::new().await;

		let (result, out) = fixture.fetch_from(".hidden", &["data"], Duration::from_secs(5)).await;
		result.expect("fetch");
		assert_eq!(out, b"secret");
	}

	/// A missing sequence fails the lookup itself, before any output, as `/fetch`
	/// answers 404 rather than starting a body.
	#[tokio::test]
	async fn a_missing_sequence_fails() {
		let _env = EnvGuard::clear(ENV);
		let fixture = Fixture::new().await;

		let (result, out) = fixture.fetch(&["data", "--group", "99"], TIMEOUT).await;
		let err = result.expect_err("group 99 does not exist");
		assert_eq!(err.to_string(), "group 99 of `data` not found", "{err:#}");
		assert!(out.is_empty());
		assert_eq!(fixture.curl("?group=99").await, (404, Vec::new()));
	}

	/// A track with no group never resolves "newest", so the deadline ends it.
	#[tokio::test]
	async fn a_lookup_times_out() {
		let _env = EnvGuard::clear(ENV);
		let fixture = Fixture::new().await;

		let (result, out) = fixture.fetch(&["empty"], Duration::from_millis(500)).await;
		let err = result.expect_err("no group ever arrives");
		assert!(err.to_string().contains("timed out"), "{err:#}");
		assert!(out.is_empty());
	}

	/// A group that never finishes is cut off by the same deadline, after the frames
	/// that did arrive.
	#[tokio::test]
	async fn a_frame_read_times_out() {
		let _env = EnvGuard::clear(ENV);
		let fixture = Fixture::new().await;

		let (result, out) = fixture
			.fetch(&["live", "--group", "0"], Duration::from_millis(500))
			.await;
		let err = result.expect_err("the group never finishes");
		assert!(err.to_string().contains("timed out"), "{err:#}");
		assert_eq!(out, b"first");
	}

	/// Each `--json` line is the whole record and decodes to the frame's bytes.
	#[tokio::test]
	async fn json_lines_decode_to_the_frames() {
		let _env = EnvGuard::clear(ENV);
		let fixture = Fixture::new().await;

		let (result, out) = fixture.fetch(&["data", "--group", "1", "--json"], TIMEOUT).await;
		result.expect("fetch");

		let lines: Vec<serde_json::Value> = out
			.split(|&byte| byte == b'\n')
			.filter(|line| !line.is_empty())
			.map(|line| serde_json::from_slice(line).expect("a JSON line"))
			.collect();
		assert_eq!(lines.len(), 2);
		for (index, (line, frame)) in lines.iter().zip(frames(1)).enumerate() {
			let payload = base64::engine::general_purpose::STANDARD
				.decode(line["payload"].as_str().expect("payload"))
				.expect("base64");
			assert_eq!(
				*line,
				serde_json::json!({
					"group": 1,
					"frame": index,
					"size": frame.len(),
					"payload": line["payload"],
				})
			);
			assert_eq!(payload, frame);
		}
	}

	/// A listener or cluster flag is refused rather than silently never served.
	#[test]
	fn only_the_dial_is_accepted() {
		let _env = EnvGuard::clear(ENV);
		let cli = Invocation::try_parse_from([
			"moq",
			"--connect",
			"http://relay",
			"--listen-tcp-bind",
			"127.0.0.1:0",
			"fetch",
			"data",
		])
		.expect("parse");
		let err = cli.dial_only("fetch", &["--broadcast"]).unwrap_err().to_string();
		assert!(err.contains("--listen-tcp-bind"), "{err}");
	}
}
