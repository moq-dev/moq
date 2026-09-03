//! Finding an output device and agreeing on a stream format with it.

use std::str::FromStr;

use cpal::traits::{DeviceTrait, HostTrait};

use crate::Error;

/// Rates to ask for, best first. 48 kHz is what Opus and the rest of the
/// pipeline run at, so matching it skips a resample; 44.1 kHz is the usual
/// second choice.
const RATES: &[u32] = &[48_000, 44_100];

/// Channel counts to try, best first. The mixer produces stereo, and mono is
/// the only other count worth naming: anything else is a surround layout we
/// would be guessing the speaker order of.
const CHANNELS: &[u16] = &[2, 1];

/// Sample formats we can write, best first: `f32` is what the mixer produces,
/// and the rest are conversions on the way out.
///
/// The filter matters as much as the order. A device that offers several
/// formats (an ALSA `plughw:` node offers every format ALSA can convert to,
/// starting at 8-bit) will happily hand back one we can't write if we pick on
/// sample rate alone.
const FORMATS: &[cpal::SampleFormat] = &[
	cpal::SampleFormat::F32,
	cpal::SampleFormat::I32,
	cpal::SampleFormat::I16,
	cpal::SampleFormat::U16,
];

/// An audio output reported by [`devices`].
#[derive(Clone, Debug)]
pub struct Device {
	/// Opaque identifier: pass to [`Config::device`](super::Config) or
	/// [`Engine::switch`](super::Engine::switch). Stable across runs and
	/// reboots where the host API can manage it.
	pub id: String,
	/// Human-readable name, e.g. "Built-in Output".
	pub name: String,
	/// Whether this is the system default output.
	///
	/// True for at most one device: the preferred host's default. Another host's
	/// default is that host's, not the system's.
	pub default: bool,
	/// The host API this device is reached through, e.g. "PipeWire" or "ALSA".
	///
	/// The same hardware is usually reachable through several, so a caller that
	/// offers a choice groups by this.
	pub host: String,
}

/// List the audio outputs, across every host API the platform offers.
pub async fn devices() -> Result<Vec<Device>, Error> {
	// cpal enumerates devices with blocking host I/O, so keep it off the
	// runtime's worker threads.
	tokio::task::spawn_blocking(list)
		.await
		.map_err(|err| Error::Playback(format!("audio host thread failed: {err}")))?
}

fn list() -> Result<Vec<Device>, Error> {
	// Every host, not just the preferred one. The same hardware appears under
	// each, and which one a caller wants is its decision: PipeWire and PulseAudio
	// carry the server's own names and routing, ALSA reaches a device directly.
	// `Device::host` is what lets a caller group or filter them.
	let preferred = cpal::default_host().id();
	let mut devices = Vec::new();
	let mut seen = std::collections::HashSet::new();

	for id in cpal::available_hosts() {
		// A host that will not open takes every device on it with it, so say so:
		// the symptom is a device missing from the listing with no other trace.
		let host = match cpal::host_from_id(id) {
			Ok(host) => host,
			Err(err) => {
				tracing::debug!(host = id.name(), error = %err, "skipping an audio host that would not open");
				continue;
			}
		};
		let default = host.default_output_device().and_then(|d| d.id().ok());

		let outputs = match host.output_devices() {
			Ok(outputs) => outputs,
			Err(err) => {
				tracing::debug!(host = id.name(), error = %err, "skipping a host that would not list its outputs");
				continue;
			}
		};
		for device in outputs {
			let Ok(device_id) = device.id() else {
				tracing::debug!(host = id.name(), "skipping an output device with no id");
				continue;
			};
			// A sound server reports one id per stream, not per device.
			if !seen.insert(device_id.to_string()) {
				continue;
			}
			devices.push(Device {
				// Only the preferred host's default is the system default; the
				// others are that host's idea of one.
				default: id == preferred && Some(&device_id) == default.as_ref(),
				name: describe(&device, &device_id),
				host: id.name().to_string(),
				id: device_id.to_string(),
			});
		}
	}

	Ok(devices)
}

/// Open the device `selector` names, or the system default when it is `None`.
pub(super) fn open(selector: Option<&str>) -> Result<cpal::Device, Error> {
	let Some(selector) = selector else {
		return cpal::default_host()
			.default_output_device()
			.ok_or_else(|| Error::Device("no default output device".into()));
	};

	// Ids are host-qualified ("alsa:hw:0,0"), so route to the host that issued
	// this one rather than searching every host for a match.
	let id = cpal::DeviceId::from_str(selector).map_err(|err| Error::Device(format!("{selector:?}: {err}")))?;
	let host = cpal::host_from_id(id.host()).map_err(|err| Error::Device(format!("{selector:?}: {err}")))?;
	host.device_by_id(&id)
		.ok_or_else(|| Error::Device(format!("output device {selector:?} not found")))
}

/// Pick the stream format to open `device` with.
///
/// Only considers formats in [`FORMATS`], and prefers in that order: a channel
/// count in [`CHANNELS`], a rate the pipeline already runs at, then a format we
/// write without converting. Failing all of those it takes the highest rate the
/// device supports, since resampling down is kinder than resampling up.
pub(super) fn negotiate(device: &cpal::Device) -> Result<cpal::SupportedStreamConfig, Error> {
	let supported = device
		.supported_output_configs()
		.map_err(|err| Error::Playback(format!("cannot enumerate output configs: {err}")))?;

	choose(supported).ok_or_else(|| Error::Unsupported("output device offers no sample format we can write".into()))
}

/// Pick the best of the stream configurations a device reports.
///
/// Split out from [`negotiate`] so it can be tested without a device: the
/// preference order is three levels deep and the outermost exists to fix an
/// audible bug.
fn choose(supported: impl Iterator<Item = cpal::SupportedStreamConfigRange>) -> Option<cpal::SupportedStreamConfig> {
	supported
		.filter(|config| FORMATS.contains(&config.sample_format()))
		.min_by_key(preference)
		.map(|config| match preferred_rate(&config) {
			Some(rate) => config.try_with_sample_rate(rate).expect("a rate the range covers"),
			None => config.with_max_sample_rate(),
		})
}

/// Rank a configuration against the ones this crate would rather have, lowest
/// first.
///
/// One key rather than nested loops because the levels are not independent: a
/// stereo device offering only 96 kHz has to beat a mono one offering 48 kHz,
/// and a pass per channel count that gave up when neither preferred rate matched
/// would hand that to the mono device and downmix. The channel count leads
/// because a downmix is audible where a resample is not.
fn preference(config: &cpal::SupportedStreamConfigRange) -> (usize, usize, std::cmp::Reverse<u32>, usize) {
	let channels = CHANNELS
		.iter()
		.position(|count| *count == config.channels())
		.unwrap_or(CHANNELS.len());
	let rate = match preferred_rate(config) {
		Some(rate) => (RATES.iter().position(|r| *r == rate).expect("from RATES"), rate),
		// Nothing we asked for, so take the most the device offers: resampling
		// down is kinder than resampling up.
		None => (RATES.len(), config.max_sample_rate()),
	};
	(
		channels,
		rate.0,
		std::cmp::Reverse(rate.1),
		rank(config.sample_format()),
	)
}

/// The first rate in [`RATES`] this configuration covers, if any.
fn preferred_rate(config: &cpal::SupportedStreamConfigRange) -> Option<u32> {
	RATES
		.iter()
		.copied()
		.find(|rate| config.min_sample_rate() <= *rate && *rate <= config.max_sample_rate())
}

/// Where `format` sits in [`FORMATS`], so a lower rank is a better format.
///
/// Unlisted formats sort last, which is what makes this usable as a tie-break
/// against a device offering something we filtered out.
fn rank(format: cpal::SampleFormat) -> usize {
	FORMATS.iter().position(|f| *f == format).unwrap_or(FORMATS.len())
}

/// A human-readable name, falling back to the id when the host can't describe
/// the device.
fn describe(device: &cpal::Device, id: &cpal::DeviceId) -> String {
	device
		.description()
		.map(|d| d.name().to_string())
		.unwrap_or_else(|_| id.id().to_string())
}

#[cfg(test)]
mod tests {
	use cpal::{SampleFormat, SupportedBufferSize, SupportedStreamConfigRange};

	use super::*;

	fn range(channels: u16, rate: u32, format: SampleFormat) -> SupportedStreamConfigRange {
		SupportedStreamConfigRange::new(channels, rate, rate, SupportedBufferSize::Unknown, format)
	}

	/// The bug this ordering exists for: an ALSA plugin node that lists mono
	/// first would otherwise be opened in mono, downmixing a stereo broadcast on
	/// the way to a stereo sink.
	#[test]
	fn stereo_wins_even_when_the_device_lists_mono_first() {
		let chosen = choose([range(1, 48_000, SampleFormat::F32), range(2, 48_000, SampleFormat::F32)].into_iter())
			.expect("a config");
		assert_eq!(chosen.channels(), 2);
	}

	/// Preferring stereo must not refuse a device that has no stereo to offer.
	#[test]
	fn mono_is_taken_when_that_is_all_there_is() {
		let chosen = choose([range(1, 48_000, SampleFormat::F32)].into_iter()).expect("a config");
		assert_eq!(chosen.channels(), 1);
	}

	/// Neither preferred count is offered, so the pass that accepts any count
	/// has to catch it. Without it a surround-only sink would not open at all.
	#[test]
	fn a_count_we_do_not_prefer_still_opens() {
		let chosen = choose([range(6, 48_000, SampleFormat::F32)].into_iter()).expect("a config");
		assert_eq!(chosen.channels(), 6);
	}

	/// Rate is preferred within a channel count, not across one: a stereo config
	/// at an awkward rate beats a mono config at the pipeline's own rate,
	/// because resampling is inaudible and a downmix is not.
	#[test]
	fn channels_outrank_the_sample_rate() {
		let chosen = choose([range(1, 48_000, SampleFormat::F32), range(2, 44_100, SampleFormat::F32)].into_iter())
			.expect("a config");
		assert_eq!((chosen.channels(), chosen.sample_rate()), (2, 44_100));
	}

	/// Within a channel count and rate, take the format the mixer already
	/// produces rather than one that costs a conversion.
	#[test]
	fn f32_is_preferred_over_a_format_we_convert_to() {
		let chosen = choose([range(2, 48_000, SampleFormat::I16), range(2, 48_000, SampleFormat::F32)].into_iter())
			.expect("a config");
		assert_eq!(chosen.sample_format(), SampleFormat::F32);
	}

	/// A device offering only formats we cannot write is an error rather than a
	/// stream that plays noise.
	#[test]
	fn a_device_we_cannot_write_to_is_rejected() {
		assert!(choose([range(2, 48_000, SampleFormat::I8)].into_iter()).is_none());
		assert!(choose(std::iter::empty()).is_none());
	}

	/// Channels lead even when neither preferred rate is on offer.
	///
	/// The case two review bots found in the first version of this, which gave
	/// up on a channel count as soon as no preferred rate matched it: a device
	/// with stereo only at 96 kHz and mono at 48 kHz went to mono, which is the
	/// downmix the whole preference exists to avoid.
	#[test]
	fn stereo_at_an_awkward_rate_beats_mono_at_a_preferred_one() {
		let chosen = choose([range(1, 48_000, SampleFormat::F32), range(2, 96_000, SampleFormat::F32)].into_iter())
			.expect("a config");
		assert_eq!((chosen.channels(), chosen.sample_rate()), (2, 96_000));
	}

	/// The same with neither count on a preferred rate, so the ordering rests on
	/// the channel rank alone rather than on one side matching a rate.
	#[test]
	fn stereo_beats_mono_when_neither_is_on_a_preferred_rate() {
		let chosen = choose(
			[
				range(1, 192_000, SampleFormat::F32),
				range(2, 96_000, SampleFormat::F32),
			]
			.into_iter(),
		)
		.expect("a config");
		assert_eq!((chosen.channels(), chosen.sample_rate()), (2, 96_000));
	}

	/// Nothing in the preferred lists matches, so the last resort picks the
	/// highest rate and breaks the tie on format.
	#[test]
	fn the_last_resort_takes_the_highest_rate() {
		let chosen = choose(
			[
				range(2, 96_000, SampleFormat::I16),
				range(2, 96_000, SampleFormat::F32),
				range(2, 32_000, SampleFormat::F32),
			]
			.into_iter(),
		)
		.expect("a config");
		assert_eq!(
			(chosen.sample_rate(), chosen.sample_format()),
			(96_000, SampleFormat::F32)
		);
	}
}
