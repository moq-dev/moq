//! Catalog publish/subscribe.
//!
//! Two formats coexist on every broadcast: [`hang`] (the original JSON
//! shape, track `catalog.json`) and [`msf`] (an alternate IETF shape,
//! track `catalog`). Publishing through [`hang::Producer`] writes both;
//! subscribers pick one based on the broadcast's filename suffix —
//! see [`CatalogFormat`].

pub mod hang;
pub mod msf;

mod format;
pub use format::*;
