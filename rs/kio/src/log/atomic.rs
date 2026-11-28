//! The log's atomics, with the ghost permissions Verus reasons about.
//!
//! Each type wraps a std atomic and, under Verus, pairs it with a permission whose
//! `value` is the atomic's current value: an operation needs the permission, so the
//! proof always knows what a load returns. Plain builds have no permissions; the
//! wrappers compile to the std operation with the ordering the caller names.
//!
//! These bodies are trusted. Verus models every atomic access as sequentially
//! consistent, so the proof assumes the orderings the log names carry what it
//! publishes: a `Release` store makes the writes before it visible to the `Acquire`
//! load that reads it. The stores require `Release` so a weakened publish fails the
//! proof; whether each load's ordering suffices is Miri's and GenMC's to check. The
//! loads rule out `Release` and `AcqRel`, which std's loads panic on.

use std::sync::atomic::{self, Ordering};

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// An `AtomicUsize` whose value the proof tracks through a `UsizePerm`.
#[cfg_attr(verus_keep_ghost, verus_verify(external_body))]
pub struct Usize(atomic::AtomicUsize);

/// An `AtomicU64` whose value the proof tracks through a `U64Perm`.
#[cfg_attr(verus_keep_ghost, verus_verify(external_body))]
pub struct U64(atomic::AtomicU64);

/// An `AtomicPtr<T>` whose value the proof tracks through a `PtrPerm`.
#[cfg_attr(verus_keep_ghost, verus_verify(external_body, accept_recursive_types(T)))]
pub struct Ptr<T>(atomic::AtomicPtr<T>);

#[cfg(verus_keep_ghost)]
verus! {

/// The permission to access a [`Usize`], and its value.
#[verifier::external_body]
pub tracked struct UsizePerm {
	_private: (),
}

impl UsizePerm {
	/// The atomic this permission is for.
	pub uninterp spec fn id(&self) -> int;

	/// The atomic's value.
	pub uninterp spec fn value(&self) -> usize;
}

impl Usize {
	/// Which atomic this is, matched against a permission's.
	pub uninterp spec fn id(&self) -> int;
}

/// The permission to access a [`U64`], and its value.
#[verifier::external_body]
pub tracked struct U64Perm {
	_private: (),
}

impl U64Perm {
	/// The atomic this permission is for.
	pub uninterp spec fn id(&self) -> int;

	/// The atomic's value.
	pub uninterp spec fn value(&self) -> u64;
}

impl U64 {
	/// Which atomic this is, matched against a permission's.
	pub uninterp spec fn id(&self) -> int;
}

/// The permission to access a [`Ptr`], and its value.
#[verifier::external_body]
#[verifier::accept_recursive_types(T)]
pub tracked struct PtrPerm<T> {
	_private: core::marker::PhantomData<T>,
}

impl<T> PtrPerm<T> {
	/// The atomic this permission is for.
	pub uninterp spec fn id(&self) -> int;

	/// The atomic's value.
	pub uninterp spec fn value(&self) -> *mut T;
}

impl<T> Ptr<T> {
	/// Which atomic this is, matched against a permission's.
	pub uninterp spec fn id(&self) -> int;
}

} // verus!

#[cfg_attr(verus_keep_ghost, verus_verify)]
impl Usize {
	#[inline]
	#[cfg_attr(verus_keep_ghost, verifier::external_body)]
	#[cfg_attr(verus_keep_ghost, verus_spec(result =>
		with
			-> perm: Tracked<UsizePerm>
		ensures
			perm@.id() == result.id(),
			perm@.value() == value,
	))]
	pub fn new(value: usize) -> Self {
		proof_with!(|= Tracked::assume_new());
		Self(atomic::AtomicUsize::new(value))
	}

	#[inline]
	#[cfg_attr(verus_keep_ghost, verifier::external_body)]
	#[cfg_attr(verus_keep_ghost, verifier::atomic)]
	#[cfg_attr(verus_keep_ghost, verus_spec(value =>
		with
			Tracked(perm): Tracked<&UsizePerm>
		requires
			perm.id() == self.id(),
			order != Ordering::Release,
			order != Ordering::AcqRel,
		ensures
			value == perm.value(),
		opens_invariants none
		no_unwind
	))]
	pub fn load(&self, order: Ordering) -> usize {
		self.0.load(order)
	}

	#[inline]
	#[cfg_attr(verus_keep_ghost, verifier::external_body)]
	#[cfg_attr(verus_keep_ghost, verifier::atomic)]
	#[cfg_attr(verus_keep_ghost, verus_spec(
		with
			Tracked(perm): Tracked<&mut UsizePerm>
		requires
			old(perm).id() == self.id(),
			order == Ordering::Release,
		ensures
			final(perm).id() == self.id(),
			final(perm).value() == value,
		opens_invariants none
		no_unwind
	))]
	pub fn store(&self, value: usize, order: Ordering) {
		self.0.store(value, order)
	}
}

#[cfg_attr(verus_keep_ghost, verus_verify)]
impl U64 {
	#[inline]
	#[cfg_attr(verus_keep_ghost, verifier::external_body)]
	#[cfg_attr(verus_keep_ghost, verus_spec(result =>
		with
			-> perm: Tracked<U64Perm>
		ensures
			perm@.id() == result.id(),
			perm@.value() == value,
	))]
	pub fn new(value: u64) -> Self {
		proof_with!(|= Tracked::assume_new());
		Self(atomic::AtomicU64::new(value))
	}

	#[inline]
	#[cfg_attr(verus_keep_ghost, verifier::external_body)]
	#[cfg_attr(verus_keep_ghost, verifier::atomic)]
	#[cfg_attr(verus_keep_ghost, verus_spec(value =>
		with
			Tracked(perm): Tracked<&U64Perm>
		requires
			perm.id() == self.id(),
			order != Ordering::Release,
			order != Ordering::AcqRel,
		ensures
			value == perm.value(),
		opens_invariants none
		no_unwind
	))]
	pub fn load(&self, order: Ordering) -> u64 {
		self.0.load(order)
	}

	#[inline]
	#[cfg_attr(verus_keep_ghost, verifier::external_body)]
	#[cfg_attr(verus_keep_ghost, verifier::atomic)]
	#[cfg_attr(verus_keep_ghost, verus_spec(
		with
			Tracked(perm): Tracked<&mut U64Perm>
		requires
			old(perm).id() == self.id(),
			order == Ordering::Release,
		ensures
			final(perm).id() == self.id(),
			final(perm).value() == value,
		opens_invariants none
		no_unwind
	))]
	pub fn store(&self, value: u64, order: Ordering) {
		self.0.store(value, order)
	}
}

#[cfg_attr(verus_keep_ghost, verus_verify)]
impl<T> Ptr<T> {
	#[inline]
	#[cfg_attr(verus_keep_ghost, verifier::external_body)]
	#[cfg_attr(verus_keep_ghost, verus_spec(result =>
		with
			-> perm: Tracked<PtrPerm<T>>
		ensures
			perm@.id() == result.id(),
			perm@.value() == value,
	))]
	pub fn new(value: *mut T) -> Self {
		proof_with!(|= Tracked::assume_new());
		Self(atomic::AtomicPtr::new(value))
	}

	#[inline]
	#[cfg_attr(verus_keep_ghost, verifier::external_body)]
	#[cfg_attr(verus_keep_ghost, verifier::atomic)]
	#[cfg_attr(verus_keep_ghost, verus_spec(value =>
		with
			Tracked(perm): Tracked<&PtrPerm<T>>
		requires
			perm.id() == self.id(),
			order != Ordering::Release,
			order != Ordering::AcqRel,
		ensures
			value == perm.value(),
		opens_invariants none
		no_unwind
	))]
	pub fn load(&self, order: Ordering) -> *mut T {
		self.0.load(order)
	}

	#[inline]
	#[cfg_attr(verus_keep_ghost, verifier::external_body)]
	#[cfg_attr(verus_keep_ghost, verifier::atomic)]
	#[cfg_attr(verus_keep_ghost, verus_spec(
		with
			Tracked(perm): Tracked<&mut PtrPerm<T>>
		requires
			old(perm).id() == self.id(),
			order == Ordering::Release,
		ensures
			final(perm).id() == self.id(),
			final(perm).value() == value,
		opens_invariants none
		no_unwind
	))]
	pub fn store(&self, value: *mut T, order: Ordering) {
		self.0.store(value, order)
	}
}

// Outside the proof: code the proof doesn't cover reaches the atomic directly, and is
// trusted to keep the log's invariant as the verified code does.
impl Usize {
	#[inline]
	pub fn unverified(&self) -> &atomic::AtomicUsize {
		&self.0
	}
}
