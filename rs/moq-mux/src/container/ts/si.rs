//! Standalone SI carriage over snapshot tracks.
//!
//! Import captures each `(PID, table_id)` pair onto its own MoQ track, mapped from
//! the catalog's `mpegts.si` section ([`catalog::SiEntry`]). The track format is
//! snapshot-shaped: every group is a complete picture of the table's current
//! sections, one frame per sub-table, each frame the sub-table's sections
//! concatenated verbatim in `section_number` order. Frames apply in order and a
//! later frame replaces an earlier one with the same sub-table identity, so a
//! writer may append incremental updates within a group; this writer never does
//! (every group is a full set), but readers honor the rule so that stays a writer
//! choice rather than a format change. A joiner reads only the newest group.
//!
//! [`Capture`] is the import half: it buffers each sub-table's sections until a
//! complete generation is assembled, commits the generation atomically, and cuts a
//! new snapshot group, so a torn multi-section transition (half old version, half
//! new) is unrepresentable on the track. [`Snapshot`] is the export half: it
//! reduces a track's frames back into the current section set.
//!
//! An import that selects a program ([`Capture::select`]) carries SI for that
//! service alone. Other services' EIT actual sub-tables are dropped on arrival, and
//! each SDT actual snapshot is rebuilt as one section holding only the selected
//! service's entry, with a fresh CRC. An SDT actual section whose own CRC fails is
//! dropped, and a revision commits only once every section has arrived, so the last
//! good snapshot stays in force until then. An SDT that does not list the
//! service carries no SDT actual, so a revision that drops it retires the earlier
//! one. Network-wide tables (NIT, BAT, SDT other, EIT other, TDT/TOT) pass through
//! verbatim, as does everything when no program is selected.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use bytes::Bytes;
use moq_net::Timestamp;

use super::catalog;

const SDT_PID: u16 = 0x0011;
const SDT_ACTUAL: u8 = 0x42;
const EIT_PID: u16 = 0x0012;

/// The checksum closing every long-form DVB section.
const CRC: crc::Crc<u32> = crc::Crc::<u32>::new(&crc::CRC_32_MPEG_2);

/// Coalesce snapshot cuts: a junction revises many sub-tables inside a second
/// (every service's now/next rolls on the hour), and one group carrying all of them
/// beats a burst of near-identical groups. Measured on the host clock, not the
/// media clock: bursts arrive in real time, and an audio-only stream never
/// advances the media clock at all (which would wedge every revision behind the
/// debounce forever). The first snapshot is never delayed.
const DEBOUNCE: Duration = Duration::from_secs(1);

/// A sub-table's identity within its `(PID, table_id)` entry: what a revised
/// generation replaces.
///
/// Generic section syntax carries `table_id_extension`; for two documented table
/// families that alone is ambiguous, so the spec-fixed scope bytes join the key
/// (see [`scope`]). A section with no long-form header has no extension, version,
/// or numbering, so its table is a single latest-value slot: each arrival replaces
/// the last. That is exactly right for the short-form tables DVB defines (TDT/TOT
/// are clocks; ST is stuffing receivers discard), and it is what lets a time table
/// be proxied at all (#2914): content-keyed identity would mint a fresh key per
/// tick. A hypothetical multi-section *static* short-form table would collapse to
/// its most recent section and alternate; the debounce bounds that at one small
/// group per second, which is the acceptable cost of not parsing tables.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) enum Identity {
	/// A long-form sub-table: `table_id_extension` plus any table-specific scope.
	Sub { ext: u16, scope: u32 },
	/// A short-form or runt section: one slot for the whole table, latest wins.
	Table,
}

/// Compute a section's sub-table identity.
pub(super) fn identity(section: &[u8]) -> Identity {
	if !long_form(section) {
		return Identity::Table;
	}
	let ext = u16::from_be_bytes([section[3], section[4]]);
	Identity::Sub {
		ext,
		scope: scope(section),
	}
}

/// The table-specific bytes that extend the generic identity, for the two table
/// families whose `table_id_extension` alone is ambiguous (#2842).
///
/// These are fixed offsets of published ETSI EN 300 468 layouts, gated on
/// `table_id`; reading them narrows *identity*, not what export claims to
/// understand (the sections stay opaque bytes).
fn scope(section: &[u8]) -> u32 {
	match section[0] {
		// SDT other: the extension is a transport_stream_id, unique only within a
		// network; original_network_id at bytes 8..10 disambiguates.
		0x46 if section.len() >= 10 => u32::from(u16::from_be_bytes([section[8], section[9]])),
		// EIT: the extension is a service_id, unique only within a transport stream;
		// transport_stream_id at 8..10 and original_network_id at 10..12 disambiguate.
		0x4E..=0x6F if section.len() >= 12 => u32::from_be_bytes([section[8], section[9], section[10], section[11]]),
		_ => 0,
	}
}

/// Whether the section has a complete long-form header (syntax indicator set and
/// the 8 header bytes present).
fn long_form(section: &[u8]) -> bool {
	section.len() >= 8 && section[1] & 0x80 != 0
}

/// Split a frame payload back into its individual sections (they are
/// self-delimiting via `section_length`). Stops at the first malformed or
/// truncated header.
pub(super) fn split_sections(payload: &Bytes) -> Vec<Bytes> {
	let mut out = Vec::new();
	let mut off = 0;
	while payload.len() >= off + 3 {
		let len = 3 + ((((payload[off + 1] & 0x0f) as usize) << 8) | payload[off + 2] as usize);
		if payload.len() < off + len {
			break;
		}
		out.push(payload.slice(off..off + len));
		off += len;
	}
	out
}

/// Rebuild an SDT sub-table as one section listing only `service`, wherever its
/// entry sat among `sections`. The header (transport_stream_id, version_number,
/// original_network_id) is kept, the numbering reset to section 0 of 0, and the
/// length and CRC recomputed. `None` when no section lists the service.
fn sdt_service<'a>(sections: impl IntoIterator<Item = &'a Bytes>, service: u16) -> Option<Vec<u8>> {
	for section in sections {
		// The service loop runs from the 11-byte header to the CRC.
		let end = section.len().saturating_sub(4);
		let mut off = 11;
		while off + 5 <= end {
			let len = 5 + ((usize::from(section[off + 3] & 0x0f) << 8) | usize::from(section[off + 4]));
			if off + len > end {
				break;
			}
			if u16::from_be_bytes([section[off], section[off + 1]]) == service {
				let mut out = section[..11].to_vec();
				out.extend_from_slice(&section[off..off + len]);
				let section_length = out.len() - 3 + 4;
				out[1] = (out[1] & 0xf0) | (section_length >> 8) as u8;
				out[2] = section_length as u8;
				out[6] = 0;
				out[7] = 0;
				out.extend_from_slice(&CRC.checksum(&out).to_be_bytes());
				return Some(out);
			}
			off += len;
		}
	}
	None
}

/// One committed generation of a sub-table: the sections currently in force.
///
/// Keyed by `section_number`, sparse on purpose: DVB EIT schedule segments skip
/// unused section numbers, so 0..=last is not a contiguous range there.
#[derive(Default, PartialEq)]
struct Generation {
	version: Option<u8>,
	sections: BTreeMap<u8, Bytes>,
}

/// A generation still being assembled, section by section.
#[derive(Default)]
struct Pending {
	/// Highest section number this generation can hold, from `last_section_number`
	/// (also raised by any observed `section_number`, so a malformed header cannot
	/// hide a section).
	last: u8,
	sections: BTreeMap<u8, Bytes>,
}

/// How a generation was judged complete, deciding whether the commit replaces or
/// merges (see [`Entry::commit`]).
enum Complete {
	/// Every section of the declaration is present.
	Contiguous,
	/// The transmission cycle wrapped: complete as observed, bounded by the
	/// highest `last_section_number` seen.
	Wrapped { last: u8 },
}

impl Pending {
	/// All of 0..=last present: the fast path for densely-numbered tables (SDT, NIT,
	/// EIT now/next), committing the moment the last section lands. EIT schedule
	/// never satisfies this: its numbering is deliberately sparse (a real 8-day
	/// guide declares `last_section_number` 248 and transmits 32 sections), so the
	/// cycle-wrap path in [`Entry::section`] is its *only* commit path, not a
	/// fallback.
	fn contiguous(&self) -> bool {
		self.sections.len() == self.last as usize + 1
	}
}

/// One `(PID, table_id)` entry: its snapshot track and the state of every
/// sub-table riding it.
struct Entry {
	/// Created with the first snapshot that carries anything, so a table filtered
	/// to nothing never gets a track.
	track: Option<moq_net::track::Producer>,
	interval: Option<Duration>,
	/// The selected service an SDT actual snapshot is reduced to.
	service: Option<u16>,
	active: BTreeMap<Identity, Generation>,
	/// In-flight generations, keyed by sub-table and version.
	pending: HashMap<(Identity, u8), Pending>,
	/// A commit changed `active` since the last cut.
	dirty: bool,
	/// The newest snapshot group carries at least one sub-table.
	populated: bool,
	/// The entry appears in the catalog's `mpegts.si` map: exactly while it is
	/// `populated`, so a subscriber never finds an empty track.
	advertised: bool,
	/// Host-clock micros of the last cut ([`DEBOUNCE`]); `None` until the first.
	last_cut: Option<u128>,
}

impl Entry {
	/// Fold one completed, current section into the store, committing its
	/// generation when it completes. Returns nothing; a commit marks the entry
	/// dirty and [`Capture::flush`] cuts the group.
	fn section(&mut self, section: Bytes) {
		let id = identity(&section);

		let Identity::Sub { .. } = id else {
			// Latest-value slot: a short-form table has no version to buffer under, so
			// each arrival replaces the last (a TDT ticks ~every 30s and every section
			// is new bytes). A byte-identical repetition is not a change.
			let changed = self
				.active
				.get(&id)
				.is_none_or(|current| current.sections.get(&0) != Some(&section));
			if changed {
				self.active.insert(
					id,
					Generation {
						version: None,
						sections: BTreeMap::from([(0u8, section)]),
					},
				);
				self.dirty = true;
			}
			return;
		};

		let version = (section[5] >> 1) & 0x1f;
		let number = section[6];
		let last = section[7];

		// A byte-identical repetition of the current state is the overwhelmingly
		// common case (SI repeats every couple of seconds); recognize it before
		// touching any pending state.
		if let Some(current) = self.active.get(&id)
			&& current.version == Some(version)
			&& current.sections.get(&number) == Some(&section)
		{
			return;
		}

		let pending = self.pending.entry((id, version)).or_default();
		pending.last = pending.last.max(last).max(number);

		if let Some(prior) = pending.sections.get(&number) {
			if *prior == section {
				// A reduced SDT reads a missing service as removal, so a dense SDT
				// missing a section (dropped for a bad CRC, or lost) must not commit as
				// complete-as-observed; it waits for the cycle that completes it.
				if self.service.is_some() {
					return;
				}
				// The same section came round again before the set completed: the
				// transmission cycle wrapped, so what we hold is the whole sub-table as
				// this mux transmits it. For EIT schedule this is the *only* commit
				// path (see [`Pending::contiguous`]). The limit: a section lost before
				// the wrap is indistinguishable from a legitimately skipped number, so
				// the committed set is complete-as-observed; the same-version merge in
				// [`Self::commit`] fills it in on the next cycle. No section-counting
				// receiver can do better; version mixing is still unrepresentable.
				let last = pending.last;
				let sections = std::mem::take(&mut pending.sections);
				self.commit(id, version, sections, Complete::Wrapped { last });
				return;
			}
			// Same version, new bytes: non-conformant (a revision must bump the
			// version), but taking the newer bytes degrades more gracefully than
			// pinning the older ones.
			pending.sections.insert(number, section);
			return;
		}

		pending.sections.insert(number, section);
		if pending.contiguous() {
			let sections = std::mem::take(&mut pending.sections);
			self.commit(id, version, sections, Complete::Contiguous);
		}
	}

	/// Commit the sub-table's generation atomically and retire everything pending
	/// for it.
	///
	/// A contiguous commit replaces the old generation wholesale: it holds every
	/// section its declaration promises, so anything else active is stale, even
	/// under the same version number (versions are five bits, so a reception gap
	/// can bring the same value back with different content). A wrap commit is
	/// complete-as-observed, not complete, so it *merges* into a same-version
	/// active generation instead: a section lost before the cycle wrapped
	/// short-circuits as a repetition on the next cycle and the wrap only ever
	/// carries the stragglers, so replacing would flip-flop between incomplete
	/// subsets forever while merging converges. The merge is bounded by the new
	/// declaration's `last_section_number`, so a shrunken table cannot resurrect
	/// sections past its own end. Repetition of an unchanged set is not a change.
	fn commit(&mut self, id: Identity, version: u8, mut sections: BTreeMap<u8, Bytes>, complete: Complete) {
		self.pending.retain(|(pid, _), _| *pid != id);
		if let Complete::Wrapped { last } = complete
			&& let Some(current) = self.active.get(&id)
			&& current.version == Some(version)
		{
			for (number, section) in current.sections.iter() {
				if *number <= last {
					sections.entry(*number).or_insert_with(|| section.clone());
				}
			}
		}
		let changed = self.active.get(&id).is_none_or(|current| current.sections != sections);
		self.active.insert(
			id,
			Generation {
				version: Some(version),
				sections,
			},
		);
		if changed {
			self.dirty = true;
		}
	}

	/// Cut a snapshot group: the complete active set, one frame per sub-table, an
	/// SDT reduced to the selected service. Nothing is cut until a snapshot carries
	/// something, and that snapshot creates the entry's track on `broadcast`.
	/// `pts` stamps the frames; `now` is the host clock for the debounce.
	fn cut(
		&mut self,
		broadcast: &moq_net::broadcast::Producer,
		(pid, table_id): (u16, u8),
		pts: Timestamp,
		now: u128,
	) -> anyhow::Result<()> {
		let frames: Vec<Vec<u8>> = self
			.active
			.values()
			.filter_map(|generation| match self.service {
				Some(service) => sdt_service(generation.sections.values(), service),
				None => Some(generation.sections.values().flatten().copied().collect()),
			})
			.collect();
		self.dirty = false;
		self.populated = !frames.is_empty();
		let track = match &mut self.track {
			Some(track) => track,
			None if frames.is_empty() => return Ok(()),
			// Deterministic, greppable name; fall back to a unique suffix on the
			// (pathological) collision with an existing track.
			None => self.track.insert(
				match broadcast.create_track(format!("{pid:#06x}-{table_id:#04x}.si"), None) {
					Ok(track) => track,
					Err(_) => broadcast.unique_track(".si", None)?,
				},
			),
		};
		let mut group = track.append_group()?;
		for payload in frames {
			group.write_frame(pts, payload)?;
		}
		group.finish()?;
		self.last_cut = Some(now);
		Ok(())
	}
}

/// Import-side SI capture: reassembled sections in, snapshot tracks and catalog
/// entries out.
///
/// Owned by the TS importer, which feeds it every completed section on a captured
/// SI PID and calls [`flush`](Self::flush) once per decode batch. Dropping it
/// removes the advertised entries from the catalog, mirroring the verbatim tracks.
pub(super) struct Capture<E: catalog::Catalog> {
	broadcast: moq_net::broadcast::Producer,
	catalog: crate::catalog::Producer<E>,
	entries: BTreeMap<(u16, u8), Entry>,
	/// Host clock driving the cut debounce (see [`DEBOUNCE`]).
	clock: crate::Clock,
	/// The selected program_number, which is the service_id of the only service
	/// carried; `None` captures every service.
	program: Option<u16>,
}

impl<E: catalog::Catalog> Capture<E> {
	pub fn new(broadcast: moq_net::broadcast::Producer, catalog: crate::catalog::Producer<E>) -> Self {
		Self {
			broadcast,
			catalog,
			entries: BTreeMap::new(),
			clock: crate::Clock::new(),
			program: None,
		}
	}

	/// Carry only the service numbered `program`: its EIT actual, and an SDT actual
	/// listing it alone. SI keys services by `service_id`, assumed equal to the PAT
	/// `program_number`.
	pub fn select(&mut self, program: u16) {
		self.program = Some(program);
	}

	/// Fold one completed section from `pid` into its `(pid, table_id)` entry,
	/// creating the entry on first sight of the pair.
	pub fn section(&mut self, pid: u16, section: Vec<u8>) -> anyhow::Result<()> {
		let Some(&table_id) = section.first() else {
			return Ok(());
		};
		// A next-version section (current_next_indicator clear) describes a future
		// state, not the current one; carrying it would need a second generation slot
		// per sub-table for a table nothing re-emits, so it is dropped until the
		// version becomes current.
		let section = Bytes::from(section);
		if long_form(&section) && section[5] & 0x01 == 0 {
			return Ok(());
		}
		// EIT actual (now/next and schedule) is keyed by service_id: another
		// service's sub-table describes a program this import does not carry.
		if let Some(program) = self.program
			&& pid == EIT_PID
			&& matches!(table_id, 0x4E | 0x50..=0x5F)
			&& !matches!(identity(&section), Identity::Sub { ext, .. } if ext == program)
		{
			return Ok(());
		}

		let service = self.program.filter(|_| (pid, table_id) == (SDT_PID, SDT_ACTUAL));
		// The rebuilt SDT gets a fresh CRC, which would certify corruption the
		// source's CRC flags; dropping the section keeps the last good generation.
		if service.is_some() && CRC.checksum(&section) != 0 {
			return Ok(());
		}
		let entry = self.entries.entry((pid, table_id)).or_insert_with(|| Entry {
			track: None,
			interval: catalog::si_interval(table_id),
			service,
			active: BTreeMap::new(),
			pending: HashMap::new(),
			dirty: false,
			populated: false,
			advertised: false,
			last_cut: None,
		});
		entry.section(section);
		Ok(())
	}

	/// Cut snapshot groups for the dirty entries, then advertise each entry whose
	/// snapshot now carries something and retract each whose snapshot went empty
	/// (one catalog lock for the whole batch).
	///
	/// `pts` is the media clock, used to stamp the frames; the cut debounce runs on
	/// the host clock ([`DEBOUNCE`]), and a first snapshot is never delayed. `force`
	/// overrides the debounce, for end of stream.
	pub fn flush(&mut self, pts: Timestamp, force: bool) -> anyhow::Result<()> {
		let now = self.clock.now().as_micros();
		for (key, entry) in self.entries.iter_mut() {
			if !entry.dirty {
				continue;
			}
			if !force
				&& let Some(last) = entry.last_cut
				&& now.saturating_sub(last) < DEBOUNCE.as_micros()
			{
				continue;
			}
			entry.cut(&self.broadcast, *key, pts, now)?;
		}

		if self.entries.values().any(|e| e.advertised != e.populated) {
			let mut guard = self.catalog.modify()?;
			let Some(mpegts) = guard.ext.mpegts_mut() else {
				anyhow::bail!("catalog extension no longer carries an mpegts section");
			};
			for ((pid, table_id), entry) in self.entries.iter_mut() {
				// Either flag implies the track exists.
				let Some(track) = &entry.track else { continue };
				if entry.advertised == entry.populated {
					continue;
				}
				if entry.populated {
					mpegts.si.entry(*pid).or_default().insert(
						*table_id,
						catalog::SiEntry {
							track: track.name().to_string(),
							interval: entry.interval,
							..Default::default()
						},
					);
				} else {
					retract(&mut mpegts.si, *pid, *table_id, track.name());
				}
				entry.advertised = entry.populated;
			}
		}
		Ok(())
	}

	/// Flush everything and finish every track.
	pub fn finish(&mut self, pts: Timestamp) -> anyhow::Result<()> {
		self.flush(pts, true)?;
		for track in self.entries.values_mut().filter_map(|entry| entry.track.as_mut()) {
			track.finish()?;
		}
		Ok(())
	}

	/// Abort every track with `err` instead of finishing, removing their catalog
	/// entries so the map never names an aborted track.
	pub fn abort(mut self, err: moq_net::Error) {
		self.unadvertise();
		for (_, entry) in std::mem::take(&mut self.entries) {
			if let Some(track) = entry.track {
				let _ = track.abort(err.clone());
			}
		}
	}

	/// Remove every advertised entry from the catalog's `si` map, once (idempotent:
	/// entries are marked unadvertised as they go, so the `Drop` after an `abort`
	/// finds nothing left to do).
	fn unadvertise(&mut self) {
		if !self.entries.values().any(|e| e.advertised) {
			return;
		}
		// A closed catalog has nothing left to unadvertise from.
		let Ok(mut guard) = self.catalog.modify() else {
			return;
		};
		let Some(mpegts) = guard.ext.mpegts_mut() else {
			return;
		};
		for ((pid, table_id), entry) in self.entries.iter_mut() {
			let Some(track) = entry.track.as_ref().filter(|_| entry.advertised) else {
				continue;
			};
			entry.advertised = false;
			retract(&mut mpegts.si, *pid, *table_id, track.name());
		}
	}
}

/// Remove the `(pid, table_id)` mapping from the catalog's `si` map, and the PID
/// once nothing is left on it.
///
/// Only a mapping still naming `track` goes: a second capture on the same broadcast
/// (same key, unique fallback track name) may have overwritten it, and it will not
/// re-advertise, so blindly removing here would strip the live capture's table for
/// good.
fn retract(si: &mut BTreeMap<u16, BTreeMap<u8, catalog::SiEntry>>, pid: u16, table_id: u8, track: &str) {
	let Some(tables) = si.get_mut(&pid) else {
		return;
	};
	if tables.get(&table_id).is_some_and(|e| e.track == track) {
		tables.remove(&table_id);
	}
	if tables.is_empty() {
		si.remove(&pid);
	}
}

impl<E: catalog::Catalog> Drop for Capture<E> {
	fn drop(&mut self) {
		self.unadvertise();
	}
}

/// Export-side reduction of a snapshot track: frames apply in order, later-wins
/// by sub-table identity. Reset per group (a group is a complete snapshot).
/// Equality is by content, so a re-published identical snapshot is not a change.
#[derive(Default, PartialEq)]
pub(super) struct Snapshot {
	subs: BTreeMap<Identity, Vec<Bytes>>,
}

impl Snapshot {
	/// Apply one frame: the payload's sections replace the sub-table they identify.
	pub fn apply(&mut self, payload: &Bytes) {
		let sections = split_sections(payload);
		let Some(first) = sections.first() else {
			return;
		};
		self.subs.insert(identity(first), sections);
	}

	/// Every section in the snapshot, in stable sub-table order.
	pub fn sections(&self) -> impl Iterator<Item = &Bytes> {
		self.subs.values().flatten()
	}

	pub fn is_empty(&self) -> bool {
		self.subs.is_empty()
	}
}
