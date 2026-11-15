//! Socket convention interop through a relay, with a harness-owned drain acknowledgment.
use anyhow::{Context, ensure};
use tokio::io::AsyncBufReadExt;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
	let args: Vec<_> = std::env::args().collect();
	ensure!(
		args.len() == 5,
		"usage: interop-socket-js dial|accept URL PREFIX VERSION"
	);
	let dial = match args[1].as_str() {
		"dial" => true,
		"accept" => false,
		_ => anyhow::bail!("unknown role"),
	};
	let mut config = moq_tokio::connect::Config::default();
	config.version.push(args[4].parse().map_err(anyhow::Error::msg)?);
	config.websocket.enabled = Some(false);
	let client = config.init(Default::default())?.with_reconnect(false);
	let origin = moq_tokio::origin::spawn();
	let endpoint = moq_net::socket::Endpoint::new(&origin, args[3].as_str(), if dial { "dialer" } else { "acceptor" })?;
	if !dial {
		ensure!(endpoint.poll_accept_track(&kio::Waiter::noop()).is_pending());
	}
	let connection = client
		.clone()
		.with_publisher(&origin)
		.with_subscriber(origin.clone())
		.connect(args[2].parse::<url::Url>()?)
		.established()
		.await?;
	if !dial {
		println!("socket ready");
	}
	// Probe refusal before data sessions can generate late queries for closed halves.
	if dial {
		ensure!(
			endpoint.dial_track("acceptor", None)?.await.is_err(),
			"rejected dial connected"
		);
	} else {
		endpoint.accept_track().await?.reject();
	}
	let own: u8 = if dial { 16 } else { 32 };
	let peer: u8 = if dial { 32 } else { 16 };
	let mut stream = if dial {
		endpoint.dial_track("acceptor", None)?.await?
	} else {
		accepted(&endpoint).await?
	};
	ensure!(stream.peer() == if dial { "acceptor" } else { "dialer" });
	ensure!(stream.session().len() == 32);
	let mut reader = stream.consumer.ordered();
	for i in 0..3 {
		if dial {
			stream.producer.write_frame(None, vec![own + i])?;
		}
		let mut group = reader.next_group().await?.context("short track half")?;
		ensure!(group.sequence == u64::from(i), "wrong group sequence");
		let frame = group.read_frame().await?.context("empty track message")?;
		ensure!(frame.payload.as_ref() == [peer + i], "wrong track frame");
		ensure!(group.read_frame().await?.is_none(), "extra frame in track message");
		if !dial {
			stream.producer.write_frame(None, vec![own + i])?;
		}
	}
	let _finished = stream.producer.finish()?;
	ensure!(reader.next_group().await?.is_none(), "extra track frame");
	eprintln!("socket data verified {}", args[1]);

	let mut reset = if dial {
		endpoint.dial_track("acceptor", None)?.await?
	} else {
		accepted(&endpoint).await?
	};
	let mut reset_reader = reset.consumer.ordered();
	if dial {
		reset.producer.write_frame(None, vec![48])?;
		ensure!(reset_reader.next_group().await.is_err(), "track RST became FIN");
		eprintln!("socket reset verified dial");
	} else {
		let mut group = reset_reader.next_group().await?.context("missing reset signal")?;
		ensure!(
			group
				.read_frame()
				.await?
				.context("empty reset signal")?
				.payload
				.as_ref() == [48]
		);
		reset.producer.abort(moq_net::Error::App(6))?;
	}
	eprintln!("socket verified {}", args[1]);
	if !dial {
		let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
		ensure!(
			lines.next_line().await?.as_deref() == Some("socket drained"),
			"peer did not drain"
		);
	}
	// Held to here so the finished half and its reader outlive the drain.
	drop(reader);
	endpoint.close();
	let _ = connection.close().await;
	client.close().await;
	Ok(())
}

// An Incoming's claimed original dial can disappear before verification.
async fn accepted(endpoint: &moq_net::socket::Endpoint) -> anyhow::Result<moq_net::socket::TrackStream> {
	loop {
		let incoming = endpoint.accept_track().await?;
		if let Ok(stream) = incoming.accept(None).await {
			return Ok(stream);
		}
	}
}
