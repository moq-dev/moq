//! Versioned hang recording objects on any [`object_store::ObjectStore`].
//!
//! The crate owns the portable layout and codecs: percent-encoded track names, `.info` JSON,
//! the binary segment envelope, and put/get/list/delete. Callers that need runtime dispatch
//! supply `Arc<dyn ObjectStore>`; the archive API itself stays generic.
//!
//! Group bounds are finite inclusive ranges in first-to-last order:
//! ```
//! let key = moq_archive::Key::groups("video", 5..=7)?;
//! assert_eq!(key.track(), "video");
//! # Ok::<(), moq_archive::Error>(())
//! ```

pub use object_store;

mod error;
pub mod info;
mod path;
pub mod segment;
pub mod store;

pub use error::{Error, Result};
pub use info::Info;
pub use path::Key;
pub use segment::{Frame, Group, Object};
pub use store::Store;

/// Recording format version written into `.info` and the binary envelope.
pub const VERSION: u64 = 1;

/// Largest group, segment, or timestamp value the format allows (`2^53 - 1`).
pub const ID_MAX: u64 = (1 << 53) - 1;

/// Decimal width of group and segment ID filename fields.
pub(crate) const ID_WIDTH: usize = 19;
