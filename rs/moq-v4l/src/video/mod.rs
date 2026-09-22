#[macro_use]
mod macros;

pub mod traits;

pub mod capture;
pub mod output;

pub use traits::{Capture, Output};
