use std::collections::HashSet;
use std::fmt;
use std::sync::{Mutex, OnceLock};

use crate::credential::{Credential, PhysicalName};
use crate::error::{Error, Result};

/// Exclusive publication of one `(context, generation, kid)`.
///
/// Not `Clone`. Dropping it does not free the generation: a later [`Credential::publish`]
/// with the same pin is [`Error::Reuse`], because a restart can reset transport sequences.
///
/// The claimed set is process-global and never evicted: every embedder in the
/// process shares it, and the context bytes are retained for the life of the process.
/// Tests must mint a unique context per publication.
pub struct Publication {
	credential: Credential,
	tracks: Mutex<HashSet<PhysicalName>>,
}

impl Publication {
	pub(crate) fn new(credential: Credential) -> Result<Self> {
		claim_generation(&credential)?;
		Ok(Self {
			credential,
			tracks: Mutex::new(HashSet::new()),
		})
	}

	/// The credential this publication holds.
	pub fn credential(&self) -> &Credential {
		&self.credential
	}

	/// Wrap a net track whose name is the derived physical name of `semantic`.
	///
	/// # Errors
	///
	/// [`Error::Identity`] if the track name is not the derived physical name,
	/// [`Error::Reuse`] if this physical name was already claimed.
	pub fn track(&self, track: moq_net::track::Producer, semantic: impl AsRef<[u8]>) -> Result<crate::track::Producer> {
		let physical = self.credential.physical_name(semantic)?;
		if track.name() != physical.as_str() {
			return Err(Error::Identity);
		}
		self.claim_track(physical.clone())?;
		crate::track::Producer::new(track, &self.credential, physical)
	}

	fn claim_track(&self, physical: PhysicalName) -> Result<()> {
		let mut tracks = self.tracks.lock().expect("publication tracks");
		if !tracks.insert(physical) {
			return Err(Error::Reuse);
		}
		Ok(())
	}
}

impl fmt::Debug for Publication {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Publication")
			.field("generation", &self.credential.generation())
			.field("kid", &self.credential.kid())
			.finish()
	}
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct GenerationKey {
	context: Box<[u8]>,
	generation: u64,
	kid: u64,
}

fn generations() -> &'static Mutex<HashSet<GenerationKey>> {
	static PUBLISHED: OnceLock<Mutex<HashSet<GenerationKey>>> = OnceLock::new();
	PUBLISHED.get_or_init(|| Mutex::new(HashSet::new()))
}

fn claim_generation(credential: &Credential) -> Result<()> {
	let key = GenerationKey {
		context: credential.context().into(),
		generation: credential.generation(),
		kid: credential.kid(),
	};
	let mut published = generations().lock().expect("publication generations");
	if !published.insert(key) {
		return Err(Error::Reuse);
	}
	Ok(())
}
