//! Socket interop peer; stdin acknowledges that the other process drained its half.
use anyhow::{Context, ensure};
use moq_net::socket::Endpoint;
use std::future::Future;
use std::task::Poll;
use tokio::io::AsyncBufReadExt;

const PAYLOADS: [&[u8]; 3] = [b"first", b"", b"last"];

async fn group(group: &mut moq_net::group::Consumer) -> anyhow::Result<()> {
	for expected in PAYLOADS {
		let frame = group.read_frame().await?.context("short group")?;
		ensure!(frame.payload.as_ref() == expected, "wrong frame");
		ensure!(frame.timestamp.scale().is_none(), "unexpected timestamp");
	}
	ensure!(group.read_frame().await?.is_none(), "extra frame");
	Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
	let args: Vec<_> = std::env::args().collect();
	ensure!(
		args.len() == 6,
		"usage: interop-socket URL PREFIX VERSION tcp|track dial|accept"
	);
	let mut config = moq_tokio::connect::Config::default();
	config.version.push(args[3].parse().map_err(anyhow::Error::msg)?);
	config.websocket.enabled = Some(false);
	let client = config.init(Default::default())?.with_reconnect(false);
	let origin = moq_tokio::origin::spawn();
	let endpoint = Endpoint::new(&origin, args[2].as_str(), "rust")?;
	let session = client
		.with_publisher(&origin)
		.with_subscriber(origin.clone())
		.connect(args[1].parse::<url::Url>()?)
		.established()
		.await?;
	let dial = args[5] == "dial";
	ensure!(dial || args[5] == "accept", "unknown role");
	match args[4].as_str() {
		"tcp" => {
			let mut stream = if dial {
				endpoint.dial_tcp("python")?.await?
			} else {
				let mut incoming = Box::pin(endpoint.accept_tcp());
				std::future::poll_fn(|cx| {
					assert!(incoming.as_mut().poll(cx).is_pending());
					Poll::Ready(())
				})
				.await;
				println!("listening");
				incoming.await?.accept().await?
			};
			ensure!(stream.peer() == "python", "wrong peer");
			for payload in PAYLOADS {
				stream
					.producer
					.write_frame(moq_net::Timestamp::unscaled(stream.producer.start), payload)?;
			}
			stream.producer.close()?;
			group(&mut stream.consumer).await?;
			println!("drained");
			ack().await?;
		}
		"track" => {
			let stream = if dial {
				endpoint.dial_track("python", None)?.await?
			} else {
				let mut incoming = Box::pin(endpoint.accept_track());
				std::future::poll_fn(|cx| {
					assert!(incoming.as_mut().poll(cx).is_pending());
					Poll::Ready(())
				})
				.await;
				println!("listening");
				incoming.await?.accept(None).await?
			};
			ensure!(stream.peer() == "python", "wrong peer");
			ensure!(stream.consumer.info().max_age.is_none(), "track has an age limit");
			let producer = stream.producer;
			let mut consumer = stream.consumer.ordered();
			// The track carries no timescale, so its groups count up from 0.
			for start in 0..3 {
				let mut group = producer.append_group(start)?;
				for payload in PAYLOADS {
					group.write_frame(moq_net::Timestamp::unscaled(start), payload)?;
				}
				group.close()?;
			}
			let _producer = producer.finish()?;
			for sequence in 0..3 {
				let mut next = consumer.next_group().await?.context("short track")?;
				ensure!(next.start == sequence, "wrong group order");
				group(&mut next).await?;
			}
			ensure!(consumer.next_group().await?.is_none(), "extra group");
			println!("drained");
			ack().await?;
		}
		_ => anyhow::bail!("unknown protocol"),
	}
	endpoint.close();
	session.close().await?;
	Ok(())
}

async fn ack() -> anyhow::Result<()> {
	let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
	ensure!(
		lines.next_line().await?.as_deref() == Some("drained"),
		"peer did not drain"
	);
	Ok(())
}
