//! End-to-end encrypted hang broadcasts.
//!
//! [`Producer`] encrypts a plaintext hang broadcast into a protected one with
//! [`moq_e2ee`]. Publish into a local broadcast exactly as for plaintext, with any importer or
//! encoder and the usual [`catalog::Producer`](crate::catalog::Producer), but never announce it.
//! [`Producer`] reads that broadcast and writes every track it carries to the announced output
//! under its physical name, so a relay sees no semantic track name, catalog, or payload:
//!
//! - `catalog.json` and `catalog.json.z` are published under their physical names. Each
//!   rendition key in the decrypted catalog, and each timeline it lists, is the physical name
//!   of its track. The compressed twin is compressed before it is encrypted.
//! - The MSF `catalog` is not published: it has no encrypted form.
//! - Only tracks the catalog names are carried, so a track it does not list (stats, a squatted
//!   name) never reaches the output. Every group keeps its sequence and every frame its index,
//!   so the timelines stay valid.
//!
//! Protected broadcasts are outside recording, replay, and viewer feedback, so a catalog that
//! references another broadcast, an archive replay, or an echo prefix is refused. So is one naming a track the source does not carry,
//! or one consumers would reject once renamed.

mod catalog;
mod producer;

pub use producer::Producer;
