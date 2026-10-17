use std::collections::BTreeMap;

use moq_e2ee::{Name, generation};

use crate::catalog::hang::{Catalog, Extra};

/// Rewrite every track `catalog` names to its physical name, returning the plaintext tracks to carry.
///
/// Rendition keys become the physical name of the track they reference and drop their `track`
/// override, as the draft requires. A track an archive timeline indexes is carried too.
/// Application sections are kept verbatim: a track named only there is not carried.
pub(super) fn protect(
	catalog: &mut Catalog<Extra>,
	generation: &generation::Producer,
) -> crate::Result<BTreeMap<String, Name>> {
	let mut tracks = BTreeMap::new();
	let mut carry = |name: String| -> crate::Result<String> {
		let physical = generation.name(&name)?;
		tracks.insert(name, physical);
		Ok(physical.to_string())
	};

	rename(&mut catalog.video.renditions, &mut carry, |c| {
		(&mut c.track, &c.broadcast)
	})?;
	rename(&mut catalog.audio.renditions, &mut carry, |c| {
		(&mut c.track, &c.broadcast)
	})?;
	rename(&mut catalog.text.renditions, &mut carry, |c| {
		(&mut c.track, &c.broadcast)
	})?;
	rename(&mut catalog.json.tracks, &mut carry, |c| (&mut c.track, &c.broadcast))?;
	rename(&mut catalog.binary.tracks, &mut carry, |c| (&mut c.track, &c.broadcast))?;

	if let Some(m2ts) = &mut catalog.m2ts {
		m2ts.track = carry(std::mem::take(&mut m2ts.track))?;
	}
	if let Some(stats) = &mut catalog.stats {
		stats.track = carry(std::mem::take(&mut stats.track))?;
	}
	if catalog.echo.is_some() {
		return Err(crate::Error::Unencryptable("an echo prefix"));
	}

	if let Some(archive) = &mut catalog.archive {
		if archive.replay.is_some() {
			return Err(crate::Error::Unencryptable("an archive replay"));
		}
		if archive.store.is_some() {
			return Err(crate::Error::Unencryptable("an archive store"));
		}
		let mut timelines = BTreeMap::new();
		for (track, mut timeline) in std::mem::take(&mut archive.timelines) {
			// The key indexes a track, carried like any other unless it is a catalog track the
			// producer publishes itself.
			let track = match track.as_str() {
				hang::Catalog::DEFAULT_NAME | hang::Catalog::COMPRESSED_NAME => generation.name(&track)?.to_string(),
				_ => carry(track)?,
			};
			timeline.track = carry(timeline.track)?;
			timelines.insert(track, timeline);
		}
		archive.timelines = timelines;
	}

	Ok(tracks)
}

/// Re-key one catalog section by physical name, carrying each entry's track.
fn rename<C>(
	entries: &mut BTreeMap<String, C>,
	carry: &mut impl FnMut(String) -> crate::Result<String>,
	fields: fn(&mut C) -> (&mut Option<String>, &Option<moq_net::path::RelativeOwned>),
) -> crate::Result<()> {
	let mut renamed = BTreeMap::new();
	for (id, mut config) in std::mem::take(entries) {
		let (track, broadcast) = fields(&mut config);
		if broadcast.is_some() {
			return Err(crate::Error::ForeignBroadcast);
		}
		let physical = carry(track.take().unwrap_or(id))?;
		if renamed.contains_key(&physical) {
			return Err(hang::Error::Duplicate(physical).into());
		}
		renamed.insert(physical, config);
	}
	*entries = renamed;
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;
	use hang::catalog::{JsonConfig, M2ts, Stats, Timeline, VideoCodec, VideoConfig};

	fn generation() -> generation::Producer {
		moq_e2ee::Credential::new(moq_e2ee::credential::Config {
			context: bytes::Bytes::from_static(b"room"),
			kid: 1,
			secret: [7; moq_e2ee::SECRET_LEN],
		})
		.unwrap()
		.produce()
	}

	#[test]
	fn every_name_is_physical() {
		let generation = generation();
		let mut catalog = Catalog::<Extra>::default();
		catalog
			.video
			.renditions
			.insert("video0.avc3".to_string(), VideoConfig::new(VideoCodec::VP8));
		let mut chat = JsonConfig::new(hang::catalog::Mode::Snapshot);
		chat.track = Some("chat.json".to_string());
		catalog.json.tracks.insert("chat".to_string(), chat);
		catalog.m2ts = Some(M2ts::new("0.ts"));
		catalog.stats = Some(Stats::new("stats.json"));
		let mut archive = hang::catalog::Archive::new();
		archive
			.timelines
			.insert("video0.avc3".to_string(), Timeline::new("video0.avc3.timeline.z", 33));
		archive
			.timelines
			.insert("catalog.json".to_string(), Timeline::new("catalog.json.timeline.z", 33));
		// Indexed but listed in no section.
		archive
			.timelines
			.insert("raw".to_string(), Timeline::new("raw.timeline.z", 33));
		catalog.archive = Some(archive);

		let tracks = protect(&mut catalog, &generation).unwrap();

		let name = |semantic: &str| generation.name(semantic).unwrap().to_string();
		let carried: Vec<_> = tracks.keys().map(String::as_str).collect();
		assert_eq!(
			carried,
			[
				"0.ts",
				"catalog.json.timeline.z",
				"chat.json",
				"raw",
				"raw.timeline.z",
				"stats.json",
				"video0.avc3",
				"video0.avc3.timeline.z"
			]
		);
		assert!(catalog.video.renditions.contains_key(&name("video0.avc3")));
		let chat = &catalog.json.tracks[&name("chat.json")];
		assert_eq!(chat.track, None, "the key is the track");
		assert_eq!(catalog.m2ts.unwrap().track, name("0.ts"));
		assert_eq!(catalog.stats.unwrap().track, name("stats.json"));
		let timelines = &catalog.archive.unwrap().timelines;
		assert_eq!(
			timelines[&name("video0.avc3")],
			Timeline::new(name("video0.avc3.timeline.z"), 33)
		);
		assert_eq!(
			timelines[&name("catalog.json")],
			Timeline::new(name("catalog.json.timeline.z"), 33)
		);
		assert_eq!(timelines[&name("raw")], Timeline::new(name("raw.timeline.z"), 33));
	}

	#[test]
	fn refuses_another_broadcast() {
		let mut catalog = Catalog::<Extra>::default();
		let mut config = VideoConfig::new(VideoCodec::VP8);
		config.broadcast = Some(moq_net::path::RelativeOwned::from("../other.hang"));
		catalog.video.renditions.insert("video".to_string(), config);
		assert!(matches!(
			protect(&mut catalog, &generation()),
			Err(crate::Error::ForeignBroadcast)
		));
	}

	#[test]
	fn refuses_an_echo_prefix() {
		let mut catalog = Catalog::<Extra>::default();
		catalog.echo = Some(hang::catalog::Echo::new("echo"));
		assert!(matches!(
			protect(&mut catalog, &generation()),
			Err(crate::Error::Unencryptable("an echo prefix"))
		));
	}

	#[test]
	fn refuses_a_replay() {
		let mut catalog = Catalog::<Extra>::default();
		let mut archive = hang::catalog::Archive::new();
		archive.replay = Some(moq_net::path::RelativeOwned::from("replay"));
		catalog.archive = Some(archive);
		assert!(matches!(
			protect(&mut catalog, &generation()),
			Err(crate::Error::Unencryptable(_))
		));
	}

	#[test]
	fn refuses_two_entries_on_one_track() {
		let mut catalog = Catalog::<Extra>::default();
		let mut alias = VideoConfig::new(VideoCodec::VP8);
		alias.track = Some("video".to_string());
		catalog
			.video
			.renditions
			.insert("video".to_string(), VideoConfig::new(VideoCodec::VP8));
		catalog.video.renditions.insert("alias".to_string(), alias);
		assert!(matches!(
			protect(&mut catalog, &generation()),
			Err(crate::Error::Hang(hang::Error::Duplicate(_)))
		));
	}
}
