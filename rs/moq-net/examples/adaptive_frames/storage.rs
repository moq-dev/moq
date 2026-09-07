//! Experimental append-only storage; polling is explicit, without executor notification.
use bytes::{BufMut, Bytes, BytesMut, buf::UninitSlice};
use std::sync::{Arc, Mutex};

const PAGE_MAX: usize = 64 * 1024;
const FRAME_MAX: usize = 32 * 1024 * 1024;

#[derive(Clone, Copy, Debug)]
/// Where the prototype stores its fixed-size frame headers.
pub(super) enum Layout {
	Indexed,
	Packed,
}

#[derive(Clone, Copy, Debug, PartialEq)]
/// Timestamp ticks in a common group timescale and declared payload bytes.
pub(super) struct Header {
	/// Ticks in the group timescale.
	pub timestamp: u64,
	/// Declared payload length in bytes.
	pub size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
/// Terminal stream failure or a rejected operation.
pub(super) enum Error {
	Closed,
	TooLarge,
	WrongSize,
	Aborted,
}

type Result<T> = std::result::Result<T, Error>;

struct State {
	headers: Vec<Header>,
	chunks: Vec<Bytes>,
	committed: usize,
	finished: bool,
	aborted: bool,
}

/// Sole writer with shared published state and an exclusively owned mutable tail.
pub(super) struct Group {
	state: Arc<Mutex<State>>,
	layout: Layout,
	tail: BytesMut,
	previous_page: usize,
	pages: usize,
	capacity: usize,
}

#[derive(Debug)]
/// Backing capacity, excluding allocator bookkeeping, shared state, and external buffers.
pub(super) struct Footprint {
	/// Number of backing pages allocated.
	pub pages: usize,
	/// Sum of backing page capacities allocated.
	pub page_bytes: usize,
	/// Capacity in bytes of the header vector.
	pub header_bytes: usize,
	/// Capacity in bytes of the published chunk vector.
	pub chunk_bytes: usize,
	/// Number of published byte slices, including page splits.
	pub chunks: usize,
}

impl Group {
	/// Create an empty group without a payload allocation.
	pub fn new(layout: Layout) -> Self {
		Self {
			state: Arc::new(Mutex::new(State {
				headers: Vec::new(),
				chunks: Vec::new(),
				committed: 0,
				finished: false,
				aborted: false,
			})),
			layout,
			tail: BytesMut::new(),
			previous_page: 0,
			pages: 0,
			capacity: 0,
		}
	}

	/// Create an independent sequential cursor.
	pub fn consumer(&self) -> Consumer {
		Consumer {
			state: self.state.clone(),
			layout: self.layout,
			frame: 0,
			chunk: 0,
			offset: 0,
			abandoned: false,
		}
	}

	fn reserve_page(&mut self, hint: usize) {
		if self.tail.capacity() > self.tail.len() {
			return;
		}
		self.publish();
		let first = hint.clamp(128, PAGE_MAX).next_power_of_two().min(PAGE_MAX);
		let size = if self.previous_page == 0 {
			first
		} else {
			(self.previous_page * 2).min(PAGE_MAX)
		};
		self.tail = BytesMut::with_capacity(size);
		self.previous_page = size;
		self.pages += 1;
		self.capacity += self.tail.capacity();
	}

	fn append(&mut self, mut bytes: &[u8]) {
		while !bytes.is_empty() {
			self.reserve_page(bytes.len());
			let n = bytes.len().min(self.tail.capacity() - self.tail.len());
			self.tail.extend_from_slice(&bytes[..n]);
			bytes = &bytes[n..];
		}
	}

	/// Declare a frame and exclusively borrow the writer until completion.
	pub fn create(&mut self, header: Header) -> Result<Writer<'_>> {
		if header.size > FRAME_MAX as u64 {
			return Err(Error::TooLarge);
		}
		{
			let state = self.state.lock().unwrap();
			if state.aborted || state.finished {
				return Err(Error::Closed);
			}
		}
		if matches!(self.layout, Layout::Indexed) {
			self.state.lock().unwrap().headers.push(header);
		} else {
			self.reserve_page(header.size as usize + 16);
			self.append(&header.timestamp.to_le_bytes());
			self.append(&header.size.to_le_bytes());
		}
		Ok(Writer {
			group: self,
			remaining: header.size as usize,
			done: false,
		})
	}

	/// Share initialized bytes while retaining exclusive access to the unused tail.
	pub fn publish(&mut self) {
		if !self.tail.is_empty() {
			self.state.lock().unwrap().chunks.push(self.tail.split().freeze());
		}
	}

	/// Publish remaining bytes and seal the group.
	pub fn finish(&mut self) -> Result<()> {
		self.publish();
		let mut state = self.state.lock().unwrap();
		if state.aborted || state.finished {
			return Err(Error::Closed);
		}
		state.finished = true;
		Ok(())
	}

	fn abort(&mut self) {
		let mut state = self.state.lock().unwrap();
		state.aborted = true;
		state.headers.clear();
		state.chunks.clear();
		self.tail = BytesMut::new();
	}

	/// Report page and metadata capacities for the experiment.
	pub fn footprint(&self) -> Footprint {
		let state = self.state.lock().unwrap();
		Footprint {
			pages: self.pages,
			page_bytes: self.capacity,
			header_bytes: state.headers.capacity() * std::mem::size_of::<Header>(),
			chunk_bytes: state.chunks.capacity() * std::mem::size_of::<Bytes>(),
			chunks: state.chunks.len(),
		}
	}
}

impl Drop for Group {
	fn drop(&mut self) {
		if !self.state.lock().unwrap().finished {
			self.abort();
		}
	}
}

/// Bounded BufMut view; finish commits metadata, publish exposes pending payload.
pub(super) struct Writer<'a> {
	group: &'a mut Group,
	remaining: usize,
	done: bool,
}

impl Writer<'_> {
	/// Share initialized bytes while retaining exclusive access to the unused tail.
	pub fn publish(&mut self) {
		self.group.publish();
	}

	/// Append an existing slice without copying its backing allocation.
	pub fn write_owned(&mut self, bytes: Bytes) -> Result<()> {
		if bytes.len() > self.remaining {
			return Err(Error::WrongSize);
		}
		if bytes.is_empty() {
			return Ok(());
		}
		self.group.publish();
		self.remaining -= bytes.len();
		self.group.state.lock().unwrap().chunks.push(bytes);
		Ok(())
	}

	/// Complete this frame; incomplete completion fails and invalidates the handle.
	pub fn finish(mut self) -> Result<()> {
		if self.remaining != 0 {
			return Err(Error::WrongSize);
		}
		self.group.state.lock().unwrap().committed += 1;
		self.done = true;
		Ok(())
	}
}

impl Drop for Writer<'_> {
	fn drop(&mut self) {
		if !self.done {
			self.group.abort();
		}
	}
}

// Safety: the mutable tail has exclusive ownership of its region. Every exposed
// chunk is bounded by the declared frame size, and advancing delegates to BytesMut.
unsafe impl BufMut for Writer<'_> {
	fn remaining_mut(&self) -> usize {
		self.remaining
	}
	fn chunk_mut(&mut self) -> &mut UninitSlice {
		if self.remaining == 0 {
			return UninitSlice::new(&mut []);
		}
		self.group.reserve_page(self.remaining);
		let n = self.remaining.min(self.group.tail.capacity() - self.group.tail.len());
		&mut self.group.tail.chunk_mut()[..n]
	}
	unsafe fn advance_mut(&mut self, cnt: usize) {
		assert!(cnt <= self.remaining);
		// Safety: BufMut's caller guarantees initialization of the advanced region.
		unsafe {
			self.group.tail.advance_mut(cnt);
		}
		self.remaining -= cnt;
	}
}

#[derive(Clone)]
/// Sequential reader; abandoning an open frame closes only this cursor.
pub(super) struct Consumer {
	state: Arc<Mutex<State>>,
	layout: Layout,
	frame: usize,
	chunk: usize,
	offset: usize,
	abandoned: bool,
}

#[derive(Debug, PartialEq)]
/// Explicit poll result; this storage prototype has no waker registration.
pub(super) enum Read<T> {
	Ready(T),
	Pending,
	End,
}

impl Consumer {
	fn read_bytes(&mut self, max: usize) -> Result<Read<Bytes>> {
		let state = self.state.lock().unwrap();
		if state.aborted {
			return Err(Error::Aborted);
		}
		while let Some(bytes) = state.chunks.get(self.chunk) {
			if self.offset == bytes.len() {
				self.chunk += 1;
				self.offset = 0;
				continue;
			}
			let end = (self.offset + max).min(bytes.len());
			let result = bytes.slice(self.offset..end);
			self.offset = end;
			return Ok(Read::Ready(result));
		}
		Ok(if state.finished { Read::End } else { Read::Pending })
	}

	/// Borrow the next frame when its header is published.
	pub fn next(&mut self) -> Result<Read<Reader<'_>>> {
		if self.abandoned {
			return Err(Error::Closed);
		}
		let header = match self.layout {
			Layout::Indexed => {
				let state = self.state.lock().unwrap();
				if state.aborted {
					return Err(Error::Aborted);
				}
				match state.headers.get(self.frame) {
					Some(header) => *header,
					None => return Ok(if state.finished { Read::End } else { Read::Pending }),
				}
			}
			Layout::Packed => {
				let mut trial = self.clone();
				let mut raw = [0; 16];
				let mut n = 0;
				while n < raw.len() {
					match trial.read_bytes(raw.len() - n)? {
						Read::Ready(bytes) => {
							raw[n..n + bytes.len()].copy_from_slice(&bytes);
							n += bytes.len();
						}
						Read::Pending => return Ok(Read::Pending),
						Read::End if n == 0 => return Ok(Read::End),
						Read::End => return Err(Error::WrongSize),
					}
				}
				self.chunk = trial.chunk;
				self.offset = trial.offset;
				Header {
					timestamp: u64::from_le_bytes(raw[..8].try_into().unwrap()),
					size: u64::from_le_bytes(raw[8..].try_into().unwrap()),
				}
			}
		};
		Ok(Read::Ready(Reader {
			consumer: self,
			header,
			read: 0,
			done: false,
		}))
	}
}

/// Borrowed payload cursor supporting frames that cross page boundaries.
pub(super) struct Reader<'a> {
	consumer: &'a mut Consumer,
	/// Declared frame metadata.
	pub header: Header,
	read: u64,
	done: bool,
}
impl Reader<'_> {
	/// Return available bytes, pending production, or committed end of frame.
	pub fn read_chunk(&mut self) -> Result<Read<Bytes>> {
		if self.read == self.header.size {
			let state = self.consumer.state.lock().unwrap();
			if state.aborted {
				return Err(Error::Aborted);
			}
			return Ok(if self.consumer.frame < state.committed {
				Read::End
			} else {
				Read::Pending
			});
		}
		match self.consumer.read_bytes((self.header.size - self.read) as usize)? {
			Read::Ready(bytes) => {
				self.read += bytes.len() as u64;
				Ok(Read::Ready(bytes))
			}
			Read::End => Err(Error::WrongSize),
			Read::Pending => Ok(Read::Pending),
		}
	}
	/// Complete this frame; incomplete completion fails and invalidates the handle.
	pub fn finish(mut self) -> Result<()> {
		if !matches!(self.read_chunk()?, Read::End) {
			return Err(Error::WrongSize);
		}
		self.consumer.frame += 1;
		self.done = true;
		Ok(())
	}
}

impl Drop for Reader<'_> {
	fn drop(&mut self) {
		if !self.done {
			self.consumer.abandoned = true;
		}
	}
}
