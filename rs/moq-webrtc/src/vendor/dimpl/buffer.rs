//! Buffer management for efficient memory reuse.
//!
//! This module provides buffer types used throughout dimpl for managing byte data
//! with minimal allocations. The [`BufferPool`] allows reusing buffers, and [`Buf`]
//! wraps `Vec<u8>` with convenient operations for protocol data handling.

use std::collections::VecDeque;
use std::fmt;
use std::ops::{Deref, DerefMut};

/// Buffer pool for reusing allocated buffers.
///
/// This pool manages a collection of reusable `Buf` instances to reduce allocations
/// during DTLS operations. Buffers are returned to the pool when no longer needed
/// and can be reused for subsequent operations.
#[derive(Default)]
pub struct BufferPool {
    free: VecDeque<Buf>,
}

impl BufferPool {
    /// Take a Buffer from the pool.
    ///
    /// Creates a new buffer if none is free.
    pub fn pop(&mut self) -> Buf {
        let mut buffer = self.free.pop_front().unwrap_or_default();
        buffer.from_pool = true;
        buffer
    }

    /// Return a buffer to the pool.
    ///
    /// Panics if the buffer was not acquired from a pool.
    pub fn push(&mut self, mut buffer: Buf) {
        assert!(
            buffer.from_pool,
            "only buffers acquired from a pool may be returned"
        );
        buffer.clear();
        self.free.push_front(buffer);
    }
}

impl fmt::Debug for BufferPool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BufferPool")
            .field("free", &self.free.len())
            .finish()
    }
}

/// Growable buffer wrapper used throughout dimpl for efficient memory management.
///
/// This wrapper around `Vec<u8>` provides convenient access to byte buffers
/// and integrates with dimpl's buffer pooling system.
/// When filling a borrowed `&mut Buf`, mutate its contents instead of replacing
/// the buffer so its pool provenance is preserved.
#[derive(Default)]
pub struct Buf {
    data: Vec<u8>,
    from_pool: bool,
}

impl Buf {
    /// Create an empty buffer for temporary or independently owned data.
    ///
    /// This buffer must not be returned to a pool. Recyclable buffers must be
    /// acquired from their pool.
    pub fn new() -> Self {
        Self::default()
    }

    /// Clear the buffer, removing all data.
    pub fn clear(&mut self) {
        self.data.clear();
    }

    /// Extend the buffer with a slice of bytes.
    pub fn extend_from_slice(&mut self, other: &[u8]) {
        self.data.extend_from_slice(other);
    }

    /// Push a single byte onto the buffer.
    pub fn push(&mut self, byte: u8) {
        self.data.push(byte);
    }

    /// Resize the buffer to the specified length, filling with the given value.
    pub fn resize(&mut self, len: usize, value: u8) {
        self.data.resize(len, value);
    }

    /// Convert the buffer into the underlying `Vec<u8>`.
    pub fn into_vec(self) -> Vec<u8> {
        self.data
    }
}

// aws-lc-rs AEAD operations require Extend<&u8> for appending authentication tags
impl<'a> Extend<&'a u8> for Buf {
    fn extend<T: IntoIterator<Item = &'a u8>>(&mut self, iter: T) {
        self.data.extend(iter.into_iter().copied());
    }
}

impl Deref for Buf {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

impl DerefMut for Buf {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.data
    }
}

impl AsRef<[u8]> for Buf {
    fn as_ref(&self) -> &[u8] {
        &self.data
    }
}

impl AsMut<[u8]> for Buf {
    fn as_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }
}

impl fmt::Debug for Buf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Buf")
            .field("len", &self.data.len())
            .finish()
    }
}

/// Trait for types that can be converted into a `Buf`.
pub trait ToBuf {
    /// Convert this value into a `Buf`.
    fn to_buf(self) -> Buf;
}

impl ToBuf for Vec<u8> {
    fn to_buf(self) -> Buf {
        Buf {
            data: self,
            from_pool: false,
        }
    }
}

impl ToBuf for &[u8] {
    fn to_buf(self) -> Buf {
        self.to_vec().to_buf()
    }
}

/// Temporary mutable buffer wrapper for in-place operations.
///
/// Used primarily for decryption operations where data needs to be modified in-place.
/// Provides mutable access to a slice with tracked length.
#[allow(clippy::len_without_is_empty)]
pub struct TmpBuf<'a>(&'a mut [u8], usize);

impl core::fmt::Debug for TmpBuf<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TmpBuf").field("data_len", &self.1).finish()
    }
}

impl<'a> TmpBuf<'a> {
    /// Create a new temporary buffer from a mutable slice.
    pub fn new(buf: &'a mut [u8]) -> Self {
        Self(buf, buf.len())
    }
}

impl<'a> TmpBuf<'a> {
    /// Get the length of the buffer
    pub fn len(&self) -> usize {
        self.1
    }

    /// Truncate the buffer to the specified length
    pub fn truncate(&mut self, len: usize) {
        self.1 = len;
    }
}

impl<'a> AsRef<[u8]> for TmpBuf<'a> {
    fn as_ref(&self) -> &[u8] {
        &self.0[..self.1]
    }
}

impl<'a> AsMut<[u8]> for TmpBuf<'a> {
    fn as_mut(&mut self) -> &mut [u8] {
        &mut self.0[..self.1]
    }
}

#[cfg(feature = "rust-crypto")]
impl<'a> aes_gcm::aead::Buffer for TmpBuf<'a> {
    fn extend_from_slice(&mut self, other: &[u8]) -> Result<(), aes_gcm::aead::Error> {
        // Check if there's enough capacity in the underlying slice
        let available = self.0.len() - self.1;
        if available < other.len() {
            return Err(aes_gcm::aead::Error);
        }
        // Copy the data into the buffer
        self.0[self.1..self.1 + other.len()].copy_from_slice(other);
        self.1 += other.len();
        Ok(())
    }

    fn truncate(&mut self, len: usize) {
        if len <= self.1 {
            self.1 = len;
        }
    }
}

/// Implement the `aead::Buffer` trait for `Buf` to support in-place AEAD operations.
#[cfg(feature = "rust-crypto")]
impl aes_gcm::aead::Buffer for Buf {
    fn extend_from_slice(&mut self, other: &[u8]) -> Result<(), aes_gcm::aead::Error> {
        self.data.extend_from_slice(other);
        Ok(())
    }

    fn truncate(&mut self, len: usize) {
        self.data.truncate(len);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pooled_buffers_keep_capacity_and_are_cleared() {
        let mut pool = BufferPool::default();
        let mut buffer = pool.pop();
        buffer.extend_from_slice(&[0xAA; 256]);
        let allocation = buffer.as_ptr();
        pool.push(buffer);

        let reused = pool.pop();
        assert!(reused.is_empty());
        assert_eq!(reused.as_ptr(), allocation);
        assert!(reused.into_vec().capacity() >= 256);
    }

    #[test]
    #[should_panic(expected = "only buffers acquired from a pool may be returned")]
    fn rejects_unpooled_buffer() {
        BufferPool::default().push(Buf::new());
    }

    #[test]
    #[should_panic(expected = "only buffers acquired from a pool may be returned")]
    fn rejects_converted_buffer() {
        BufferPool::default().push(vec![0xAA; 256].to_buf());
    }

    #[test]
    #[should_panic(expected = "only buffers acquired from a pool may be returned")]
    fn converting_to_vec_does_not_keep_pool_provenance() {
        let mut pool = BufferPool::default();
        let buffer = pool.pop();
        pool.push(buffer.into_vec().to_buf());
    }

    #[test]
    #[should_panic(expected = "only buffers acquired from a pool may be returned")]
    fn taking_buffer_does_not_make_replacement_pooled() {
        let mut pool = BufferPool::default();
        let mut buffer = pool.pop();
        pool.push(std::mem::take(&mut buffer));
        pool.push(buffer);
    }
}
