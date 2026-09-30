//! AAC-LC through symphonia, the pure-Rust fallback for hosts without a
//! platform AAC decoder.

use symphonia_core::codecs::audio::AudioDecoder;

use super::Backend;
use crate::decode::Decoded;
use crate::{Activity, Error, Layout, aac};

pub(super) const NAME: &str = "symphonia";

pub(super) struct Symphonia {
	inner: symphonia_codec_aac::AacDecoder,
	sample_rate: u32,
	layout: Layout,
}

impl Symphonia {
	/// AAC-LC in mono or stereo only, which is what every gateway that feeds
	/// this crate publishes.
	///
	/// HE-AAC is rejected however its config spells it: leading with SBR or PS
	/// (mp4a.40.5 / .29), or leading with LC and declaring SBR in a sync extension
	/// after the core. Symphonia decodes no SBR either way, so the alternative is
	/// half-rate audio that sounds like a fault rather than an unsupported codec.
	/// A stream that signals SBR only in band is indistinguishable from LC in the
	/// config, and does decode as the core.
	pub(super) fn open(catalog: &hang::catalog::AudioConfig) -> Result<Box<dyn Backend>, Error> {
		use symphonia_core::codecs::audio::well_known::CODEC_ID_AAC;
		use symphonia_core::codecs::audio::{AudioCodecParameters, AudioDecoderOptions};

		let hang::catalog::AudioCodec::AAC(codec) = &catalog.codec else {
			return Err(Error::Unsupported(format!("symphonia cannot decode {}", catalog.codec)));
		};
		let description = aac::description(catalog, codec.profile)?;

		let mut params = AudioCodecParameters::new();
		params
			.for_codec(CODEC_ID_AAC)
			.with_extra_data(description.to_vec().into_boxed_slice());

		let inner = symphonia_codec_aac::AacDecoder::try_new(&params, &AudioDecoderOptions::default())
			.map_err(|err| Error::Unsupported(format!("aac decoder: {err}")))?;

		// Resolved by the decoder from the config, so this is what it will emit
		// even when the catalog's own fields say otherwise.
		let params = inner.codec_params();
		let sample_rate = params
			.sample_rate
			.ok_or_else(|| Error::Unsupported("aac config declares no sample rate".into()))?;
		let channel_count = params
			.channels
			.as_ref()
			.map(|channels| channels.count())
			.ok_or_else(|| Error::Unsupported("aac config declares no channels".into()))?;

		Ok(Box::new(Self {
			inner,
			sample_rate,
			layout: Layout::from_channels(channel_count as u32)?,
		}))
	}
}

impl Backend for Symphonia {
	fn decode(&mut self, packet: &[u8]) -> Result<Decoded, Error> {
		// The packet is a raw AAC frame, not ADTS, so there is nothing to timestamp
		// it with here: the container carries the timestamp and the decoder only
		// reads the payload.
		let packet = symphonia_core::packet::PacketRef::new(
			0,
			symphonia_core::units::Timestamp::ZERO,
			symphonia_core::units::Duration::ZERO,
			packet,
		);

		let decoded = self
			.inner
			.decode_ref(&packet)
			.map_err(|err| Error::Decode(format!("aac: {err}")))?;

		let mut samples = Vec::new();
		decoded.copy_to_vec_interleaved(&mut samples);
		Ok(Decoded {
			samples,
			activity: Activity::Active,
		})
	}

	fn reset(&mut self) -> Result<(), Error> {
		self.inner.reset();
		Ok(())
	}

	fn sample_rate(&self) -> u32 {
		self.sample_rate
	}

	fn layout(&self) -> Layout {
		self.layout
	}

	fn name(&self) -> &str {
		NAME
	}
}
