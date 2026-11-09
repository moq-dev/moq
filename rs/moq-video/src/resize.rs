//! Frame resizing options.

use crate::Output;

/// Options for [`Frame::resize`](crate::Frame::resize).
///
/// Build with `Config::default()` and set fields, so future options stay
/// additive.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Config {
	/// Where the scaled pixels live.
	///
	/// [`Output::Native`] scales a GPU surface on its own device, downloading
	/// and scaling on the CPU only when the device refuses. [`Output::Cpu`]
	/// downloads first and always scales on the CPU. [`Output::Render`] is
	/// `Native` for a surface `render::Renderer` imports and `Cpu` otherwise.
	pub output: Output,
	/// The VA-API render node that scales a DMA-BUF, such as `/dev/dri/renderD129`.
	///
	/// `None` uses the node the VA-API codecs share. Name the encoder's node so
	/// the scaled buffer imports into it without a copy.
	#[cfg(target_os = "linux")]
	pub vaapi: Option<std::path::PathBuf>,
}
