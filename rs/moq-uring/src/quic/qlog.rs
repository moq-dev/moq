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

/// Most estimated qlog memory held at once, across every trace.
///
/// This covers worker-side staging buffers, queued messages and their owned
/// buffers, and the control messages reserved for live traces. A disk that
/// cannot keep up must not grow any of them without bound, and blocking the
/// worker is the one thing this sink exists to avoid, so traces and chunks past
/// the ceiling are dropped. JSON-SEQ records are newline delimited, so a reader
/// resynchronizes after the gap.
const MEMORY_MAX: usize = 64 * 1024 * 1024;

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
	/// Files are named `moq-<started>-<process>-<sink>-<trace>-<id>-<side>.qlog`,
	/// where `started` is the sink's creation time in milliseconds since the
	/// epoch and `trace` keeps reused connection ids distinct.
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
		let memory = Arc::new(AtomicUsize::new(0));
		let thread = std::thread::Builder::new()
			.name("moq-qlog".to_string())
			.spawn({
				let memory = memory.clone();
				move || write_traces(rx, &memory)
			})
			.map_err(|err| Error::Qlog(format!("failed to spawn the qlog writer: {err}")))?;

		Ok(Self {
			inner: Arc::new(Inner {
				dir,
				started,
				process,
				sink,
				tx: Some(tx),
				memory,
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
		// The trace slot stays unique within the sink even when a peer reuses
		// the same Initial destination connection id.
		let id = match cid {
			Some(cid) => {
				let cid = cid.iter().fold(String::with_capacity(cid.len() * 2), |mut id, byte| {
					let _ = write!(id, "{byte:02x}");
					id
				});
				format!("connection{file}-{cid}")
			}
			None => format!("endpoint{file}"),
		};
		let path = inner.dir.join(format!(
			"moq-{}-{}-{}-{id}-{}.qlog",
			inner.started,
			inner.process,
			inner.sink,
			side.as_str()
		));
		let open_accounted = std::mem::size_of::<Msg>() + path.capacity();
		let close_accounted = std::mem::size_of::<Msg>();
		let staging_accounted = CHUNK;
		if !inner.reserve(open_accounted + close_accounted + staging_accounted) {
			inner.warn_dropped();
			return Box::new(io::sink());
		}
		if !inner.send(Msg::Open {
			file,
			path,
			accounted: open_accounted,
		}) {
			inner.release(open_accounted + close_accounted + staging_accounted);
			return Box::new(io::sink());
		}
		Box::new(Trace {
			inner,
			file,
			close_accounted,
			staging_accounted,
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
	/// Estimated memory staged, queued, or reserved across all traces.
	memory: Arc<AtomicUsize>,
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
		let mut memory = self.memory.load(Ordering::Relaxed);
		loop {
			let Some(next) = memory.checked_add(bytes).filter(|next| *next <= MEMORY_MAX) else {
				return false;
			};
			match self
				.memory
				.compare_exchange_weak(memory, next, Ordering::Relaxed, Ordering::Relaxed)
			{
				Ok(_) => return true,
				Err(actual) => memory = actual,
			}
		}
	}

	fn release(&self, bytes: usize) {
		self.memory.fetch_sub(bytes, Ordering::Relaxed);
	}

	fn warn_dropped(&self) {
		if !self.dropped.swap(true, Ordering::Relaxed) {
			tracing::warn!("dropping qlog output: over {MEMORY_MAX} bytes of trace memory are in use");
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
	Open { file: u64, path: PathBuf, accounted: usize },
	Data { file: u64, buf: Vec<u8>, accounted: usize },
	Close { file: u64, accounted: usize },
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
	/// Memory reserved when the trace opens, guaranteeing its close.
	close_accounted: usize,
	/// Memory reserved for the trace's retained staging buffer.
	staging_accounted: usize,
	buf: Vec<u8>,
}

impl Trace {
	fn stage(&mut self) {
		if self.buf.is_empty() {
			return;
		}
		// The old buffer transfers its staging reservation to the Data message.
		// Reserve the replacement buffer and message before allocating either.
		let accounted = std::mem::size_of::<Msg>() + self.staging_accounted;
		if !self.inner.reserve(accounted) {
			self.inner.warn_dropped();
			self.buf.clear();
			return;
		}
		let buf = std::mem::replace(&mut self.buf, Vec::with_capacity(self.staging_accounted));
		debug_assert_eq!(buf.capacity(), self.staging_accounted);
		if !self.inner.send(Msg::Data {
			file: self.file,
			buf,
			accounted,
		}) {
			self.inner.release(accounted);
		}
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
		drop(std::mem::take(&mut self.buf));
		self.inner.release(self.staging_accounted);
		if !self.inner.send(Msg::Close {
			file: self.file,
			accounted: self.close_accounted,
		}) {
			self.inner.release(self.close_accounted);
		}
	}
}

/// The writer thread: open, append, close, until the last sender is gone.
fn write_traces(rx: mpsc::Receiver<Msg>, memory: &AtomicUsize) {
	use std::io::Write as _;

	let mut files: HashMap<u64, std::fs::File> = HashMap::new();
	for msg in rx {
		match msg {
			Msg::Open { file, path, accounted } => {
				match std::fs::File::create(&path) {
					Ok(handle) => {
						files.insert(file, handle);
					}
					Err(err) => tracing::warn!(path = %path.display(), %err, "failed to open a qlog trace"),
				}
				drop(path);
				memory.fetch_sub(accounted, Ordering::Relaxed);
			}
			Msg::Data { file, buf, accounted } => {
				if let Some(handle) = files.get_mut(&file)
					&& let Err(err) = handle.write_all(&buf)
				{
					tracing::warn!(%err, "failed to write a qlog trace");
					files.remove(&file);
				}
				drop(buf);
				memory.fetch_sub(accounted, Ordering::Relaxed);
			}
			Msg::Close { file, accounted } => {
				files.remove(&file);
				memory.fetch_sub(accounted, Ordering::Relaxed);
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
	fn reused_connection_ids_use_distinct_files() {
		let dir = tempfile::tempdir().expect("temp dir");
		let sink = Sink::directory(dir.path()).expect("sink");

		let mut first = sink.trace(b"same", Side::Server);
		first.write_all(b"first").expect("write first trace");
		drop(first);
		let mut second = sink.trace(b"same", Side::Server);
		second.write_all(b"second").expect("write second trace");
		drop(second);
		drop(sink);

		let mut contents: Vec<_> = std::fs::read_dir(dir.path())
			.expect("read qlog directory")
			.map(|entry| std::fs::read(entry.expect("directory entry").path()).expect("read qlog file"))
			.collect();
		contents.sort();
		assert_eq!(contents, [b"first".to_vec(), b"second".to_vec()]);
	}

	#[test]
	fn an_open_trace_accounts_for_staging_and_close() {
		let dir = tempfile::tempdir().expect("temp dir");
		let sink = Sink::directory(dir.path()).expect("sink");
		let trace = sink.endpoint_trace(Side::Server);

		let accounted = CHUNK + std::mem::size_of::<Msg>();
		for _ in 0..10_000 {
			if sink.inner.memory.load(Ordering::Relaxed) == accounted {
				break;
			}
			std::thread::yield_now();
		}
		assert_eq!(sink.inner.memory.load(Ordering::Relaxed), accounted);

		drop(trace);
		drop(sink);
	}

	#[test]
	fn a_trace_without_staging_memory_is_dropped() {
		let dir = tempfile::tempdir().expect("temp dir");
		let sink = Sink::directory(dir.path()).expect("sink");
		let held = MEMORY_MAX - CHUNK + 1;
		assert!(sink.inner.reserve(held));

		let mut trace = sink.endpoint_trace(Side::Server);
		trace.write_all(b"dropped").expect("write dropped trace");
		drop(trace);
		sink.inner.release(held);
		drop(sink);

		assert_eq!(std::fs::read_dir(dir.path()).expect("read qlog directory").count(), 0);
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
			memory: Arc::new(AtomicUsize::new(CHUNK)),
			next: AtomicU64::new(0),
			dropped: AtomicBool::new(false),
			thread: None,
		});
		let mut trace = Trace {
			inner,
			file: 0,
			close_accounted: 0,
			staging_accounted: CHUNK,
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
