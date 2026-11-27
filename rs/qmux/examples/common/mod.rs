//! The scenario both interop examples share; only who opens which stream differs.

use std::time::Duration;

use anyhow::{Context as _, bail};
use bytes::{Bytes, BytesMut};
use moq_net::transport::poll::{RecvStream as _, SendOptions, SendStream as _, Session as _};
use qmux::{RecvStream, SendStream, Session, Version};

pub const PROTOCOL: &str = "qmux-interop";
pub const CLIENT_SEED: usize = 17;
pub const SERVER_SEED: usize = 29;
pub const TIMEOUT: Duration = Duration::from_secs(15);
const PAYLOAD_LEN: usize = 1_200_000;

/// The payload seed, reliable prefix, and code of the stream Rust resets with RESET_STREAM_AT
const RESET_SEED: usize = 41;
const RESET_PREFIX: usize = 16 * 1024;
const RESET_CODE: u32 = 7;

pub fn parse_version(value: &str) -> anyhow::Result<Version> {
	match value {
		"qmux-00" => Ok(Version::QMux00),
		"qmux-01" => Ok(Version::QMux01),
		"qmux-02" => Ok(Version::QMux02),
		_ => bail!("unsupported QMux version: {value}"),
	}
}

/// Large enough to need both stream- and connection-level flow-control updates.
pub fn payload(seed: usize) -> Vec<u8> {
	(0..PAYLOAD_LEN)
		.map(|index| ((index * 31 + seed) % 251) as u8)
		.collect()
}

/// On QMux02, open a stream, write a prefix, and reset it with RESET_STREAM_AT keeping all of
/// it, which the peer reads before the reset. The prefix fits the peer's stream window, so the
/// reset comes while some of it may still be buffered. Only the prefix is written: bytes past it
/// may reach the peer's reader before the reset does, and the mux tests cover dropping them.
pub async fn send_reset_at(session: &Session, version: Version) -> anyhow::Result<()> {
	if version != Version::QMux02 {
		return Ok(());
	}
	let mut send = kio::clock::timeout(TIMEOUT, session.open_uni(SendOptions::default()))
		.await
		.context("timed out opening the reset stream")??;
	let data = payload(RESET_SEED);
	let mut prefix = Bytes::copy_from_slice(&data[..RESET_PREFIX]);
	while !prefix.is_empty() {
		send.write_buf(&mut prefix).await?;
	}
	send.reset_at(&send.offset(), RESET_CODE);
	Ok(())
}

/// Write all of `data` then the FIN, bounded by [`TIMEOUT`].
pub async fn send_all(send: &mut SendStream, data: &[u8], what: &str) -> anyhow::Result<()> {
	let mut data = Bytes::copy_from_slice(data);
	kio::clock::timeout(TIMEOUT, async {
		while !data.is_empty() {
			send.write_buf(&mut data).await?;
		}
		anyhow::Ok(())
	})
	.await
	.with_context(|| format!("timed out writing {what}"))??;
	send.finish()?;
	Ok(())
}

/// Read until the FIN, bounded by [`TIMEOUT`].
pub async fn recv_all(recv: &mut RecvStream, what: &str) -> anyhow::Result<Bytes> {
	let mut buf = BytesMut::new();
	kio::clock::timeout(TIMEOUT, async {
		while let Some(chunk) = recv.read_chunk(usize::MAX).await? {
			buf.extend_from_slice(&chunk);
		}
		anyhow::Ok(())
	})
	.await
	.with_context(|| format!("timed out reading {what}"))??;
	Ok(buf.freeze())
}
