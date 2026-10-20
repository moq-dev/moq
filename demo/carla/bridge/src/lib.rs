//! The native half of the CARLA demo's GPU video path.
//!
//! CARLA renders each truck camera with Unreal. The CARLA plugin copies the
//! render target on the GPU into an exportable Vulkan image it shares with this
//! library, which hands it to moq-video's encoder opened for the plugin's
//! device: NVENC converts, scales, and encodes it there. Compressed frames
//! leave through a Unix socket to the Python worker, which publishes them on
//! the truck's MoQ broadcast. Raw pixels never reach CPU memory, and nothing
//! here falls back to a CPU path: an unsupported device or a failed GPU step is
//! an error.
//!
//! - [`Bridge`] listens for the worker and owns the streams. The CARLA plugin
//!   holds one per process through the C ABI in `include/carla_bridge.h`.
//! - A stream is one worker broadcast (`truck1/front`) with its renditions,
//!   demand and encoders. Cameras register as sources of a stream by name.
//! - A [`Source`] is one camera's bounded set of exportable image slots.
//!
//! The worker protocol is [`protocol`].
//!
//! Linux only; the crate is empty elsewhere.
#![cfg(target_os = "linux")]

mod ffi;
pub mod protocol;
mod server;
mod source;
mod stream;

use std::collections::HashMap;
use std::fs::{File, TryLockError};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use anyhow::{Context, Result, bail};
use parking_lot::Mutex;

pub use moq_video::Size;
pub use moq_video::frame::vulkan::{Device, Format};
pub use source::Source;

use server::Output;
use stream::Stream;

/// The bridge: a socket for the worker and the streams it encodes.
pub struct Bridge {
    shared: Arc<Shared>,
    listener: Option<JoinHandle<()>>,
    /// Held for the bridge's life: whoever holds it owns the socket path, so a
    /// second process refuses the path instead of unlinking a live listener.
    _lock: File,
}

pub(crate) struct Shared {
    path: PathBuf,
    output: Arc<Output>,
    streams: Mutex<HashMap<String, Arc<Stream>>>,
    connection: Mutex<Option<(u64, UnixStream)>>,
    closing: AtomicBool,
}

impl Bridge {
    /// Listen for the worker at `path`, replacing a socket a previous process
    /// left behind. Another live bridge on `path` is an error.
    pub fn listen(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        // A socket file cannot say whether its listener is alive, and probing it
        // with a connection would displace that bridge's worker. An flock beside
        // it can: the kernel releases it when its holder exits.
        let mut lock_path = path.clone().into_os_string();
        lock_path.push(".lock");
        let lock_path = PathBuf::from(lock_path);
        let lock = File::options()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .with_context(|| format!("open {}", lock_path.display()))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                bail!("another bridge is listening on {}", path.display())
            }
            Err(TryLockError::Error(err)) => {
                return Err(err).with_context(|| format!("lock {}", lock_path.display()));
            }
        }
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_socket() => {
                std::fs::remove_file(&path)
                    .with_context(|| format!("remove the stale socket {}", path.display()))?;
            }
            Ok(_) => bail!("{} exists and is not a socket", path.display()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err).with_context(|| format!("inspect {}", path.display())),
        }
        let listener =
            UnixListener::bind(&path).with_context(|| format!("listen on {}", path.display()))?;
        let shared = Arc::new(Shared {
            path,
            output: Arc::default(),
            streams: Mutex::default(),
            connection: Mutex::default(),
            closing: AtomicBool::new(false),
        });
        let listener = std::thread::Builder::new()
            .name("carla-bridge:listen".into())
            .spawn({
                let shared = shared.clone();
                move || server::listen(&shared, listener)
            })
            .context("start the bridge listener")?;
        Ok(Self {
            shared,
            listener: Some(listener),
            _lock: lock,
        })
    }

    /// The socket the worker connects to.
    pub fn path(&self) -> &Path {
        &self.shared.path
    }

    /// Register a camera of `stream` rendering `size` images in `format` on the
    /// Vulkan `device`, with `slots` images to cycle. Every camera of a stream
    /// renders on one device.
    pub fn source(
        &self,
        stream: &str,
        device: Device,
        size: Size,
        format: Format,
        slots: usize,
    ) -> Result<Source> {
        let stream = self.shared.stream(stream)?;
        Source::new(stream, device, size, format, slots)
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.shared.closing.store(true, Ordering::Release);
        // Wake the blocking accept; it sees `closing` and exits.
        let _ = UnixStream::connect(&self.shared.path);
        if let Some(listener) = self.listener.take() {
            let _ = listener.join();
        }
        if let Some((_, socket)) = self.shared.connection.lock().take() {
            let _ = socket.shutdown(std::net::Shutdown::Both);
        }
        for stream in self.shared.streams.lock().drain().map(|(_, stream)| stream) {
            stream.shutdown();
        }
        let _ = std::fs::remove_file(&self.shared.path);
    }
}

impl Shared {
    fn closing(&self) -> bool {
        self.closing.load(Ordering::Acquire)
    }

    /// The stream called `name`, started on first use by either side.
    fn stream(&self, name: &str) -> Result<Arc<Stream>> {
        let mut streams = self.streams.lock();
        if let Some(stream) = streams.get(name) {
            return Ok(stream.clone());
        }
        let stream = Stream::spawn(name.to_owned(), self.output.clone())?;
        streams.insert(name.to_owned(), stream.clone());
        Ok(stream)
    }

    /// Make `socket` the worker connection, returning the one it replaces.
    fn replace_connection(&self, key: u64, socket: UnixStream) -> Option<UnixStream> {
        self.connection
            .lock()
            .replace((key, socket))
            .map(|(_, socket)| socket)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn socket_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("carla-bridge-{name}-{}.sock", std::process::id()))
    }

    #[test]
    fn a_live_bridge_keeps_its_socket() {
        let path = socket_path("live");
        let first = Bridge::listen(&path).unwrap();
        let err = Bridge::listen(&path)
            .err()
            .expect("a second bridge on the path");
        assert!(err.to_string().contains("another bridge"), "{err:#}");
        // The first bridge still owns a socket that accepts connections.
        UnixStream::connect(&path).expect("the first bridge still listens");
        drop(first);
    }

    #[test]
    fn a_stale_socket_is_replaced() {
        let path = socket_path("stale");
        let _ = std::fs::remove_file(&path);
        // A listener that exited without unlinking, like a crashed process.
        drop(UnixListener::bind(&path).unwrap());
        let bridge = Bridge::listen(&path).unwrap();
        UnixStream::connect(&path).expect("the new bridge listens");
        drop(bridge);
    }
}
