//! Common Encryption (ISO/IEC 23001-7) passthrough: what an encrypted track's init declares, and
//! the checks that keep a republished fragment decryptable.

use hang::catalog::drm;
use mp4_atom::{Atom, Encode, FourCC};

use super::Error;
use crate::Result;

/// The `seig` sample group: per-sample key IDs, which is key rotation.
const SEIG: FourCC = FourCC::new(b"seig");

/// What an encrypted track's sample entry and its moov declare.
#[derive(Debug, Clone)]
pub(crate) struct Protection {
	/// The scheme `schm` names.
	pub scheme: drm::Scheme,
	/// `tenc`'s default key ID.
	pub kid: [u8; 16],
	/// The moov's `pssh` boxes that apply to this track, in their order there.
	pub pssh: Vec<mp4_atom::Pssh>,
	/// How each fragment's encryption information is laid out.
	aux: Aux,
}

/// How an encrypted track's `senc` lays out each sample's encryption information.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Aux {
	/// The size of each sample's IV, 0 under a constant IV.
	iv_size: u8,
	/// Every fragment must carry a `senc`: the samples have per-sample IVs, or they are video,
	/// which Common Encryption only protects with subsample maps.
	senc: bool,
}

impl Protection {
	/// The protection of `trak`'s sample entry, or `None` when it is clear.
	///
	/// Refuses a clear lead (an encrypted entry beside another), a scheme other than `cenc` or
	/// `cbcs`, key rotation declared in the sample table, and an encrypted track that no `pssh`
	/// applies to, since the catalog could name no DRM system for it.
	pub fn of(trak: &mp4_atom::Trak, moov: &mp4_atom::Moov) -> Result<Option<Self>> {
		let stbl = &trak.mdia.minf.stbl;
		let codecs = &stbl.stsd.codecs;
		let Some(sinf) = codecs.iter().find_map(sinf) else {
			return Ok(None);
		};
		// A clear lead: an encrypted entry beside a clear one, refused as several codecs are.
		if codecs.len() > 1 {
			return Err(Error::MultipleCodecs.into());
		}

		let schm = sinf
			.schm
			.as_ref()
			.ok_or(Error::InvalidProtection("protected sample entry without schm"))?;
		let scheme = match &<[u8; 4]>::from(schm.scheme_type) {
			b"cenc" => drm::Scheme::Cenc,
			b"cbcs" => drm::Scheme::Cbcs,
			_ => return Err(Error::UnsupportedScheme(schm.scheme_type).into()),
		};
		// mp4-atom refuses a cenc or cbcs sinf without a tenc.
		let tenc = sinf
			.schi
			.as_ref()
			.and_then(|schi| schi.tenc.as_ref())
			.ok_or(Error::InvalidProtection("protected sample entry without tenc"))?;

		if rotates(&stbl.sgpd, &stbl.sbgp) {
			return Err(Error::KeyRotation.into());
		}

		let kid = tenc.default_kid;
		// A version 0 pssh applies to every key; a version 1 one names the keys it covers.
		let pssh: Vec<_> = (moov.pssh.iter())
			.filter(|pssh| {
				pssh.kid
					.as_ref()
					.is_none_or(|kids| kids.is_empty() || kids.contains(&kid))
			})
			.cloned()
			.collect();
		if pssh.is_empty() {
			return Err(Error::MissingPssh.into());
		}

		let iv_size = tenc.default_per_sample_iv_size;
		let video = matches!(codecs[..], [mp4_atom::Codec::Encv(_)]);
		let aux = Aux {
			iv_size,
			// A clear-by-default track (no seig, refused above) has no encryption information.
			senc: tenc.default_is_protected && (iv_size > 0 || video),
		};
		Ok(Some(Self { scheme, kid, pssh, aux }))
	}

	/// How the track's fragments lay out their encryption information.
	pub fn aux(&self) -> Aux {
		self.aux
	}

	/// One catalog entry per DRM system, with that system's first `pssh` box.
	pub fn entries(&self) -> Result<Vec<drm::Entry>> {
		let mut entries: Vec<drm::Entry> = Vec::new();
		for pssh in &self.pssh {
			let system = drm::Uuid::from_bytes(pssh.system_id);
			if entries.iter().any(|entry| entry.system == system) {
				continue;
			}
			let mut entry = drm::Entry::new(system, self.scheme, drm::Uuid::from_bytes(self.kid));
			let mut buf = Vec::new();
			pssh.encode(&mut buf).map_err(Error::from)?;
			entry.pssh = Some(buf.into());
			entries.push(entry);
		}
		Ok(entries)
	}
}

impl Aux {
	/// Refuse a fragment that lacks the `senc` its samples need to be decrypted.
	///
	/// mp4-atom reads only the ISO `senc`, so this also refuses one that carries its information
	/// only in PIFF's `uuid` box, which the rebuilt fragment would drop.
	///
	/// A fragment kept whole passes through as the source wrote it, so its `senc` and `saiz` sample
	/// counts are not held to its `trun`: ffmpeg's `cenc` audio (as of Lavf 63) carries every earlier
	/// fragment's entries too, and refusing that would refuse ffmpeg's own output. Only a fragment
	/// the importer drops samples from is held to it, by [`restate`](Self::restate).
	pub fn check(&self, traf: &mp4_atom::Traf) -> Result<()> {
		if self.senc && traf.senc.is_none() {
			return Err(Error::InvalidProtection("encrypted fragment without senc").into());
		}
		Ok(())
	}

	/// Restate `source`'s encryption information in `kept`, the same fragment holding only the
	/// samples at `indices`, ascending positions among `source`'s samples.
	///
	/// The importer drops samples an edit list skips, such as AAC's priming frame, and those before
	/// the first sync sample of a stream joined mid-GOP; the samples that remain still need their
	/// IVs and subsample maps.
	pub fn restate(&self, source: &mp4_atom::Traf, kept: &mut mp4_atom::Traf, indices: &[usize]) -> Result<()> {
		let Some(senc) = &source.senc else {
			// Nothing per sample to keep: a constant IV over whole samples. Information elsewhere
			// is refused, as it is for any fragment.
			if source.saio.iter().any(|saio| is_cenc(saio.aux_info.as_ref())) {
				return Err(Error::InvalidProtection("encryption auxiliary information outside senc").into());
			}
			return Ok(());
		};
		if senc.version != mp4_atom::SencBoxVersion::V0 {
			return Err(Error::InvalidProtection("unsupported senc version").into());
		}

		let entries = self.entries(senc)?;
		if entries.len() as u64 != super::sample::count(source) {
			return Err(Error::InvalidProtection("senc and trun disagree on the sample count").into());
		}
		let mut data = Vec::with_capacity(senc.data.len());
		data.extend_from_slice(&(indices.len() as u32).to_be_bytes());
		for &index in indices {
			let entry = entries
				.get(index)
				.ok_or(Error::InvalidProtection("a kept sample past senc"))?;
			data.extend_from_slice(entry);
		}
		kept.senc = Some(mp4_atom::Senc {
			version: senc.version.clone(),
			use_subsamples: senc.use_subsamples,
			data,
		});

		let mut saiz = source.saiz.iter().filter(|saiz| is_cenc(saiz.aux_info.as_ref()));
		if let Some(saiz) = saiz.next() {
			if saiz.sample_count as u64 != super::sample::count(source) {
				return Err(Error::InvalidProtection("saiz and trun disagree on the sample count").into());
			}
			if saiz.default_sample_info_size == 0 {
				let sizes = (indices.iter())
					.map(|&index| saiz.sample_info_size.get(index).copied())
					.collect::<Option<Vec<_>>>()
					.ok_or(Error::InvalidProtection("a kept sample past saiz"))?;
				kept.saiz.push(mp4_atom::Saiz {
					aux_info: saiz.aux_info.clone(),
					default_sample_info_size: 0,
					sample_count: indices.len() as u32,
					sample_info_size: sizes,
				});
			} else {
				kept.saiz.push(mp4_atom::Saiz {
					aux_info: saiz.aux_info.clone(),
					default_sample_info_size: saiz.default_sample_info_size,
					sample_count: indices.len() as u32,
					sample_info_size: Vec::new(),
				});
			}
		}
		// Several would be refused by `reserve_saio` on the source's own fragments too.
		if saiz.next().is_some() {
			return Err(Error::InvalidProtection("encryption auxiliary information split over several saiz").into());
		}
		kept.saio = (source.saio.iter())
			.filter(|saio| is_cenc(saio.aux_info.as_ref()))
			.cloned()
			.collect();
		Ok(())
	}

	/// Each sample's encryption information in `senc`: its IV, then its subsample map when
	/// `senc` carries them.
	fn entries<'a>(&self, senc: &'a mp4_atom::Senc) -> Result<Vec<&'a [u8]>> {
		let malformed = || Error::InvalidProtection("malformed senc");
		let data = senc.data.as_slice();
		let count = data.get(..4).ok_or_else(malformed)?;
		let count = u32::from_be_bytes(count.try_into().expect("four bytes")) as usize;
		// A constant IV over whole samples leaves every entry empty, so the count is bounded by
		// what a fragment may hold rather than by the box's size.
		if count as u64 > super::MAX_FRAGMENT_SAMPLES {
			return Err(malformed().into());
		}
		let mut entries = Vec::with_capacity(count);
		let mut position = 4;
		for _ in 0..count {
			let start = position;
			position += self.iv_size as usize;
			if senc.use_subsamples {
				let subsamples = data.get(position..position + 2).ok_or_else(malformed)?;
				let subsamples = u16::from_be_bytes(subsamples.try_into().expect("two bytes")) as usize;
				// Each subsample is a 16-bit clear and a 32-bit protected byte count.
				position += 2 + subsamples * 6;
			}
			entries.push(data.get(start..position).ok_or_else(malformed)?);
		}
		if position != data.len() {
			return Err(malformed().into());
		}
		Ok(entries)
	}
}

/// The first `sinf` of an encrypted sample entry in a CMAF init's `moov`, `None` for a clear init
/// or one that does not decode.
pub(crate) fn init_sinf(init: &[u8]) -> Option<mp4_atom::Sinf> {
	use mp4_atom::{Any, DecodeMaybe};

	let mut cursor = std::io::Cursor::new(init);
	while let Ok(Some(atom)) = Any::decode_maybe(&mut cursor) {
		if let Any::Moov(moov) = atom {
			return (moov.trak.iter())
				.flat_map(|trak| &trak.mdia.minf.stbl.stsd.codecs)
				.find_map(sinf)
				.cloned();
		}
	}
	None
}

/// The `sinf` of an encrypted sample entry, `None` for a clear one.
pub(crate) fn sinf(codec: &mp4_atom::Codec) -> Option<&mp4_atom::Sinf> {
	match codec {
		mp4_atom::Codec::Encv(encv) => Some(&encv.sinf),
		mp4_atom::Codec::Enca(enca) => Some(&enca.sinf),
		_ => None,
	}
}

/// The sample entry an encrypted one wraps, or `codec` itself when it is clear.
///
/// The catalog names a protected rendition by this codec, as MSE and EME expect.
pub(crate) fn clear(codec: &mp4_atom::Codec) -> &mp4_atom::Codec {
	match codec {
		mp4_atom::Codec::Encv(encv) => &encv.codec,
		mp4_atom::Codec::Enca(enca) => &enca.codec,
		codec => codec,
	}
}

/// Whether sample groups assign keys per sample (`seig`), which is key rotation.
///
/// Other groups, such as AAC's `roll`, say nothing about keys and pass through.
fn rotates(sgpd: &[mp4_atom::Sgpd], sbgp: &[mp4_atom::Sbgp]) -> bool {
	sgpd.iter().any(|sgpd| sgpd.grouping_type == SEIG) || sbgp.iter().any(|sbgp| sbgp.grouping_type == SEIG)
}

/// Refuse a fragment of an encrypted track whose keys rotate: a `pssh` in its `moof`, or a `seig`
/// sample group in its `traf`.
pub(crate) fn check_moof(moof: &mp4_atom::Moof, traf: &mp4_atom::Traf) -> Result<()> {
	if !moof.pssh.is_empty() || rotates(&traf.sgpd, &traf.sbgp) {
		return Err(Error::KeyRotation.into());
	}
	Ok(())
}

/// Whether `saio` addresses Common Encryption's auxiliary information: an untyped one (which an
/// encrypted track's scheme types) or one typed by a Common Encryption scheme.
fn is_cenc(aux: Option<&mp4_atom::AuxInfo>) -> bool {
	aux.is_none_or(|aux| {
		matches!(
			&<[u8; 4]>::from(aux.aux_info_type),
			b"cenc" | b"cbc1" | b"cens" | b"cbcs"
		)
	})
}

/// Prepare an encrypted track's fragment for re-encoding: refuse auxiliary information the
/// rebuilt fragment would lose, and reserve the one `saio` offset [`point_saio`] fills in.
///
/// The per-sample IVs and subsample maps must sit in the fragment's `senc`, which the rebuilt
/// fragment carries; ones only in the source's `mdat`, or split over several `saio` boxes, would be
/// lost or misaddressed.
pub(crate) fn reserve_saio(traf: &mut mp4_atom::Traf) -> Result<()> {
	let count = traf.saio.iter().filter(|saio| is_cenc(saio.aux_info.as_ref())).count();
	match (traf.senc.is_some(), count) {
		(_, 0) => Ok(()),
		(false, _) => Err(Error::InvalidProtection("encryption auxiliary information outside senc").into()),
		(true, 1) => {
			for saio in traf.saio.iter_mut().filter(|saio| is_cenc(saio.aux_info.as_ref())) {
				// The rebuilt senc holds every sample's information back to back, so one offset
				// addresses all of it, however many runs the source split it over.
				saio.offsets = vec![0];
			}
			Ok(())
		}
		(true, _) => Err(Error::InvalidProtection("encryption auxiliary information split over several saio").into()),
	}
}

/// Point the Common Encryption `saio` of the encoded per-track fragment `moof` at its `senc`'s
/// first sample, relative to the `moof` as `default-base-is-moof` addresses it.
///
/// `moof` must be the encoding of `traf`'s fragment with the offset [`reserve_saio`] reserved, so
/// the offset this fills in keeps its size.
pub(crate) fn point_saio(traf: &mut mp4_atom::Traf, moof: &[u8]) -> Result<()> {
	if !traf.saio.iter().any(|saio| is_cenc(saio.aux_info.as_ref())) {
		return Ok(());
	}
	let senc = find(
		moof,
		&[mp4_atom::Moof::KIND, mp4_atom::Traf::KIND, mp4_atom::Senc::KIND],
	)
	.ok_or(Error::InvalidProtection("rebuilt fragment has no senc"))?;
	// senc's version and flags, then its sample count, precede the first sample's information.
	let offset = senc + 4 + 4;
	for saio in traf.saio.iter_mut().filter(|saio| is_cenc(saio.aux_info.as_ref())) {
		saio.offsets = vec![offset as u64];
	}
	Ok(())
}

/// The offset of the body of the box at `path`, walking from the start of `buf`.
fn find(buf: &[u8], path: &[FourCC]) -> Option<usize> {
	let (first, rest) = path.split_first()?;
	let mut position = 0;
	while position < buf.len() {
		let mut cursor = &buf[position..];
		let header = <mp4_atom::Header as mp4_atom::Decode>::decode(&mut cursor).ok()?;
		let header_size = buf.len() - position - cursor.len();
		let size = header
			.size
			.map(|size| size + header_size)
			.unwrap_or(buf.len() - position);
		let body = position + header_size;
		if header.kind == *first {
			if rest.is_empty() {
				return Some(body);
			}
			// A box's children start right after its header; moof and traf carry no fields.
			let end = (position + size).min(buf.len());
			return find(&buf[body..end], rest).map(|offset| body + offset);
		}
		position = position.checked_add(size)?;
	}
	None
}
