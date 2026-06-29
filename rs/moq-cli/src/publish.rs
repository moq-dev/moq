use clap::Subcommand;
use hang::moq_net;
use moq_mux::container::{flv, fmp4, ts};

#[derive(Subcommand, Clone)]
pub enum PublishFormat {
	Avc3,
	Fmp4,
	/// MPEG-TS (transport stream) read from stdin.
	Ts,
	/// FLV (Flash Video / RTMP) read from stdin.
	Flv,
}

enum PublishDecoder {
	Avc3(Box<moq_mux::import::TrackStream>),
	Fmp4(Box<fmp4::Import>),
	// TS carries undecoded elementary streams (SCTE-35, teletext, DVB AC-3, ...)
	// verbatim, so it uses the typed `mpegts` catalog extension rather than the untyped default.
	Ts(Box<ts::Import<ts::catalog::Ext>>),
	Flv(Box<flv::Import>),
}

impl PublishDecoder {
	/// Decode a chunk of stdin bytes. Each importer buffers any partial trailing
	/// frame internally, so the caller feeds fresh chunks rather than an
	/// accumulating buffer.
	fn decode_chunk(&mut self, chunk: &[u8]) -> anyhow::Result<()> {
		match self {
			Self::Avc3(d) => d.decode(chunk)?,
			Self::Fmp4(d) => d.decode(chunk)?,
			Self::Ts(d) => d.decode(chunk)?,
			Self::Flv(d) => d.decode(chunk)?,
		}
		Ok(())
	}

	/// Flush any buffered trailing frame and close the tracks at end of input.
	fn finish(&mut self) -> anyhow::Result<()> {
		match self {
			Self::Avc3(d) => d.finish()?,
			Self::Fmp4(d) => d.finish()?,
			Self::Ts(d) => d.finish()?,
			Self::Flv(d) => d.finish()?,
		}
		Ok(())
	}
}

pub struct Publish {
	source: PublishDecoder,
	broadcast: moq_net::BroadcastProducer,
}

impl Publish {
	pub fn new(format: &PublishFormat) -> anyhow::Result<Self> {
		let mut broadcast = moq_net::Broadcast::new().produce();

		// TS carries undecoded elementary streams (SCTE-35, teletext, DVB AC-3, ...)
		// verbatim, so it uses the `mpegts` catalog extension rather than the media-only
		// `()`. The catalog producer owns the broadcast's catalog tracks, so each broadcast
		// gets exactly one; TS builds its `Ext` catalog here instead of the shared `()` below.
		if let PublishFormat::Ts = format {
			let catalog = moq_mux::catalog::Producer::with_catalog(
				&mut broadcast,
				moq_mux::catalog::hang::Catalog::<ts::catalog::Ext>::default(),
			)?;
			let ts = ts::Import::new(broadcast.clone(), catalog);
			return Ok(Self {
				source: PublishDecoder::Ts(Box::new(ts)),
				broadcast,
			});
		}

		let catalog = moq_mux::catalog::Producer::new(&mut broadcast)?;
		let source = match format {
			PublishFormat::Avc3 => {
				let track = moq_mux::import::unique_track(&mut broadcast, ".avc3")?;
				let avc3 = moq_mux::import::TrackStream::new(track, catalog.clone(), "avc3")?;
				PublishDecoder::Avc3(Box::new(avc3))
			}
			PublishFormat::Fmp4 => {
				let fmp4 = fmp4::Import::new(broadcast.clone(), catalog.clone());
				PublishDecoder::Fmp4(Box::new(fmp4))
			}
			PublishFormat::Ts => unreachable!("TS is handled above with the mpegts catalog extension"),
			PublishFormat::Flv => {
				let flv = flv::Import::new(broadcast.clone(), catalog.clone());
				PublishDecoder::Flv(Box::new(flv))
			}
		};

		Ok(Self { source, broadcast })
	}

	pub fn consume(&self) -> moq_net::BroadcastConsumer {
		self.broadcast.consume()
	}

	pub async fn run(self) -> anyhow::Result<()> {
		let mut decoder = self.source;

		let mut stdin = tokio::io::stdin();
		let mut buffer = bytes::BytesMut::new();

		loop {
			buffer.clear();
			let n = tokio::io::AsyncReadExt::read_buf(&mut stdin, &mut buffer).await?;
			if n == 0 {
				// EOF: flush the importer's buffered trailing frame and close the tracks.
				decoder.finish()?;
				return Ok(());
			}
			decoder.decode_chunk(&buffer)?;
		}
	}
}
