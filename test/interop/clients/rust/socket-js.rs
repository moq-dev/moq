//! Socket convention interop through a relay, with a harness-owned drain acknowledgment.
use anyhow::{Context, ensure};
use moq_tokio::moq_net;
use tokio::io::AsyncBufReadExt;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
	let args: Vec<_> = std::env::args().collect();
	ensure!(
		args.len() == 6,
		"usage: interop-socket-js dial|accept URL PREFIX tcp|track VERSION"
	);
	let dial = match args[1].as_str() {
		"dial" => true,
		"accept" => false,
		_ => anyhow::bail!("unknown role"),
	};
	let tcp = match args[4].as_str() {
		"tcp" => true,
		"track" => false,
		_ => anyhow::bail!("unknown protocol"),
	};
	let mut config = moq_tokio::connect::Config::default();
	config.version.push(args[5].parse().map_err(anyhow::Error::msg)?);
	config.websocket.enabled = Some(false);
	let client = config.init(Default::default())?.with_reconnect(false);
	let origin = moq_tokio::origin::spawn();
	let endpoint = moq_net::socket::Endpoint::new(&origin, args[3].as_str(), if dial { "dialer" } else { "acceptor" })?;
	if !dial {
		let waiter = kio::Waiter::noop();
		if tcp {
			ensure!(endpoint.poll_accept_tcp(&waiter).is_pending());
		} else {
			ensure!(endpoint.poll_accept_track(&waiter).is_pending());
		}
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
		if tcp {
			ensure!(endpoint.dial_tcp("acceptor")?.await.is_err(), "rejected dial connected");
		} else {
			ensure!(
				endpoint.dial_track("acceptor", None)?.await.is_err(),
				"rejected dial connected"
			);
		}
	} else if tcp {
		endpoint.accept_tcp().await?.reject();
	} else {
		endpoint.accept_track().await?.reject();
	}
	let own: u8 = if dial { 16 } else { 32 };
	let peer: u8 = if dial { 32 } else { 16 };
	let _hold: Box<dyn std::any::Any> = if tcp {
		let mut stream = if dial {
			endpoint.dial_tcp("acceptor")?.await?
		} else {
			accepted_tcp(&endpoint).await?
		};
		ensure!(stream.peer() == if dial { "acceptor" } else { "dialer" });
		ensure!(stream.session().len() == 32);
		for i in 0..3 {
			stream.producer.write_frame(None, vec![own + i])?;
		}
		stream.producer.finish()?;
		for i in 0..3 {
			let frame = stream.consumer.read_frame().await?.context("short TCP half")?;
			ensure!(frame.payload.as_ref() == [peer + i], "wrong TCP frame");
		}
		ensure!(stream.consumer.read_frame().await?.is_none(), "extra TCP frame");
		Box::new(stream)
	} else {
		let mut stream = if dial {
			endpoint.dial_track("acceptor", None)?.await?
		} else {
			accepted_track(&endpoint).await?
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
		stream.producer.finish()?;
		ensure!(reader.next_group().await?.is_none(), "extra track frame");
		Box::new((stream.producer, reader))
	};
	eprintln!("socket data verified {} {}", args[4], args[1]);
	if tcp {
		let mut reset = if dial {
			endpoint.dial_tcp("acceptor")?.await?
		} else {
			accepted_tcp(&endpoint).await?
		};
		if dial {
			reset.producer.write_frame(None, vec![48])?;
			ensure!(reset.consumer.read_frame().await.is_err(), "TCP RST became FIN");
			eprintln!("socket reset verified tcp dial");
		} else {
			ensure!(
				reset
					.consumer
					.read_frame()
					.await?
					.context("missing reset signal")?
					.payload
					.as_ref() == [48]
			);
			reset.producer.abort(moq_net::Error::App(6))?;
		}
	} else {
		let mut reset = if dial {
			endpoint.dial_track("acceptor", None)?.await?
		} else {
			accepted_track(&endpoint).await?
		};
		let mut reader = reset.consumer.ordered();
		if dial {
			reset.producer.write_frame(None, vec![48])?;
			ensure!(reader.next_group().await.is_err(), "track RST became FIN");
			eprintln!("socket reset verified track dial");
		} else {
			let mut group = reader.next_group().await?.context("missing reset signal")?;
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
	}
	eprintln!("socket verified {} {}", args[4], args[1]);
	if !dial {
		let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
		ensure!(
			lines.next_line().await?.as_deref() == Some("socket drained"),
			"peer did not drain"
		);
	}
	endpoint.close();
	let _ = connection.close().await;
	client.close().await;
	Ok(())
}

// An Incoming's claimed original dial can disappear before verification.
async fn accepted_tcp(endpoint: &moq_net::socket::Endpoint) -> anyhow::Result<moq_net::socket::TcpStream> {
	loop {
		let incoming = endpoint.accept_tcp().await?;
		if let Ok(stream) = incoming.accept().await {
			return Ok(stream);
		}
	}
}

async fn accepted_track(endpoint: &moq_net::socket::Endpoint) -> anyhow::Result<moq_net::socket::TrackStream> {
	loop {
		let incoming = endpoint.accept_track().await?;
		if let Ok(stream) = incoming.accept(None).await {
			return Ok(stream);
		}
	}
}
