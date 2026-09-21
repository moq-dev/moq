//! Where a frame's pixels live once an operation hands it back.

/// The representation a decoder or scaler hands frames back in.
///
/// A choice, not a promise of GPU residency. [`Native`](Self::Native) lets a
/// hardware backend keep its picture where it made it, which on a software
/// backend is CPU memory anyway. [`Cpu`](Self::Cpu) is the portable exit: the
/// result is always [`Surface::I420`](crate::Surface::I420), downloaded when it
/// had to be. `#[non_exhaustive]` so a narrower choice (a specific device, a
/// shareable handle) can be added without breaking a `match`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Output {
	/// The backend's natural representation: a GPU surface from a hardware
	/// decoder or scaler, CPU pixels from a software one. The default, since it
	/// never pays for a download nobody asked for. Match on each
	/// [`Surface`](crate::Surface) rather than assuming either.
	#[default]
	Native,
	/// CPU-resident [`I420`](crate::I420), whatever produced it. A hardware
	/// decode pays a download per picture; a backend that can decode straight
	/// to system memory does that instead.
	Cpu,
}
