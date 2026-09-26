//! `moq ls`: list the broadcasts live on a relay, the MoQ counterpart of the
//! relay's HTTP `/announced/<prefix>`.

use std::collections::BTreeSet;

use anyhow::Context;
use hang::moq_net::{self, announce::Event};
use tokio::io::{AsyncWrite, AsyncWriteExt};

use crate::args::MoqSide;

/// List the broadcasts live on a relay.
#[derive(usage::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct Args {
	/// Only list paths under this prefix.
	pub prefix: Option<String>,

	/// Keep running, printing `+ path` and `- path` as broadcasts come and go.
	#[usage(long)]
	pub follow: bool,

	/// Print one JSON object per line instead of the bare paths.
	#[usage(long)]
	pub json: bool,
}

/// One `--json` line.
#[derive(serde::Serialize)]
struct Line<'a> {
	path: &'a str,
	active: bool,
}

/// List what is live under the prefix `args` names and write it to stdout.
pub async fn run(moq: MoqSide, args: Args, net: crate::Net) -> anyhow::Result<()> {
	list(&moq, &args, &net, &mut tokio::io::stdout()).await
}

async fn list(moq: &MoqSide, args: &Args, net: &crate::Net, out: &mut (impl AsyncWrite + Unpin)) -> anyhow::Result<()> {
	let url = moq
		.client
		.url
		.clone()
		.context("`ls` dials a relay: pass --connect <url>")?;
	let prefix = args.prefix.as_deref().unwrap_or_default();

	// Scoped to the prefix, so the session only asks the relay for what is listed.
	let pattern = moq_net::Pattern::subtree(prefix).with_context(|| format!("invalid prefix `{prefix}`"))?;
	let origin = moq_tokio::origin::spawn()
		.scope("", &moq_net::Patterns::from(pattern))
		.with_context(|| format!("failed to scope to `{prefix}`"))?;

	// Subscribe-only: this session reads and never publishes.
	let client = net
		.client(moq.client.clone())?
		.with_subscriber(origin.clone())
		.with_reconnect(false);
	let connection = client.connect(url).established().await.context("failed to connect")?;

	// Registered once the session has started, so `Live` waits for the relay's initial set.
	let mut announced = origin.consume().announced();
	let mut live = BTreeSet::new();

	loop {
		// Checked first: a closing session retracts every route, which is not news.
		let event = tokio::select! {
			biased;
			closed = connection.closed() => {
				closed?;
				anyhow::bail!("connection closed");
			}
			event = announced.next() => event.context("announcements ended")?,
		};

		let (announce, active) = match event {
			Event::Announced(announce) => (announce, true),
			Event::Retracted(announce) => (announce, false),
			// A new route for a path already live changes nothing listed.
			Event::Updated(_) => continue,
			Event::Live if args.follow => continue,
			Event::Live => break,
		};

		let path = announce.prefix.as_str();
		if args.follow {
			let line = match args.json {
				true => json(path, active)?,
				false => format!("{} {path}", if active { '+' } else { '-' }),
			};
			out.write_all(format!("{line}\n").as_bytes()).await?;
			out.flush().await?;
		} else if active {
			live.insert(path.to_owned());
		} else {
			live.remove(path);
		}
	}

	for path in &live {
		let line = match args.json {
			true => json(path, true)?,
			false => path.clone(),
		};
		out.write_all(format!("{line}\n").as_bytes()).await?;
	}
	out.flush().await?;
	Ok(())
}

fn json(path: &str, active: bool) -> serde_json::Result<String> {
	serde_json::to_string(&Line { path, active })
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::args::{Command, Invocation};
	use crate::test_env::EnvGuard;
	use std::time::Duration;
	use tokio::io::{AsyncBufReadExt, BufReader, DuplexStream, Lines};

	const TIMEOUT: Duration = Duration::from_secs(10);
	const ENV: &[&str] = &["MOQ_CONNECT", "MOQ_HOP", "MOQ_BROADCAST"];

	/// A running relay holding `demo/a`, `demo/b`, and `other/c` from one publisher.
	struct Fixture {
		/// `--connect` and the TLS pin for the relay's generated certificate.
		connect: [String; 4],
		/// Stops the relay, which ends every session on it.
		shutdown: moq_relay::shutdown::Trigger,
		/// The published broadcasts by path; dropping one retracts it.
		broadcasts: Vec<(&'static str, moq_net::broadcast::Producer)>,
		_publisher: moq_tokio::Connection,
	}

	impl Fixture {
		async fn new() -> Self {
			let _ = moq_tokio::crypto::install_default();
			let fixture = moq_relay::test_relay().await.expect("test relay");
			let ready = fixture.relay.ready();
			let shutdown = fixture.relay.shutdown_trigger().clone();
			let relay = fixture.relay.cluster().origin.consume();
			tokio::spawn(fixture.relay.run());
			ready.wait().await.expect("relay ready");

			let connect = [
				"--connect".to_string(),
				fixture.url.to_string(),
				"--connect-tls-fingerprint".to_string(),
				fixture.fingerprint.clone(),
			];

			let origin = moq_tokio::origin::spawn();
			let broadcasts = ["demo/a", "demo/b", "other/c"]
				.into_iter()
				.map(|path| {
					let broadcast = origin.create_broadcast(path).expect("broadcast");
					broadcast.announce(Default::default()).expect("announce");
					(path, broadcast)
				})
				.collect::<Vec<_>>();

			let (moq, _) = parse(&connect, &[]);
			let publisher = net()
				.client(moq.client.clone())
				.expect("client")
				.with_publisher(origin.consume())
				.with_reconnect(false)
				.connect(fixture.url.clone())
				.established()
				.await
				.expect("publisher connects");

			// Every broadcast is on the relay before anything lists it.
			for (path, _) in &broadcasts {
				tokio::time::timeout(TIMEOUT, relay.routed_broadcast(*path))
					.await
					.expect("relay routes the broadcast")
					.expect("broadcast");
			}

			Self {
				connect,
				shutdown,
				broadcasts,
				_publisher: publisher,
			}
		}

		/// Run `moq <connect> ls <args>` to completion, returning what it wrote.
		async fn ls(&self, args: &[&str]) -> String {
			let (moq, args) = parse(&self.connect, args);
			let mut out = Vec::new();
			tokio::time::timeout(TIMEOUT, list(&moq, &args, &net(), &mut out))
				.await
				.expect("ls exits once caught up")
				.expect("ls");
			String::from_utf8(out).expect("UTF-8")
		}

		/// Start `moq <connect> ls --follow <args>`, returning its lines as they come
		/// and the running task.
		fn follow(
			&self,
			args: &[&str],
		) -> (
			Lines<BufReader<DuplexStream>>,
			tokio::task::JoinHandle<anyhow::Result<()>>,
		) {
			let args = ["--follow"].iter().chain(args).copied().collect::<Vec<_>>();
			let (moq, args) = parse(&self.connect, &args);
			let (mut writer, reader) = tokio::io::duplex(4096);
			let task = tokio::spawn(async move { list(&moq, &args, &net(), &mut writer).await });
			(BufReader::new(reader).lines(), task)
		}

		/// Unpublish `path`.
		fn retract(&mut self, path: &str) {
			self.broadcasts.retain(|(held, _)| *held != path);
		}
	}

	/// The next line `--follow` prints.
	async fn next(lines: &mut Lines<BufReader<DuplexStream>>) -> String {
		tokio::time::timeout(TIMEOUT, lines.next_line())
			.await
			.expect("a line in time")
			.expect("read")
			.expect("a line")
	}

	/// Parse an ls invocation the way `main` does.
	fn parse(connect: &[String], args: &[&str]) -> (MoqSide, Args) {
		let argv = ["moq"]
			.into_iter()
			.chain(connect.iter().map(String::as_str))
			.chain(["ls"])
			.chain(args.iter().copied());
		let mut cli = Invocation::try_parse_from(argv).expect("parse");
		cli.dial_only("ls", &[]).expect("only the dial");
		match cli.stages.remove(0) {
			Command::Ls(args) => (cli.moq, args),
			_ => unreachable!("parsed an ls"),
		}
	}

	fn net() -> crate::Net {
		crate::Net {
			quic: Default::default(),
			#[cfg(feature = "iroh")]
			iroh: None,
		}
	}

	/// Exactly the announced set under the prefix, one path per line.
	#[tokio::test]
	async fn prints_the_live_set_and_exits() {
		let _env = EnvGuard::clear(ENV);
		let fixture = Fixture::new().await;

		assert_eq!(fixture.ls(&[]).await, "demo/a\ndemo/b\nother/c\n");
		assert_eq!(fixture.ls(&["demo"]).await, "demo/a\ndemo/b\n");
		assert_eq!(fixture.ls(&["nothing"]).await, "");
	}

	/// The initial replay as `+` lines, then a `-` when a publisher leaves.
	#[tokio::test]
	async fn follow_prints_the_replay_then_changes() {
		let _env = EnvGuard::clear(ENV);
		let mut fixture = Fixture::new().await;

		let (mut lines, task) = fixture.follow(&["demo"]);
		let mut replay = vec![next(&mut lines).await, next(&mut lines).await];
		replay.sort();
		assert_eq!(replay, ["+ demo/a", "+ demo/b"]);

		fixture.retract("demo/a");
		assert_eq!(next(&mut lines).await, "- demo/a");
		assert!(!task.is_finished(), "follow runs until interrupted");
		task.abort();
	}

	/// Each `--json` line is `{"path", "active"}`, in either mode.
	#[tokio::test]
	async fn json_lines_parse() {
		let _env = EnvGuard::clear(ENV);
		let mut fixture = Fixture::new().await;

		let lines = fixture.ls(&["demo", "--json"]).await;
		let lines: Vec<serde_json::Value> = lines
			.lines()
			.map(|line| serde_json::from_str(line).expect("a JSON line"))
			.collect();
		assert_eq!(
			lines,
			[
				serde_json::json!({"path": "demo/a", "active": true}),
				serde_json::json!({"path": "demo/b", "active": true}),
			]
		);

		let (mut lines, task) = fixture.follow(&["other", "--json"]);
		let line: serde_json::Value = serde_json::from_str(&next(&mut lines).await).expect("a JSON line");
		assert_eq!(line, serde_json::json!({"path": "other/c", "active": true}));
		fixture.retract("other/c");
		let line: serde_json::Value = serde_json::from_str(&next(&mut lines).await).expect("a JSON line");
		assert_eq!(line, serde_json::json!({"path": "other/c", "active": false}));
		task.abort();
	}

	/// `--follow` fails once the relay goes away rather than waiting forever.
	#[tokio::test]
	async fn follow_fails_when_the_session_ends() {
		let _env = EnvGuard::clear(ENV);
		let fixture = Fixture::new().await;

		let (mut lines, task) = fixture.follow(&["other"]);
		assert_eq!(next(&mut lines).await, "+ other/c");

		fixture.shutdown.start();
		let result = tokio::time::timeout(TIMEOUT, task)
			.await
			.expect("follow exits")
			.expect("task");
		result.expect_err("a lost session is an error");
	}

	/// `ls` lists a prefix, so a `--broadcast` is refused rather than ignored.
	#[test]
	fn only_connect_is_accepted() {
		let _env = EnvGuard::clear(ENV);
		let cli = Invocation::try_parse_from(["moq", "--connect", "http://relay", "--broadcast", "demo", "ls"])
			.expect("parse");
		let err = cli.dial_only("ls", &[]).unwrap_err().to_string();
		assert!(err.contains("--broadcast"), "{err}");
	}
}
