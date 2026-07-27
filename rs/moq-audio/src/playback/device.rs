//! Finding an output device and agreeing on a stream format with it.

use std::str::FromStr;

use cpal::traits::{DeviceTrait, HostTrait};

use crate::Error;

/// Rates to ask for, best first. 48 kHz is what Opus and the rest of the
/// pipeline run at, so matching it skips a resample; 44.1 kHz is the usual
/// second choice.
const RATES: &[u32] = &[48_000, 44_100];

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
	pub default: bool,
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
	let mut devices = Vec::new();

	for id in cpal::available_hosts() {
		let Ok(host) = cpal::host_from_id(id) else { continue };
		let default = host.default_output_device().and_then(|d| d.id().ok());
		let Ok(outputs) = host.output_devices() else { continue };

		for device in outputs {
			let Ok(id) = device.id() else { continue };
			devices.push(Device {
				default: Some(&id) == default.as_ref(),
				name: describe(&device, &id),
				id: id.to_string(),
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
/// Only considers formats in [`FORMATS`], then prefers a rate the pipeline
/// already runs at, then falls back to the highest the device supports, since
/// resampling down is kinder than resampling up.
pub(super) fn negotiate(device: &cpal::Device) -> Result<cpal::SupportedStreamConfig, Error> {
	let supported: Vec<_> = device
		.supported_output_configs()
		.map_err(|err| Error::Playback(format!("cannot enumerate output configs: {err}")))?
		.filter(|config| FORMATS.contains(&config.sample_format()))
		.collect();

	for &rate in RATES {
		for &format in FORMATS {
			let config = supported
				.iter()
				.filter(|c| c.sample_format() == format)
				.find_map(|c| (*c).try_with_sample_rate(rate));

			if let Some(config) = config {
				return Ok(config);
			}
		}
	}

	// Rate first, format as the tie-break, so an exotic device still opens.
	supported
		.into_iter()
		.max_by_key(|c| (c.max_sample_rate(), std::cmp::Reverse(rank(c.sample_format()))))
		.map(|c| c.with_max_sample_rate())
		.ok_or_else(|| Error::Unsupported("output device offers no sample format we can write".into()))
}

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
