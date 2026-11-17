//! Which hardware a codec or scaler opens.

use std::path::{Path, PathBuf};

use crate::Error;

/// The hardware a decoder, encoder, or scaler opens.
///
/// Present on every platform and honoured by whichever backend opens. A Linux
/// hardware backend that drives another kind of device is passed over, so a
/// named node also steers automatic backend selection; software and a platform
/// API that picks its own device (VideoToolbox, Media Foundation, MediaCodec)
/// ignore it. A named device that fails to open is an error rather than a
/// search for another one: only [`Auto`](Self::Auto) chooses. Whether software
/// may take over when no hardware opens is the backend `Kind`'s call, not this.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Device {
	/// Let each backend pick its device.
	#[default]
	Auto,
	/// A DRM render node, such as `/dev/dri/renderD129`: VA-API opens it, and
	/// NVENC and NVDEC open the CUDA device behind it when an NVIDIA driver runs it.
	Render(PathBuf),
	/// A V4L2 memory-to-memory codec node, such as `/dev/video11`.
	Video(PathBuf),
}

impl Device {
	/// The named render node, if any.
	#[cfg_attr(
		not(all(target_os = "linux", any(feature = "vaapi", feature = "nvidia"))),
		allow(dead_code, reason = "only the VA-API and NVIDIA backends open one")
	)]
	pub(crate) fn render(&self) -> Option<&Path> {
		match self {
			Self::Render(path) => Some(path),
			_ => None,
		}
	}

	/// The named V4L2 codec node, if any.
	#[cfg_attr(
		not(all(target_os = "linux", feature = "v4l2")),
		allow(dead_code, reason = "only the V4L2 backends open one")
	)]
	pub(crate) fn video(&self) -> Option<&Path> {
		match self {
			Self::Video(path) => Some(path),
			_ => None,
		}
	}

	/// Whether a backend that drives `drives` opens this device, so selection
	/// passes over one that would open another.
	///
	/// Fails when telling needs the named node's driver and that cannot be read.
	pub(crate) fn takes(&self, drives: Drives) -> Result<bool, Error> {
		match (self, drives) {
			(Self::Auto, _) | (_, Drives::Own) => Ok(true),
			(Self::Render(_), Drives::Render) | (Self::Video(_), Drives::Video) => Ok(true),
			(Self::Render(path), Drives::Nvidia) => nvidia(path),
			_ => Ok(false),
		}
	}
}

impl std::fmt::Display for Device {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Self::Auto => f.write_str("any device"),
			Self::Render(path) => write!(f, "render node {}", path.display()),
			Self::Video(path) => write!(f, "codec node {}", path.display()),
		}
	}
}

/// The kind of [`Device`] a backend opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code, reason = "a build without a backend of some kind never names that kind")]
pub(crate) enum Drives {
	/// None a [`Device`] names: software, or a platform API that picks its own.
	Own,
	/// Any DRM render node (VA-API).
	Render,
	/// A DRM render node an NVIDIA driver runs (NVENC, NVDEC).
	Nvidia,
	/// A V4L2 codec node.
	Video,
}

/// Whether the NVIDIA driver runs the render node at `path`.
#[cfg(target_os = "linux")]
fn nvidia(path: &Path) -> Result<bool, Error> {
	Ok(field(&uevent(path)?, "DRIVER") == Some("nvidia"))
}

#[cfg(not(target_os = "linux"))]
fn nvidia(_path: &Path) -> Result<bool, Error> {
	Ok(false)
}

/// The sysfs `uevent` of the device behind the character node at `path`.
#[cfg(target_os = "linux")]
fn uevent(path: &Path) -> Result<String, Error> {
	use std::os::unix::fs::{FileTypeExt, MetadataExt};

	let failed = |err: &dyn std::fmt::Display| Error::codec(anyhow::anyhow!("{}: {err}", path.display()));
	let meta = std::fs::metadata(path).map_err(|err| failed(&err))?;
	if !meta.file_type().is_char_device() {
		return Err(failed(&"not a device node"));
	}
	let rdev = meta.rdev();
	let sysfs = format!(
		"/sys/dev/char/{}:{}/device/uevent",
		libc::major(rdev),
		libc::minor(rdev)
	);
	std::fs::read_to_string(&sysfs).map_err(|err| failed(&format_args!("{sysfs}: {err}")))
}

/// The value of `key` in a sysfs `uevent`'s `KEY=value` lines.
#[cfg(target_os = "linux")]
fn field<'a>(uevent: &'a str, key: &str) -> Option<&'a str> {
	uevent
		.lines()
		.find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
}

/// A PCI device's address, without the function: what CUDA reports per device.
#[cfg(all(target_os = "linux", feature = "nvidia"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Pci {
	pub domain: u32,
	pub bus: u32,
	pub device: u32,
}

#[cfg(all(target_os = "linux", feature = "nvidia"))]
impl Pci {
	/// The PCI address of the device behind the node at `path`.
	pub(crate) fn of(path: &Path) -> Result<Self, Error> {
		let uevent = uevent(path)?;
		let slot = field(&uevent, "PCI_SLOT_NAME")
			.ok_or_else(|| Error::codec(anyhow::anyhow!("{} is not a PCI device", path.display())))?;
		Self::parse(slot)
			.ok_or_else(|| Error::codec(anyhow::anyhow!("{}: malformed PCI address {slot}", path.display())))
	}

	/// Parses `domain:bus:device.function`, all hex, as sysfs spells it.
	fn parse(slot: &str) -> Option<Self> {
		let (domain, rest) = slot.split_once(':')?;
		let (bus, rest) = rest.split_once(':')?;
		let (device, _function) = rest.split_once('.')?;
		let hex = |s: &str| u32::from_str_radix(s, 16).ok();
		Some(Self {
			domain: hex(domain)?,
			bus: hex(bus)?,
			device: hex(device)?,
		})
	}

	/// The ordinal of the device at this address among `devices`, each an ordinal and its address.
	pub(crate) fn find(self, devices: impl IntoIterator<Item = (usize, Self)>) -> Option<usize> {
		devices
			.into_iter()
			.find_map(|(ordinal, pci)| (pci == self).then_some(ordinal))
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_named_node_steers_to_the_backends_that_drive_its_kind() {
		let render = Device::Render("/dev/dri/renderD129".into());
		let video = Device::Video("/dev/video11".into());
		for drives in [Drives::Own, Drives::Render, Drives::Nvidia, Drives::Video] {
			assert!(Device::Auto.takes(drives).unwrap(), "{drives:?} refused Auto");
		}
		assert!(render.takes(Drives::Own).unwrap());
		assert!(render.takes(Drives::Render).unwrap());
		assert!(!render.takes(Drives::Video).unwrap());
		assert!(video.takes(Drives::Own).unwrap());
		assert!(video.takes(Drives::Video).unwrap());
		assert!(!video.takes(Drives::Render).unwrap());
		assert!(!video.takes(Drives::Nvidia).unwrap());
	}

	/// The NVIDIA check reads the node, so a node that is not there is an error,
	/// not a quiet pass to another device.
	#[cfg(target_os = "linux")]
	#[test]
	fn a_missing_render_node_fails_the_nvidia_check() {
		let missing = Device::Render("/dev/dri/renderD999-missing".into());
		assert!(missing.takes(Drives::Nvidia).is_err());
		let file = Device::Render("/proc/self/status".into());
		assert!(file.takes(Drives::Nvidia).is_err());
	}

	#[cfg(target_os = "linux")]
	#[test]
	fn uevent_fields_are_read_by_exact_key() {
		let uevent = "DRIVER=nvidia\nPCI_CLASS=30000\nPCI_SLOT_NAME=0000:01:00.0\n";
		assert_eq!(field(uevent, "DRIVER"), Some("nvidia"));
		assert_eq!(field(uevent, "PCI_SLOT_NAME"), Some("0000:01:00.0"));
		assert_eq!(field(uevent, "PCI"), None);
	}

	#[cfg(all(target_os = "linux", feature = "nvidia"))]
	#[test]
	fn a_render_node_maps_to_the_cuda_device_at_its_pci_address() {
		let first = Pci::parse("0000:01:00.0").unwrap();
		let second = Pci::parse("0001:a1:1f.0").unwrap();
		assert_eq!(
			second,
			Pci {
				domain: 1,
				bus: 0xa1,
				device: 0x1f
			}
		);
		let devices = [(0, first), (1, second)];
		assert_eq!(first.find(devices), Some(0));
		assert_eq!(second.find(devices), Some(1));
		assert_eq!(Pci::parse("0000:02:00.0").unwrap().find(devices), None);
		assert_eq!(Pci::parse("0000:01:00"), None);
	}
}
