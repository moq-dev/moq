//! Catalog publish/subscribe.
//!
//! Two catalog formats coexist:
//!
//! - [`hang`] — the JSON catalog (track `catalog.json`) used by every codec
//!   importer in [`crate::import`]. Publish via [`hang::Producer`]; subscribe
//!   via [`hang::Consumer`].
//! - [`msf`] — the MSF catalog (track `catalog`), an alternate JSON shape.
//!   Subscribe via [`msf::Consumer`]; the same publish-side wraps both since
//!   [`hang::Producer`] writes both tracks on every update.
//!
//! [`CatalogFormat`] picks which one to subscribe to based on the broadcast's
//! filename-style suffix.

pub mod hang;
pub mod msf;

mod format;
pub use format::*;
