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

/// A process-local id for each sink, keeping filenames unique when multiple
/// worker groups start during the same millisecond.
static NEXT_SINK: AtomicU64 = AtomicU64::new(0);

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
	/// Files are named `moq-<started>-<process>-<sink>-<id>-<side>.qlog`, where
	/// `started` is the sink's creation time in milliseconds since the epoch
	/// and `id` names the connection.
	pub fn directory(dir: impl Into<PathBuf>) -> Result<Self, Error> {
		let dir = dir.into();
		let process = std::process::id();
		let sink = NEXT_SINK.fetch_add(1, Ordering::Relaxed);
		let started = std::time::SystemTime::now()
			.duration_since(std::time::UNIX_EPOCH)
			.unwrap_or_default()
			.as_millis();

		// A read-only or missing directory is reported here, where the
		// operator set it, rather than as a warning per connection once the
		// traces they went looking for are already not being written.
		let probe = dir.join(format!(".moq-qlog-{process}-{sink}-{started}"));
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
				process,
				sink,
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
		let path = inner.dir.join(format!(
			"moq-{}-{}-{}-{id}-{}.qlog",
			inner.started,
			inner.process,
			inner.sink,
			side.as_str()
		));
		let queued = std::mem::size_of::<Msg>() + path.capacity();
		let close_queued = std::mem::size_of::<Msg>();
		if !inner.reserve(queued + close_queued) {
			inner.warn_dropped();
			return Box::new(io::sink());
		}
		if !inner.send(Msg::Open { file, path, queued }) {
			inner.release(queued + close_queued);
			return Box::new(io::sink());
		}
		Box::new(Trace {
			inner,
			file,
			close_queued,
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
	/// Process id in every filename.
	process: u32,
	/// Process-local sink id in every filename.
	sink: u64,
	/// `Option` so [`Drop`] can close the channel before joining the thread.
	tx: Option<mpsc::Sender<Msg>>,
	/// Memory handed to the thread or reserved for a trace's close message.
	queued: Arc<AtomicUsize>,
	/// The next trace's slot in the writer thread's table.
	next: AtomicU64,
	/// Whether a chunk has been dropped, so the warning is logged once.
	dropped: AtomicBool,
	/// `Option` for the same reason as `tx`: [`Drop`] takes it to join.
	thread: Option<std::thread::JoinHandle<()>>,
}

impl Inner {
	fn send(&self, msg: Msg) -> bool {
		if let Some(tx) = &self.tx {
			return tx.send(msg).is_ok();
		}
		false
	}

	fn reserve(&self, bytes: usize) -> bool {
		let mut queued = self.queued.load(Ordering::Relaxed);
		loop {
			let Some(next) = queued.checked_add(bytes).filter(|next| *next <= QUEUED_MAX) else {
				return false;
			};
			match self
				.queued
				.compare_exchange_weak(queued, next, Ordering::Relaxed, Ordering::Relaxed)
			{
				Ok(_) => return true,
				Err(actual) => queued = actual,
			}
		}
	}

	fn release(&self, bytes: usize) {
		self.queued.fetch_sub(bytes, Ordering::Relaxed);
	}

	fn warn_dropped(&self) {
		if !self.dropped.swap(true, Ordering::Relaxed) {
			tracing::warn!("dropping qlog output: over {QUEUED_MAX} bytes are queued for the writer");
		}
	}

	/// Queue a chunk, dropping it when the writer thread is too far behind.
	fn send_chunk(&self, file: u64, buf: Vec<u8>) {
		let queued = std::mem::size_of::<Msg>() + buf.capacity();
		if !self.reserve(queued) {
			self.warn_dropped();
			return;
		}
		if !self.send(Msg::Data { file, buf, queued }) {
			self.release(queued);
		}
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
	Open { file: u64, path: PathBuf, queued: usize },
	Data { file: u64, buf: Vec<u8>, queued: usize },
	Close { file: u64, queued: usize },
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
	/// Queue space reserved when the trace opens, guaranteeing its close.
	close_queued: usize,
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
	fn write(&mut self, mut buf: &[u8]) -> io::Result<usize> {
		let written = buf.len();
		while !buf.is_empty() {
			let len = (CHUNK - self.buf.len()).min(buf.len());
			self.buf.extend_from_slice(&buf[..len]);
			buf = &buf[len..];
			if self.buf.len() == CHUNK {
				self.stage();
			}
		}
		Ok(written)
	}

	fn flush(&mut self) -> io::Result<()> {
		self.stage();
		Ok(())
	}
}

impl Drop for Trace {
	fn drop(&mut self) {
		self.stage();
		if !self.inner.send(Msg::Close {
			file: self.file,
			queued: self.close_queued,
		}) {
			self.inner.release(self.close_queued);
		}
	}
}

/// The writer thread: open, append, close, until the last sender is gone.
fn write_traces(rx: mpsc::Receiver<Msg>, queued: &AtomicUsize) {
	use std::io::Write as _;

	let mut files: HashMap<u64, std::fs::File> = HashMap::new();
	for msg in rx {
		match msg {
			Msg::Open {
				file,
				path,
				queued: accounted,
			} => {
				queued.fetch_sub(accounted, Ordering::Relaxed);
				match std::fs::File::create(&path) {
					Ok(handle) => {
						files.insert(file, handle);
					}
					Err(err) => tracing::warn!(path = %path.display(), %err, "failed to open a qlog trace"),
				}
			}
			Msg::Data {
				file,
				buf,
				queued: accounted,
			} => {
				queued.fetch_sub(accounted, Ordering::Relaxed);
				if let Some(handle) = files.get_mut(&file)
					&& let Err(err) = handle.write_all(&buf)
				{
					tracing::warn!(%err, "failed to write a qlog trace");
					files.remove(&file);
				}
			}
			Msg::Close {
				file,
				queued: accounted,
			} => {
				queued.fetch_sub(accounted, Ordering::Relaxed);
				files.remove(&file);
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use std::io::Write as _;

	use super::*;

	#[test]
	fn concurrent_sinks_use_distinct_files() {
		let dir = tempfile::tempdir().expect("temp dir");
		let first = Sink::directory(dir.path()).expect("first sink");
		let second = Sink::directory(dir.path()).expect("second sink");

		let mut first_trace = first.endpoint_trace(Side::Server);
		let mut second_trace = second.endpoint_trace(Side::Server);
		first_trace.write_all(b"first").expect("write first trace");
		second_trace.write_all(b"second").expect("write second trace");
		drop(first_trace);
		drop(second_trace);
		drop(first);
		drop(second);

		let mut contents: Vec<_> = std::fs::read_dir(dir.path())
			.expect("read qlog directory")
			.map(|entry| std::fs::read(entry.expect("directory entry").path()).expect("read qlog file"))
			.collect();
		contents.sort();
		assert_eq!(contents, [b"first".to_vec(), b"second".to_vec()]);
	}

	#[test]
	fn an_open_trace_reserves_its_close_message() {
		let dir = tempfile::tempdir().expect("temp dir");
		let sink = Sink::directory(dir.path()).expect("sink");
		let trace = sink.endpoint_trace(Side::Server);

		let close_queued = std::mem::size_of::<Msg>();
		for _ in 0..10_000 {
			if sink.inner.queued.load(Ordering::Relaxed) == close_queued {
				break;
			}
			std::thread::yield_now();
		}
		assert_eq!(sink.inner.queued.load(Ordering::Relaxed), close_queued);

		drop(trace);
		drop(sink);
	}

	#[test]
	fn an_oversized_write_is_split_before_queueing() {
		let (tx, rx) = mpsc::channel();
		let inner = Arc::new(Inner {
			dir: PathBuf::new(),
			started: 0,
			process: 0,
			sink: 0,
			tx: Some(tx),
			queued: Arc::new(AtomicUsize::new(0)),
			next: AtomicU64::new(0),
			dropped: AtomicBool::new(false),
			thread: None,
		});
		let mut trace = Trace {
			inner,
			file: 0,
			close_queued: 0,
			buf: Vec::with_capacity(CHUNK),
		};

		trace.write_all(&vec![0; CHUNK * 2 + 1]).expect("write trace");
		for _ in 0..2 {
			let Msg::Data { buf, .. } = rx.recv().expect("queued chunk") else {
				panic!("expected trace data");
			};
			assert_eq!(buf.len(), CHUNK);
		}
		assert_eq!(trace.buf.len(), 1);
	}
}
