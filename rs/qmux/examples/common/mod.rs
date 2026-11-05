//! The scenario both interop examples share; only who opens which stream differs.

use std::time::Duration;

use anyhow::{Context as _, bail};
use bytes::{Bytes, BytesMut};
use moq_net::transport::poll::{RecvStream as _, SendStream as _};
use qmux::{RecvStream, SendStream, Version};

pub const PROTOCOL: &str = "qmux-interop";
pub const CLIENT_SEED: usize = 17;
pub const SERVER_SEED: usize = 29;
pub const TIMEOUT: Duration = Duration::from_secs(15);
const PAYLOAD_LEN: usize = 1_200_000;

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
