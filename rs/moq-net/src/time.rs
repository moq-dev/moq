//! Timer facade using kio on native and wasmtimer in the browser.

#[cfg(not(target_family = "wasm"))]
pub use kio::time::{Instant, Sleep, interval, sleep};

#[cfg(target_family = "wasm")]
pub use wasmtimer::{
	std::Instant,
	tokio::{Sleep, interval, sleep},
};
