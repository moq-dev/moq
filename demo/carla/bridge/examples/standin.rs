//! Feed the bridge without CARLA: the test's stand-in renderer drives an sd
//! and an hd camera of one stream at 30 fps, cycling colors, for exercising
//! the worker's bridge client, a relay and the player by hand.
//!
//! ```sh
//! cargo run -p moq-carla-bridge --example standin -- /tmp/carla-bridge.sock truck1/front
//! ```
//!
//! Needs the host NVIDIA driver on the loader path, as `just carla bridge-gpu`
//! arranges.
#![cfg_attr(
    target_os = "linux",
    expect(
        clippy::disallowed_methods,
        clippy::disallowed_types,
        reason = "the stand-in paces real GPU renders at 30 fps on a thread with no runtime"
    )
)]

#[cfg(target_os = "linux")]
#[path = "../tests/standin/mod.rs"]
mod standin;

#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
use carla_bridge::{Bridge, Size};

#[cfg(target_os = "linux")]
const STEP: Duration = Duration::from_nanos(33_333_333);

// An example needs a `main` on every target, or `--all-targets` stops compiling
// on a Mac, so elsewhere it refuses instead of vanishing.
#[cfg(not(target_os = "linux"))]
fn main() -> Result<(), &'static str> {
    Err("standin needs Linux and the NVIDIA driver")
}

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let (Some(socket), Some(stream)) = (args.next(), args.next()) else {
        anyhow::bail!("usage: standin <socket> <stream>");
    };
    let gpu = standin::Gpu::open().ok_or_else(|| anyhow::anyhow!("no NVIDIA Vulkan device"))?;
    let bridge = Bridge::listen(&socket)?;
    let mut cameras = [
        standin::Camera::new(&gpu, &bridge, &stream, Size::new(426, 240), 3),
        standin::Camera::new(&gpu, &bridge, &stream, Size::new(1280, 720), 3),
    ];
    eprintln!("standin: rendering {stream} for the worker at {socket}");

    let start = Instant::now();
    let mut wanted = [false; 2];
    let mut frame = 0u64;
    loop {
        frame += 1;
        let deadline = start + STEP * frame as u32;
        std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
        // A slow sweep through the hue circle, so motion is visible.
        let phase = frame as f64 / 90.0 * std::f64::consts::TAU;
        let channel = |offset: f64| (128.0 + 100.0 * (phase + offset).sin()) as u8;
        let rgb = [channel(0.0), channel(2.1), channel(4.2)];
        for (index, camera) in cameras.iter_mut().enumerate() {
            let now = camera.wanted();
            if now != wanted[index] {
                eprintln!(
                    "standin: {}x{} camera {}",
                    camera.size.width,
                    camera.size.height,
                    if now { "rendering" } else { "parked" }
                );
                wanted[index] = now;
            }
            if now && !camera.render(rgb, frame, frame * 33_333) {
                eprintln!("standin: frame {frame} dropped, every slot busy");
            }
        }
    }
}
