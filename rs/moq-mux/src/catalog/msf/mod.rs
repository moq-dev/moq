//! MSF catalog. Subscribe-only.
//!
//! [`Consumer`] reads MSF and converts to a [`hang::Catalog`] on the fly,
//! so the rest of the pipeline only sees one shape. Publishing happens
//! through [`super::hang::Producer`].

mod consumer;

pub use consumer::Consumer;
