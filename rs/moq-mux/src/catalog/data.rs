use super::hang::CatalogExt;
use super::{Estimator, Rendition, RenditionConfig};

/// The config a data producer ([`Producer::json_stream`](super::Producer::json_stream) and the
/// like) publishes as its catalog entry.
///
/// Implemented for the [`json::Config`](crate::json::Config) and
/// [`binary::Config`](crate::binary::Config) builders, which list the track in the `json` or
/// `binary` section. Also implemented for any [`RenditionConfig`] that embeds the data config `D`
/// ([`JsonConfig`](hang::catalog::JsonConfig) or [`BinaryConfig`](hang::catalog::BinaryConfig))
/// through [`AsMut`], which is how an application lists a data track in its own section beside its
/// own per-track fields; see the [`RenditionConfig`] example.
///
/// The producer sets the embedded config's `mode`, encodes the track with its `compression`, and
/// owns the entry: it is written when the producer is created and removed when it drops.
pub trait IntoRendition<E: CatalogExt, D> {
	/// The catalog entry, embedding `D`.
	type Config: RenditionConfig<E> + AsMut<D>;

	/// Build the catalog entry.
	fn into_rendition(self) -> Self::Config;
}

impl<E: CatalogExt, D, C: RenditionConfig<E> + AsMut<D>> IntoRendition<E, D> for C {
	type Config = C;

	fn into_rendition(self) -> C {
		self
	}
}

/// A data track's catalog entry, owned for the life of its producer and kept current with the
/// bitrate its writes measure.
///
/// Erases the entry's type, so a data producer's own type doesn't depend on which section lists it.
pub(crate) struct Listing {
	rendition: Box<dyn Owned>,
	estimator: Estimator,
}

/// The parts of a [`Rendition`] a [`Listing`] uses, without its config type.
trait Owned: Send + Sync {
	fn name(&self) -> &str;
	fn detects(&self) -> bool;
	fn timestamp(&self) -> crate::Result<moq_net::Timestamp>;
	fn estimate(&mut self, estimate: super::Estimate) -> crate::Result<()>;
}

impl<E: CatalogExt, C: RenditionConfig<E>> Owned for Rendition<E, C> {
	fn name(&self) -> &str {
		Rendition::name(self)
	}
	fn detects(&self) -> bool {
		C::detects()
	}
	fn timestamp(&self) -> crate::Result<moq_net::Timestamp> {
		Rendition::timestamp(self, None)
	}
	fn estimate(&mut self, estimate: super::Estimate) -> crate::Result<()> {
		Rendition::estimate(self, estimate)
	}
}

impl Listing {
	/// Publish `config` as the entry `rendition` reserved.
	pub(crate) fn new<E: CatalogExt, C: RenditionConfig<E>>(
		mut rendition: Rendition<E, C>,
		config: C,
	) -> crate::Result<Self> {
		rendition.set(config)?;
		Ok(Self {
			rendition: Box::new(rendition),
			estimator: Estimator::new(),
		})
	}

	/// The track name, which is also the catalog key.
	pub(crate) fn name(&self) -> &str {
		self.rendition.name()
	}

	/// Measure a write of `bytes`, stamped on the broadcast clock.
	///
	/// `bytes` is only evaluated for an entry that detects its estimate, since measuring can cost a
	/// second serialization.
	pub(crate) fn record(&mut self, bytes: impl FnOnce() -> usize) -> crate::Result<()> {
		if !self.rendition.detects() {
			return Ok(());
		}
		let now = self.rendition.timestamp()?;
		self.record_at(now, bytes())
	}

	/// [`record`](Self::record) at a chosen time, which a test needs since the broadcast clock only
	/// moves in real time.
	pub(crate) fn record_at(&mut self, now: moq_net::Timestamp, bytes: usize) -> crate::Result<()> {
		// Each write is its own span, closed by the next one.
		self.estimator.cut(Some(now));
		self.estimator.write(now, bytes);

		// The spacing between writes is the application's cadence, not a flush delay, so only the
		// bitrate is measured. A publisher that knows its jitter sets it on the entry.
		let estimate = self.estimator.estimate().with_jitter(None);
		self.rendition.estimate(estimate)
	}
}

/// The serialized size of `value`, as an upper bound on what a JSON write puts on the wire:
/// compression and deltas only shrink it.
pub(crate) fn json_len<T: serde::Serialize>(value: &T) -> usize {
	struct Count(usize);

	impl std::io::Write for Count {
		fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
			self.0 += buf.len();
			Ok(buf.len())
		}
		fn flush(&mut self) -> std::io::Result<()> {
			Ok(())
		}
	}

	let mut count = Count(0);
	// Only reached after the producer serialized the same value, so this cannot fail.
	let _ = serde_json::to_writer(&mut count, value);
	count.0
}
