//! Where a worker's QUIC connections write their qlog traces.
//!
//! A [`Sink`] owns a directory and the one background thread that writes into
//! it. Connections stage their trace bytes in memory and hand whole chunks to
//! that thread, so a pinned worker never issues a file syscall: the QUIC
//! stacks want a `std::io::Write` that is `Send + Sync`, which rules out
//! holding the worker's `!Send` ring handle, and a synchronous `write(2)` on
//! the worker would stall every connection sharing its core for as long as
//! the filesystem takes.
//!
//! One file per trace, in the qlog JSON-SEQ format the tokio listener already
//! writes, so an existing workflow reads both without knowing which runtime
//! produced them.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};

use super::Error;

/// Bytes staged per trace before a chunk is handed to the writer thread.
///
/// The QUIC stacks serialize each event straight into the writer in many small
/// pieces, so something has to batch them. `BufWriter`'s default is what the
/// tokio listener's traces are batched by, and matching it keeps a live trace
/// as fresh on disk here as it is there.
const CHUNK: usize = 8 * 1024;

/// Most bytes queued for the writer thread at once, across every trace.
///
/// A disk that cannot keep up with the traces must not grow the queue without
/// bound, and blocking the worker is the one thing this sink exists to avoid,
/// so chunks past the ceiling are dropped. JSON-SEQ records are newline
/// delimited, so a reader resynchronizes after the gap.
const QUEUED_MAX: usize = 64 * 1024 * 1024;

/// Which end of a connection a trace was captured from.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Side {
	Client,
	Server,
}

impl Side {
	fn as_str(self) -> &'static str {
		match self {
			Self::Client => "client",
			Self::Server => "server",
		}
	}
}

/// A directory of qlog traces, and the thread that writes them.
///
/// Build one per process (or per worker group) and hand clones to every
/// [`Transport`](super::Transport) that should capture: clones share the
/// directory and the writer thread, so the cost is one thread however many
/// workers and connections there are. Dropping the last clone flushes and
/// closes every open trace before returning.
#[derive(Clone)]
pub struct Sink {
	inner: Arc<Inner>,
}

impl Sink {
	/// Write traces into `dir`, which must already exist and be writable.
	///
	/// Files are named `moq-<started>-<id>-<side>.qlog`, where `started` is
	/// the sink's creation time in milliseconds since the epoch (so a second
	/// run does not overwrite the first) and `id` names the connection.
	pub fn directory(dir: impl Into<PathBuf>) -> Result<Self, Error> {
		let dir = dir.into();
		let started = std::time::SystemTime::now()
			.duration_since(std::time::UNIX_EPOCH)
			.unwrap_or_default()
			.as_millis();

		// A read-only or missing directory is reported here, where the
		// operator set it, rather than as a warning per connection once the
		// traces they went looking for are already not being written.
		let probe = dir.join(format!(".moq-qlog-{}-{started}", std::process::id()));
		std::fs::write(&probe, b"").map_err(|err| Error::Qlog(format!("{}: {err}", dir.display())))?;
		let _ = std::fs::remove_file(&probe);

		let (tx, rx) = mpsc::channel();
		let queued = Arc::new(AtomicUsize::new(0));
		let thread = std::thread::Builder::new()
			.name("moq-qlog".to_string())
			.spawn({
				let queued = queued.clone();
				move || write_traces(rx, &queued)
			})
			.map_err(|err| Error::Qlog(format!("failed to spawn the qlog writer: {err}")))?;

		Ok(Self {
			inner: Arc::new(Inner {
				dir,
				started,
				tx: Some(tx),
				queued,
				next: AtomicU64::new(0),
				dropped: AtomicBool::new(false),
				thread: Some(thread),
			}),
		})
	}

	/// A writer for one connection's trace, named from its connection id.
	#[cfg_attr(all(feature = "quinn", not(feature = "noq")), allow(dead_code))]
	pub(crate) fn trace(&self, cid: &[u8], side: Side) -> Box<dyn io::Write + Send + Sync> {
		self.open(Some(cid), side)
	}

	/// A writer for a trace covering a whole endpoint rather than one
	/// connection, which is all quinn-proto's single per-config sink can
	/// express. Each event carries the qlog `group_id` of the connection it
	/// belongs to.
	#[cfg_attr(any(not(feature = "quinn"), feature = "noq"), allow(dead_code))]
	pub(crate) fn endpoint_trace(&self, side: Side) -> Box<dyn io::Write + Send + Sync> {
		self.open(None, side)
	}

	fn open(&self, cid: Option<&[u8]>, side: Side) -> Box<dyn io::Write + Send + Sync> {
		use std::fmt::Write as _;

		let inner = self.inner.clone();
		let file = inner.next.fetch_add(1, Ordering::Relaxed);
		// An endpoint-wide trace has no connection id to name it, so the
		// sink's own slot stands in and stays unique within the directory.
		let id = match cid {
			Some(cid) => cid.iter().fold(String::with_capacity(cid.len() * 2), |mut id, byte| {
				let _ = write!(id, "{byte:02x}");
				id
			}),
			None => format!("endpoint{file}"),
		};
		let path = inner
			.dir
			.join(format!("moq-{}-{id}-{}.qlog", inner.started, side.as_str()));
		inner.send(Msg::Open { file, path });
		Box::new(Trace {
			inner,
			file,
			buf: Vec::with_capacity(CHUNK),
		})
	}
}

impl std::fmt::Debug for Sink {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Sink").field("dir", &self.inner.dir).finish()
	}
}

/// The directory, the channel to the writer thread, and the shared accounting.
struct Inner {
	dir: PathBuf,
	/// Milliseconds since the epoch when the sink was built, in every filename.
	started: u128,
	/// `Option` so [`Drop`] can close the channel before joining the thread.
	tx: Option<mpsc::Sender<Msg>>,
	/// Bytes handed to the thread and not yet written, against [`QUEUED_MAX`].
	queued: Arc<AtomicUsize>,
	/// The next trace's slot in the writer thread's table.
	next: AtomicU64,
	/// Whether a chunk has been dropped, so the warning is logged once.
	dropped: AtomicBool,
	/// `Option` for the same reason as `tx`: [`Drop`] takes it to join.
	thread: Option<std::thread::JoinHandle<()>>,
}

impl Inner {
	fn send(&self, msg: Msg) {
		if let Some(tx) = &self.tx {
			let _ = tx.send(msg);
		}
	}

	/// Queue a chunk, dropping it when the writer thread is too far behind.
	fn send_chunk(&self, file: u64, buf: Vec<u8>) {
		let queued = self.queued.fetch_add(buf.len(), Ordering::Relaxed) + buf.len();
		if queued > QUEUED_MAX {
			self.queued.fetch_sub(buf.len(), Ordering::Relaxed);
			if !self.dropped.swap(true, Ordering::Relaxed) {
				tracing::warn!("dropping qlog output: over {QUEUED_MAX} bytes are queued for the writer");
			}
			return;
		}
		self.send(Msg::Data { file, buf });
	}
}

impl Drop for Inner {
	fn drop(&mut self) {
		// Close the channel so the loop ends, then wait for the tail of every
		// trace to reach the disk. Traces hold an `Arc` on us, so they are all
		// gone by now and nothing else can be queued.
		self.tx.take();
		if let Some(thread) = self.thread.take() {
			let _ = thread.join();
		}
	}
}

/// What the writer thread is told to do.
enum Msg {
	Open { file: u64, path: PathBuf },
	Data { file: u64, buf: Vec<u8> },
	Close { file: u64 },
}

/// One connection's trace: the `io::Write` a QUIC stack streams into.
///
/// The stacks serialize each event straight into the writer, so this stages
/// small writes and only crosses to the writer thread once a whole chunk is
/// ready. `flush` is called when a trace ends; the tail goes out on drop
/// regardless.
struct Trace {
	inner: Arc<Inner>,
	file: u64,
	buf: Vec<u8>,
}

impl Trace {
	fn stage(&mut self) {
		if self.buf.is_empty() {
			return;
		}
		let buf = std::mem::replace(&mut self.buf, Vec::with_capacity(CHUNK));
		self.inner.send_chunk(self.file, buf);
	}
}

impl io::Write for Trace {
	fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
		self.buf.extend_from_slice(buf);
		if self.buf.len() >= CHUNK {
			self.stage();
		}
		Ok(buf.len())
	}

	fn flush(&mut self) -> io::Result<()> {
		self.stage();
		Ok(())
	}
}

impl Drop for Trace {
	fn drop(&mut self) {
		self.stage();
		self.inner.send(Msg::Close { file: self.file });
	}
}

/// The writer thread: open, append, close, until the last sender is gone.
fn write_traces(rx: mpsc::Receiver<Msg>, queued: &AtomicUsize) {
	use std::io::Write as _;

	let mut files: HashMap<u64, std::fs::File> = HashMap::new();
	for msg in rx {
		match msg {
			Msg::Open { file, path } => match std::fs::File::create(&path) {
				Ok(handle) => {
					files.insert(file, handle);
				}
				Err(err) => tracing::warn!(path = %path.display(), %err, "failed to open a qlog trace"),
			},
			Msg::Data { file, buf } => {
				queued.fetch_sub(buf.len(), Ordering::Relaxed);
				if let Some(handle) = files.get_mut(&file)
					&& let Err(err) = handle.write_all(&buf)
				{
					tracing::warn!(%err, "failed to write a qlog trace");
					files.remove(&file);
				}
			}
			Msg::Close { file } => {
				files.remove(&file);
			}
		}
	}
}
