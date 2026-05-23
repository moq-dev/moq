//! MSF catalog: alternate JSON-encoded broadcast description.
//!
//! Subscribe-only here: [`Consumer`] reads MSF and converts on the fly to
//! a [`hang::Catalog`] so downstream code only deals with one shape.
//! Publishing happens through [`super::hang::Producer`], which writes both
//! tracks together.

mod consumer;

pub use consumer::Consumer;
