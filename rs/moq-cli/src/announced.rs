//! `moq announced`: follow the broadcasts announced on a relay, the MoQ
//! counterpart of the relay's HTTP `/announced/<prefix>`.

use std::collections::BTreeSet;
use std::io::IsTerminal;

use anyhow::Context;
use hang::moq_net::{self, announce::Event};
use tokio::io::{AsyncWrite, AsyncWriteExt};

use crate::args::MoqSide;

/// Follow the broadcasts announced on a relay as they start and end.
#[derive(usage::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct Args {
	/// Only follow paths under this prefix.
	pub prefix: Option<String>,

	/// Print each start and end as a line of JSON.
	#[usage(long)]
	pub json: bool,
}

/// One `--json` line.
#[derive(serde::Serialize)]
struct Line<'a> {
	path: &'a str,
	active: bool,
}

/// How each start and end is written.
enum Output {
	/// Redraw the announced set in place, for a person watching a terminal.
	View(View),
	/// `+ path` / `- path`, one line per event.
	Lines,
	/// One [`Line`] per event.
	Json,
}

/// Follow what is announced under the prefix `args` names, writing to stdout until
/// the session ends.
pub async fn run(moq: MoqSide, args: Args, net: crate::Net) -> anyhow::Result<()> {
	let output = match args.json {
		true => Output::Json,
		false if std::io::stdout().is_terminal() => Output::View(View::default()),
		false => Output::Lines,
	};
	follow(&moq, args.prefix.as_deref(), &net, output, &mut tokio::io::stdout()).await
}

async fn follow(
	moq: &MoqSide,
	prefix: Option<&str>,
	net: &crate::Net,
	mut output: Output,
	out: &mut (impl AsyncWrite + Unpin),
) -> anyhow::Result<()> {
	let url = moq
		.client
		.url
		.clone()
		.context("`announced` dials a relay: pass --connect <url>")?;
	let prefix = prefix.unwrap_or_default();

	// Scoped to the prefix, so the session only asks the relay for what is followed.
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

	let mut announced = origin.consume().announced();

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
			Event::Start(announce) => (announce, true),
			Event::End(announce) => (announce, false),
			// An `Update` is a new route for a path already announced, which changes
			// nothing shown; nothing else names a path.
			_ => continue,
		};

		let path = announce.prefix.as_str();
		let text = match &mut output {
			Output::View(view) => match view.apply(path, active) {
				Some(frame) => frame,
				None => continue,
			},
			Output::Lines => format!("{} {path}\n", if active { '+' } else { '-' }),
			Output::Json => format!("{}\n", serde_json::to_string(&Line { path, active })?),
		};
		out.write_all(text.as_bytes()).await?;
		out.flush().await?;
	}
}

/// The announced set, drawn below the cursor and redrawn in place on each change.
#[derive(Default)]
struct View {
	announced: BTreeSet<String>,
	/// Lines the last frame drew, which the next one moves back over.
	drawn: usize,
}

impl View {
	/// Apply one start or end, returning the frame that redraws the set when it changed.
	///
	/// Autowrap is off while drawing, so a path wider than the terminal is cut off
	/// rather than wrapped onto a line the next frame would not move back over.
	fn apply(&mut self, path: &str, active: bool) -> Option<String> {
		let changed = match active {
			true => self.announced.insert(path.to_owned()),
			false => self.announced.remove(path),
		};
		if !changed {
			return None;
		}

		let mut frame = String::from("\x1b[?7l");
		if self.drawn > 0 {
			// To the start of the first line drawn, then erase everything below it.
			frame.push_str(&format!("\x1b[{}F\x1b[J", self.drawn));
		}
		for path in &self.announced {
			frame.push_str(path);
			frame.push('\n');
		}
		frame.push_str("\x1b[?7h");
		self.drawn = self.announced.len();
		Some(frame)
	}
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

			// Every broadcast is on the relay before anything follows it.
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

		/// Start `moq <connect> announced <args>` as if piped, returning its lines as
		/// they come and the running task.
		fn follow(
			&self,
			args: &[&str],
		) -> (
			Lines<BufReader<DuplexStream>>,
			tokio::task::JoinHandle<anyhow::Result<()>>,
		) {
			let (moq, args) = parse(&self.connect, args);
			let output = match args.json {
				true => Output::Json,
				false => Output::Lines,
			};
			let (mut writer, reader) = tokio::io::duplex(4096);
			let task =
				tokio::spawn(async move { follow(&moq, args.prefix.as_deref(), &net(), output, &mut writer).await });
			(BufReader::new(reader).lines(), task)
		}

		/// Unpublish `path`.
		fn retract(&mut self, path: &str) {
			self.broadcasts.retain(|(held, _)| *held != path);
		}
	}

	/// The next line printed.
	async fn next(lines: &mut Lines<BufReader<DuplexStream>>) -> String {
		tokio::time::timeout(TIMEOUT, lines.next_line())
			.await
			.expect("a line in time")
			.expect("read")
			.expect("a line")
	}

	/// Parse an `announced` invocation the way `main` does.
	fn parse(connect: &[String], args: &[&str]) -> (MoqSide, Args) {
		let argv = ["moq"]
			.into_iter()
			.chain(connect.iter().map(String::as_str))
			.chain(["announced"])
			.chain(args.iter().copied());
		let mut cli = Invocation::try_parse_from(argv).expect("parse");
		cli.dial_only("announced", &[]).expect("only the dial");
		match cli.stages.remove(0) {
			Command::Announced(args) => (cli.moq, args),
			_ => unreachable!("parsed an announced"),
		}
	}

	fn net() -> crate::Net {
		crate::Net {
			quic: Default::default(),
			#[cfg(feature = "iroh")]
			iroh: None,
		}
	}

	/// What is already announced as `+` lines, then a `-` when a publisher leaves.
	#[tokio::test]
	async fn prints_the_replay_then_changes() {
		let _env = EnvGuard::clear(ENV);
		let mut fixture = Fixture::new().await;

		let (mut lines, task) = fixture.follow(&["demo"]);
		let mut replay = vec![next(&mut lines).await, next(&mut lines).await];
		replay.sort();
		assert_eq!(replay, ["+ demo/a", "+ demo/b"]);

		fixture.retract("demo/a");
		assert_eq!(next(&mut lines).await, "- demo/a");
		assert!(!task.is_finished(), "runs until interrupted");
		task.abort();
	}

	/// Each `--json` line is `{"path", "active"}`.
	#[tokio::test]
	async fn json_lines_parse() {
		let _env = EnvGuard::clear(ENV);
		let mut fixture = Fixture::new().await;

		let (mut lines, task) = fixture.follow(&["other", "--json"]);
		let line: serde_json::Value = serde_json::from_str(&next(&mut lines).await).expect("a JSON line");
		assert_eq!(line, serde_json::json!({"path": "other/c", "active": true}));
		fixture.retract("other/c");
		let line: serde_json::Value = serde_json::from_str(&next(&mut lines).await).expect("a JSON line");
		assert_eq!(line, serde_json::json!({"path": "other/c", "active": false}));
		task.abort();
	}

	/// It fails once the relay goes away rather than waiting forever.
	#[tokio::test]
	async fn fails_when_the_session_ends() {
		let _env = EnvGuard::clear(ENV);
		let fixture = Fixture::new().await;

		let (mut lines, task) = fixture.follow(&["other"]);
		assert_eq!(next(&mut lines).await, "+ other/c");

		fixture.shutdown.start();
		let result = tokio::time::timeout(TIMEOUT, task)
			.await
			.expect("exits once the session ends")
			.expect("task");
		result.expect_err("a lost session is an error");
	}

	/// Each change redraws the whole set over the previous frame; a no-op draws nothing.
	#[test]
	fn the_view_redraws_in_place() {
		let mut view = View::default();
		assert_eq!(view.apply("b", true).unwrap(), "\x1b[?7lb\n\x1b[?7h");
		assert_eq!(view.apply("a", true).unwrap(), "\x1b[?7l\x1b[1F\x1b[Ja\nb\n\x1b[?7h");
		assert_eq!(view.apply("a", true), None, "already shown");
		assert_eq!(view.apply("c", false), None, "never shown");
		assert_eq!(view.apply("b", false).unwrap(), "\x1b[?7l\x1b[2F\x1b[Ja\n\x1b[?7h");
		assert_eq!(view.apply("a", false).unwrap(), "\x1b[?7l\x1b[1F\x1b[J\x1b[?7h");
		assert_eq!(view.apply("a", true).unwrap(), "\x1b[?7la\n\x1b[?7h");
	}

	/// `announced` follows a prefix, so a `--broadcast` is refused rather than ignored.
	#[test]
	fn only_connect_is_accepted() {
		let _env = EnvGuard::clear(ENV);
		let cli = Invocation::try_parse_from(["moq", "--connect", "http://relay", "--broadcast", "demo", "announced"])
			.expect("parse");
		let err = cli.dial_only("announced", &[]).unwrap_err().to_string();
		assert!(err.contains("--broadcast"), "{err}");
	}

	/// The one-shot listing is gone, with no alias or flag left behind for it.
	#[test]
	fn ls_and_follow_are_unknown() {
		let _env = EnvGuard::clear(ENV);
		assert!(Invocation::try_parse_from(["moq", "--connect", "http://relay", "ls"]).is_err());
		assert!(Invocation::try_parse_from(["moq", "--connect", "http://relay", "announced", "--follow"]).is_err());
	}
}
