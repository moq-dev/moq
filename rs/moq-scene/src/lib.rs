//! Scenes, live sources, and a GPU compositor for a MoQ streaming studio.
//!
//! The UI-free half of `moq-studio`, so the same scenes can be composed by a
//! windowed app, a headless renderer, or a test:
//!
//! - [`Collection`] is the scene graph: a [`Canvas`], named [`Source`]s, and
//!   [`Scene`]s placing them. It saves to a versioned TOML file that holds no
//!   credentials.
//! - [`source::Sources`] runs each source: a camera, display, window,
//!   microphone, or system-audio capture (with the `capture` feature), a still
//!   image, a solid color, or a broadcast subscribed over MoQ. Pictures go to the
//!   compositor and sound goes to a `moq_audio::bus::Bus`, the program audio.
//! - [`Compositor`] draws the scene in preview and the scene on program into
//!   textures on the caller's `wgpu` device, cutting or fading between them.
//!   `moq_video::render::export` hands the program to an encoder, on the GPU
//!   where the platform allows.
//!
//! Every picture is drawn on the program clock the caller ticks
//! [`Compositor::render`] with, from whatever each source delivered last: a
//! source that stalls repeats its last picture rather than holding up the
//! canvas.

mod collection;
mod compositor;
pub mod source;

pub use collection::{Audio, Canvas, Collection, Item, Rect, Rgba, Scene, Source, Transition};
pub use compositor::{Compositor, FORMAT};

/// The `wgpu` the compositor draws with: `moq_video`'s, so its textures and
/// the decoder's are the same type.
pub use moq_video::render::wgpu;

/// Errors from loading scenes, opening sources, and drawing them.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// The scene file is not valid TOML of the expected shape.
	#[error("scene file: {0}")]
	Format(String),

	/// The scene file is from another version of the format.
	#[error("scene file version {found} is not supported; this build reads version {supported}")]
	Version {
		/// The version the file names.
		found: i64,
		/// The version this build reads.
		supported: u32,
	},

	/// The collection breaks a rule the types cannot express, such as an item
	/// naming a source that does not exist.
	#[error("invalid scene collection: {0}")]
	Invalid(String),

	/// An image source could not be read or decoded.
	#[error("image: {0}")]
	Image(String),

	/// This build or platform cannot open the source.
	#[error("unsupported source: {0}")]
	Unsupported(String),

	/// Capturing, decoding, or drawing video failed.
	#[error(transparent)]
	Video(#[from] moq_video::Error),

	/// Capturing, decoding, or mixing audio failed.
	#[error(transparent)]
	Audio(#[from] moq_audio::Error),

	/// Reading a broadcast's catalog or media failed.
	#[error(transparent)]
	Mux(#[from] moq_mux::Error),

	/// The broadcast subscription failed.
	#[error(transparent)]
	Net(#[from] moq_net::Error),
}
