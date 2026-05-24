//! MSF catalog.
//!
//! The IETF MoQT Streaming Format catalog, served on the `catalog`
//! track. moq-mux subscribes here but doesn't publish; [`Consumer`]
//! reads MSF and converts it to a [`hang::Catalog`] on the fly so the
//! rest of the pipeline only sees one shape. Publishing is the hang
//! producer's job (it writes both tracks).

mod consumer;

pub use consumer::Consumer;
