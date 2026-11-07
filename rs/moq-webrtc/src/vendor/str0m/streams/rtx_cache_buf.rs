#![allow(missing_docs)]

use std::mem;
use std::time::{Duration, Instant};

use crate::util::already_happened;

/// Fixed size buffer that evicts the oldest entries based on time.
///
/// The buffer is ringbuffer-esque in that all elements are inserted with a position that is modulo to an
/// insert index into the buffer. That makes lookups very fast since they are fixed offsets.
///
/// The buffer evicts values based on time. If the size of the buffer is too small and would
/// overwrite an entry that has not been evicted due to age, the buffer grows.
#[derive(Debug)]
pub struct EvictingBuffer<T> {
    buf: Vec<Option<Entry<T>>>,
    /// How long to keep entries for.
    max_age: Duration,
    /// The maximum size allowed to grow to. Once this sized is reached, new pushed
    /// entries will overwrite older entries even if they haven't reached max_age.
    max_size: usize,
    /// Last inserted position
    last_position: Option<u64>,
    /// Next element to evict.
    next_evict: Option<u64>,
    // Last timeout when we evicted elements.
    last_timeout: Instant,
}

/// Entry in the buffer to keep track of position and time separately.
#[derive(Debug)]
struct Entry<T> {
    /// Position is offset modulus the buffer size.
    position: u64,
    /// Entry time. Used for eviction.
    timestamp: Instant,
    /// The value held at this entry.
    value: T,
}

// We don't want to require that T is Clone/Copy, which means we
// must do this instead of using the vec![] macro.
fn prepare_buf<T>(len: usize) -> Vec<Option<T>> {
    let mut buf = Vec::with_capacity(len);
    for _ in 0..len {
        buf.push(None);
    }
    buf
}

impl<T> EvictingBuffer<T> {
    /// Creates a new buffer with an initial size.
    pub fn new(initial_size: usize, max_age: Duration, max_size: usize) -> Self {
        Self {
            buf: prepare_buf(initial_size),
            max_age,
            max_size,
            last_position: None,
            next_evict: None,
            last_timeout: already_happened(),
        }
    }

    fn index_for_position(&self, position: u64) -> usize {
        (position % self.buf.len() as u64) as usize
    }

    #[inline(always)]
    fn is_inert(&self) -> bool {
        self.buf.is_empty() || self.max_age.is_zero()
    }

    /// Push a new entry.
    ///
    /// Position is an increasing sequence number. The sequence can be out of order.
    pub fn push(&mut self, position: u64, timestamp: Instant, value: T) {
        if self.is_inert() {
            return;
        }

        if timestamp < self.last_timeout {
            // Value is already considered evicted.
            return;
        }

        let next_evict = if let Some(v) = self.next_evict {
            v
        } else {
            // First ever cached value sets the initial evict position.
            self.next_evict = Some(position);
            position
        };

        if position < next_evict {
            // Do not cache values preceding evict position.
            return;
        }

        let mut index = self.index_for_position(position);

        if let Some(entry) = &self.buf[index] {
            // If the position is exactly the same, we allow it since it's
            // replacing the current T value. If position differs, we've
            // wrapped around.
            if entry.position != position {
                // Make space to continue.
                self.grow();
                index = self.index_for_position(position);
            }
        }
        self.last_position = Some(position);

        self.buf[index] = Some(Entry {
            position,
            timestamp,
            value,
        });
    }

    /// Get the entry for the previously inserted position.
    #[allow(unused)]
    pub fn get(&self, position: u64) -> Option<&T> {
        if self.is_inert() {
            return None;
        }

        let index = (position % self.buf.len() as u64) as usize;
        if let Some(entry) = &self.buf[index] {
            if entry.position == position {
                return Some(&entry.value);
            }
        }
        None
    }

    pub fn get_mut(&mut self, position: u64) -> Option<&mut T> {
        if self.is_inert() {
            return None;
        }

        let index = (position % self.buf.len() as u64) as usize;
        if let Some(entry) = &mut self.buf[index] {
            if entry.position == position {
                return Some(&mut entry.value);
            }
        }
        None
    }

    pub fn maybe_evict(&mut self, now: Instant) {
        if self.is_inert() {
            return;
        }

        if now < self.last_timeout {
            // Time cannot go backwards.
            return;
        }
        self.last_timeout = now;

        self.evict(now);
    }

    fn evict(&mut self, now: Instant) {
        let Some(start_position) = self.next_evict else {
            // Before first element been pushed.
            return;
        };

        let mut position = start_position;
        let start_index = self.index_for_position(position);

        loop {
            let index = self.index_for_position(position);

            if index == start_index && position > start_position {
                // looped around without finding the end. Means there are no elements.
                break;
            }

            let Some(entry) = &self.buf[index] else {
                // No entry means we might have a gap. We can't break on gaps because
                // there might be entries to evict later. Skip the position and check
                // until we find a timestamp.
                position += 1;
                continue;
            };

            let age = now.saturating_duration_since(entry.timestamp);

            if age > self.max_age {
                // Evict.
                self.buf[index] = None;
            } else {
                // We assume entries are roughly in time order (some jumble is allowed).
                // Once we reach an element we should not evict, stop.
                break;
            }

            position += 1;
        }

        self.next_evict = Some(position);
    }

    fn grow(&mut self) {
        if self.buf.len() >= self.max_size {
            // No growing.
            return;
        }

        // This is the new sized buffer. We can make other strategies for growing.
        let new_size = self
            .max_size
            .min((self.buf.len() * 133) / 100)
            .max(self.buf.len() + 1);

        let old_buffer = mem::replace(&mut self.buf, prepare_buf(new_size));

        // Move all entries over to the new buffer. Changing the buffer size might alter the
        // index position. However max_position and next_evict are already containing positions
        // not index, which means they stay correct.
        for e in old_buffer.into_iter().flatten() {
            let index = self.index_for_position(e.position);
            self.buf[index] = Some(e);
        }
    }

    pub fn contains(&self, position: u64) -> bool {
        if self.is_inert() {
            return false;
        }

        let index = self.index_for_position(position);
        self.buf[index].is_some()
    }

    pub fn last_position(&self) -> Option<u64> {
        if self.is_inert() {
            return None;
        }

        self.last_position
    }

    pub fn last(&self) -> Option<&T> {
        if self.is_inert() {
            return None;
        }

        let last = self.last_position?;
        self.get(last)
    }

    pub fn clear(&mut self) {
        for i in 0..self.buf.len() {
            self.buf[i] = None;
        }
        self.last_position = None;
        self.next_evict = None;
        self.last_timeout = already_happened();
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn push_and_get() {
        let mut buf = EvictingBuffer::new(1, Duration::from_secs(10), 10);
        let now = Instant::now();

        buf.push(5, now, 'A');

        assert_eq!(buf.index_for_position(5), buf.index_for_position(3));
        assert_eq!(buf.get(5), Some(&'A'));
        assert_eq!(buf.get(3), None); // modulo to same index
    }

    #[test]
    fn push_over_capacity() {
        let mut buf = EvictingBuffer::new(2, Duration::from_secs(10), 10);
        let now = Instant::now();

        buf.push(5, now + Duration::from_secs(0), 'A');
        buf.push(6, now + Duration::from_secs(1), 'B');
        buf.push(7, now + Duration::from_secs(2), 'C');

        // The size is 2, we should have grown to accomodate.
        assert_eq!(buf.get(5), Some(&'A'));
        assert_eq!(buf.get(6), Some(&'B'));
        assert_eq!(buf.get(7), Some(&'C'));
    }

    #[test]
    fn push_before_next_evict() {
        let mut buf = EvictingBuffer::new(2, Duration::from_secs(10), 10);
        let now = Instant::now();

        buf.push(6, now + Duration::from_secs(0), 'B');
        assert_eq!(buf.next_evict, Some(6));

        // Before the initial evict position, thus ignored.
        buf.push(5, now + Duration::from_secs(1), 'A');

        assert_eq!(buf.get(5), None);
    }

    #[test]
    fn evict_oldest() {
        let mut buf = EvictingBuffer::new(2, Duration::from_secs(10), 10);
        let now = Instant::now();

        buf.push(5, now + Duration::from_secs(0), 'A');
        buf.push(6, now + Duration::from_secs(1), 'B');

        // Nothing should go
        buf.maybe_evict(now + Duration::from_secs(1));
        assert_eq!(buf.get(5), Some(&'A'));
        assert_eq!(buf.get(6), Some(&'B'));

        // One entry gone.
        buf.maybe_evict(now + Duration::from_secs(11));
        assert_eq!(buf.get(5), None);
        assert_eq!(buf.get(6), Some(&'B'));
    }

    #[test]
    fn evict_with_gap() {
        let mut buf = EvictingBuffer::new(4, Duration::from_secs(10), 10);
        let now = Instant::now();

        buf.push(5, now + Duration::from_secs(0), 'A');
        // GAP
        buf.push(7, now + Duration::from_secs(2), 'C');
        buf.push(8, now + Duration::from_secs(3), 'D');

        // Should evict A and C
        buf.maybe_evict(now + Duration::from_secs(13));

        assert_eq!(buf.get(5), None);
        assert_eq!(buf.get(7), None);
        assert_eq!(buf.get(8), Some(&'D'));
    }

    #[test]
    fn evict_all() {
        let mut buf = EvictingBuffer::new(4, Duration::from_secs(10), 10);
        let now = Instant::now();

        buf.push(5, now + Duration::from_secs(0), 'A');
        buf.push(6, now + Duration::from_secs(1), 'B');

        // Should evict A and B
        buf.maybe_evict(now + Duration::from_secs(12));

        assert_eq!(buf.get(5), None);
        assert_eq!(buf.get(6), None);
    }

    fn buffer_cmp(b: &EvictingBuffer<char>) -> Vec<Option<char>> {
        b.buf.iter().map(|e| e.as_ref().map(|v| v.value)).collect()
    }

    #[test]
    fn grow_and_reindex() {
        let mut buf = EvictingBuffer::new(2, Duration::from_secs(10), 10);
        let now = Instant::now();

        buf.push(2, now + Duration::from_secs(0), 'A');
        buf.push(3, now + Duration::from_secs(1), 'B');

        assert_eq!(buffer_cmp(&buf), &[Some('A'), Some('B')]);

        // overwrites 2, thus grows
        buf.push(4, now + Duration::from_secs(2), 'C');

        assert_eq!(buffer_cmp(&buf), &[Some('B'), Some('C'), Some('A')]);

        assert_eq!(buf.get(2), Some(&'A'));
        assert_eq!(buf.get(3), Some(&'B'));
        assert_eq!(buf.get(4), Some(&'C'));
    }
}
