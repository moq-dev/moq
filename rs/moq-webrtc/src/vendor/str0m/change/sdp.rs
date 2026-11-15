//! Strategy that amends the [`Rtc`] via SDP OFFER/ANSWER negotiation.

use std::fmt;
use std::ops::{Deref, DerefMut};
use tracing::{debug, warn};

use crate::vendor::str0m::Rtc;
use crate::vendor::str0m::RtcError;
use crate::vendor::str0m::crypto::Fingerprint;
use crate::vendor::str0m::format::CodecConfig;
use crate::vendor::str0m::format::PayloadParams;
use crate::vendor::str0m::media::{Media, Rids, Simulcast};
use crate::vendor::str0m::packet::MediaKind;
use crate::vendor::str0m::poll::RtcMut;
use crate::vendor::str0m::rtp_::MidRid;
use crate::vendor::str0m::rtp_::Rid;
use crate::vendor::str0m::rtp_::{Direction, Extension, ExtensionMap, Mid, Pt, Ssrc};
use crate::vendor::str0m::sdp::{self, MediaAttribute, MediaLine, MediaType, Msid, Sdp};
use crate::vendor::str0m::sdp::{Proto, SessionAttribute, Setup, SimulcastGroups};
use crate::vendor::str0m::session::Session;
use crate::vendor::str0m::{Candidate, IceCreds};
use crate::vendor::str0m_proto::Id;

pub use crate::vendor::str0m::sdp::{SdpAnswer, SdpOffer};
use crate::vendor::str0m::streams::{DEFAULT_RTX_CACHE_DURATION, DEFAULT_RTX_RATIO_CAP, Streams};

/// Changes to the Rtc via SDP Offer/Answer dance.
///
/// Only one local offer/answer exchange can be pending at a time. Applications must track
/// queued changes outside str0m and wait for the current exchange to finish before creating
/// another offer. If an incoming offer interrupts the exchange, recreate the desired local
/// changes against the updated session after answering that offer.
pub struct SdpApi<'a> {
	// `RtcMut` arms the readiness on mutable deref only; see [`crate::vendor::str0m::poll`].
	rtc: RtcMut<'a>,
	changes: Changes,
}

impl<'a> SdpApi<'a> {
	pub(crate) fn new(rtc: &'a mut Rtc) -> Self {
		SdpApi {
			rtc: RtcMut::new(rtc),
			changes: Changes::default(),
		}
	}

	fn new_mid(&self) -> Mid {
		loop {
			let mid = self.rtc.new_mid();
			if !self.changes.has_mid(mid) {
				break mid;
			}
		}
	}

	/// Accept an [`SdpOffer`] from the remote peer. If this call returns successfully, the
	/// changes will have been made to the session. The resulting [`SdpAnswer`] should be
	/// sent to the remote peer.
	///
	/// <b>Note. Pending changes from a previous non-completed [`SdpApi`][super::SdpApi] will be
	/// considered rolled back when calling this function.</b> Any outstanding [`SdpPendingOffer`]
	/// is invalidated, even if this call returns an error. The application must track and
	/// recreate any local changes it still wants to negotiate.
	///
	/// The incoming SDP is validated in various ways which can cause this call to fail.
	/// Example of such problems would be an SDP without any m-lines, missing `a=fingerprint`
	/// or if `a=group` doesn't match the number of m-lines.
	///
	/// ```ignore
	/// # use std::time::Instant;
	/// # use crate::vendor::str0m::Rtc;
	/// # use crate::vendor::str0m::change::{SdpOffer};
	/// // obtain offer from remote peer.
	/// let json_offer: &[u8] = todo!();
	/// let offer: SdpOffer = serde_json::from_slice(json_offer).unwrap();
	///
	/// let mut rtc = Rtc::new(Instant::now());
	/// let answer = rtc.sdp_api().accept_offer(offer).unwrap();
	///
	/// // send json_answer to remote peer.
	/// let json_answer = serde_json::to_vec(&answer).unwrap();
	/// ```
	pub fn accept_offer(mut self, offer: SdpOffer) -> Result<SdpAnswer, RtcError> {
		debug!("Accept offer");

		// Invalidate any outstanding PendingOffer.
		self.rtc.next_change_id();

		if offer.media_lines.is_empty() {
			return Err(RtcError::RemoteSdp("No m-lines in offer".into()));
		}

		if self.rtc.ice.ice_lite() && offer.session.ice_lite() {
			return Err(RtcError::RemoteSdp("Both peers being ICE-Lite not supported".into()));
		}

		add_ice_details(&mut self.rtc, &offer, None)?;

		if self.rtc.remote_fingerprint.is_none() {
			if let Some(f) = offer.fingerprint() {
				self.rtc.remote_fingerprint = Some(f);
			} else {
				self.rtc.disconnect();
				return Err(RtcError::RemoteSdp("missing a=fingerprint".into()));
			}
		}

		if !self.rtc.dtls.is_inited() {
			// The side that makes the first offer is the controlling side, unless they
			// are ICE Lite, in which case the roles are reversed (see RFC 5245).
			self.rtc.ice.set_controlling(offer.session.ice_lite());
		}

		// Ensure setup=active/passive is corresponding remote and init dtls.
		init_dtls(&mut self.rtc, &offer)?;

		// Modify session with offer. An m=application line is answered rejected.
		apply_offer(&mut self.rtc.session, offer)?;

		let params = AsSdpParams::new(&self.rtc, None);
		let sdp = as_sdp(&self.rtc.session, params);

		debug!("Create answer");
		Ok(sdp.into())
	}

	/// Accept an answer to a previously created [`SdpOffer`].
	///
	/// This function returns an [`RtcError::ChangesOutOfOrder`] if we have created and applied another
	/// [`SdpApi`][super::SdpApi] before calling this. The same also happens if we use
	/// [`SdpApi::accept_offer()`] before using this pending instance.
	///
	/// ```ignore
	/// # use std::time::Instant;
	/// # use crate::vendor::str0m::Rtc;
	/// # use crate::vendor::str0m::media::{MediaKind, Direction};
	/// # use crate::vendor::str0m::change::SdpAnswer;
	/// let mut rtc = Rtc::new(Instant::now());
	///
	/// let mut changes = rtc.sdp_api();
	/// let mid = changes.add_media(MediaKind::Audio, Direction::SendOnly, None, None, None);
	/// let (offer, pending) = changes.apply().unwrap();
	///
	/// // send offer to remote peer, receive answer back
	/// let answer: SdpAnswer = todo!();
	///
	/// rtc.sdp_api().accept_answer(pending, answer).unwrap();
	/// ```
	pub fn accept_answer(mut self, pending: SdpPendingOffer, answer: SdpAnswer) -> Result<(), RtcError> {
		debug!("Accept answer");

		// Ensure we don't use the wrong changes below. We must use that of pending.
		drop(self.changes);

		if !self.rtc.is_correct_change_id(pending.change_id) {
			return Err(RtcError::ChangesOutOfOrder);
		}

		if self.rtc.ice.ice_lite() && answer.session.ice_lite() {
			return Err(RtcError::RemoteSdp("Both peers being ICE-Lite not supported".into()));
		}

		add_ice_details(&mut self.rtc, &answer, Some(&pending))?;

		// Ensure setup=active/passive is corresponding remote and init dtls.
		init_dtls(&mut self.rtc, &answer)?;

		if self.rtc.remote_fingerprint.is_none() {
			if let Some(f) = answer.fingerprint() {
				self.rtc.remote_fingerprint = Some(f);
			} else {
				self.rtc.disconnect();
				return Err(RtcError::RemoteSdp("missing a=fingerprint".into()));
			}
		}

		// Modify session with answer
		apply_answer(&mut self.rtc.session, pending.changes, answer)?;

		Ok(())
	}

	/// Test if any changes have been made.
	///
	/// If changes have been made, nothing happens until we call [`SdpApi::apply()`].
	///
	/// ```ignore
	/// # #[cfg(any())] {
	/// # use std::time::Instant;
	/// # use crate::vendor::str0m::{Rtc, media::MediaKind, media::Direction};
	/// let mut rtc = Rtc::new(Instant::now());
	///
	/// let mut changes = rtc.sdp_api();
	/// assert!(!changes.has_changes());
	///
	/// let mid = changes.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
	/// assert!(changes.has_changes());
	/// # }
	/// ```
	pub fn has_changes(&self) -> bool {
		!self.changes.0.is_empty()
	}

	/// Add audio or video media and get the `mid` that will be used.
	///
	/// Each call will result in a new m-line in the offer identified by the [`Mid`].
	///
	/// The mid is not valid to use until the SDP offer-answer dance is complete and
	/// the mid been advertised via [`Event::MediaAdded`][crate::vendor::str0m::Event::MediaAdded].
	///
	/// * `stream_id` is used to synchronize media. It is `a=msid-semantic: WMS <streamId>` line in SDP.
	/// * `track_id` is becomes both the track id in `a=msid <streamId> <trackId>` as well as the
	///   CNAME in the RTP SDES.
	///
	/// ```ignore
	/// # #[cfg(any())] {
	/// # use std::time::Instant;
	/// # use crate::vendor::str0m::{Rtc, media::MediaKind, media::Direction};
	/// let mut rtc = Rtc::new(Instant::now());
	///
	/// let mut changes = rtc.sdp_api();
	///
	/// let mid = changes.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
	/// # }
	/// ```
	pub fn add_media(
		&mut self,
		kind: MediaKind,
		dir: Direction,
		stream_id: Option<String>,
		track_id: Option<String>,
		simulcast: Option<crate::vendor::str0m::media::Simulcast>,
	) -> Mid {
		let mid = self.new_mid();

		// https://www.rfc-editor.org/rfc/rfc8830
		// msid-id = 1*64token-char
		fn is_token_char(c: &char) -> bool {
			// token-char = %x21 / %x23-27 / %x2A-2B / %x2D-2E / %x30-39
			// / %x41-5A / %x5E-7E
			let u = *c as u32;
			u == 0x21
				|| (0x23..=0x27).contains(&u)
				|| (0x2a..=0x2b).contains(&u)
				|| (0x2d..=0x2e).contains(&u)
				|| (0x30..=0x39).contains(&u)
				|| (0x41..=0x5a).contains(&u)
				|| (0x5e..0x7e).contains(&u)
		}

		let stream_id = if let Some(stream_id) = stream_id {
			stream_id.chars().filter(is_token_char).take(64).collect()
		} else {
			Id::<20>::random().to_string()
		};

		let track_id = if let Some(track_id) = track_id {
			track_id.chars().filter(is_token_char).take(64).collect()
		} else {
			Id::<20>::random().to_string()
		};

		let mut ssrcs = Vec::new();

		// Main SSRC, not counting RTX.
		let main_ssrc_count = simulcast.as_ref().map(|s| s.send.len()).unwrap_or(1);

		for _ in 0..main_ssrc_count {
			let rtx = kind.is_video().then(|| self.rtc.session.streams.new_ssrc());
			ssrcs.push((self.rtc.session.streams.new_ssrc(), rtx));
		}

		// TODO: let user configure stream/track name.
		let msid = Msid {
			stream_id,
			track_id: track_id.clone(),
		};

		let add = AddMedia {
			mid,
			cname: track_id,
			msid,
			kind,
			dir,
			ssrcs,
			simulcast,

			// Added later
			pts: vec![],
			exts: ExtensionMap::empty(),
			index: 0,
		};

		self.changes.0.push(Change::AddMedia(add));
		mid
	}

	/// Change the direction of an already existing media.
	///
	/// All media have a direction. The media can be added by this side via
	/// [`SdpApi::add_media()`] or by the remote peer. Either way, the direction
	/// of the line can be changed at any time.
	///
	/// It's possible to set the direction [`Direction::Inactive`] for media that
	/// will not be used by the session anymore.
	///
	/// If the direction is set for media that doesn't exist, or if the direction is
	/// the same that's already set [`SdpApi::apply()`] not require a negotiation.
	pub fn set_direction(&mut self, mid: Mid, dir: Direction) {
		let changed = self.rtc.session.set_direction(mid, dir);

		if changed {
			self.changes.0.push(Change::Direction(mid, dir));
		}
	}

	/// Stop an already existing media.
	///
	/// The next generated offer emits the m-line with port 0 and excludes
	/// it from the BUNDLE group, per [RFC 8843] §7.5.3. The remote
	/// transceiver transitions to the "stopped" state and the m-line slot
	/// becomes eligible for recycling.
	///
	/// Unlike [`SdpApi::set_direction()`] with [`Direction::Inactive`],
	/// a stopped m-line cannot be reactivated.
	///
	/// If the media doesn't exist, or is already stopped, [`SdpApi::apply()`]
	/// will not require a negotiation.
	///
	/// [RFC 8843]: https://datatracker.ietf.org/doc/html/rfc8843#section-7.5.3
	pub fn stop_media(&mut self, mid: Mid) {
		let changed = self.rtc.session.stop_media(mid);

		if changed {
			self.changes.0.push(Change::Direction(mid, Direction::Inactive));
		}
	}

	/// Perform an ICE restart.
	///
	/// Only one ICE restart can be pending at the time. Calling this repeatedly removes any other
	/// pending ICE restart.
	///
	/// The local ICE candidates can be kept as is, or be cleared out, in which case new ice
	/// candidates must be added via [`Rtc::add_local_candidate`] before connectivity can be
	/// re-established.
	///
	/// Returns the new ICE credentials that will be used going forward.
	pub fn ice_restart(&mut self, keep_local_candidates: bool) -> IceCreds {
		self.changes.retain(|c| !matches!(c, Change::IceRestart(_, _)));

		let new_creds = IceCreds::new();
		self.changes
			.push(Change::IceRestart(new_creds.clone(), keep_local_candidates));

		new_creds
	}

	/// Attempt to apply the changes made.
	///
	/// If this returns [`SdpOffer`], the caller the changes are
	/// not happening straight away, and the caller is expected to do a negotiation with the remote
	/// peer and apply the answer using [`SdpPendingOffer`].
	///
	/// In case this returns `None`, there were no changes.
	///
	/// The [`SdpPendingOffer`] is valid until the next time we call this function, at which
	/// point using it will raise an error. Using [`SdpApi::accept_offer()`] will also invalidate
	/// the current [`SdpPendingOffer`].
	///
	/// ```ignore
	/// # #[cfg(any())] {
	/// # use std::time::Instant;
	/// # use crate::vendor::str0m::Rtc;
	/// let mut rtc = Rtc::new(Instant::now());
	///
	/// let changes = rtc.sdp_api();
	/// assert!(changes.apply().is_none());
	/// # }
	/// ```
	pub fn apply(mut self) -> Option<(SdpOffer, SdpPendingOffer)> {
		if self.changes.is_empty() {
			return None;
		}

		let change_id = self.rtc.next_change_id();

		let offer = create_offer(&mut self.rtc, &self.changes);
		let pending = SdpPendingOffer {
			change_id,
			changes: self.changes,
		};
		debug!("Create offer");
		Some((offer, pending))
	}
}

/// Pending offer from a previous [`Rtc::sdp_api()`] call.
///
/// Represents one local negotiation change set, which can contain multiple media
/// changes, and allows accepting its remote answer. It is not a queue of pending negotiations.
/// Creating another offer with [`SdpApi::apply()`] invalidates it, as does calling
/// [`SdpApi::accept_offer()`]. Invalidated changes cannot be reapplied through this token.
/// Applications must track queued changes outside str0m and recreate any desired changes
/// after an interrupted negotiation against the current session state.
///
/// No changes have been made to the session
/// before we call [`SdpApi::accept_answer()`], which means that rolling back a
/// change is as simple as dropping this instance.
///
/// ```ignore
/// # use std::time::Instant;
/// # use crate::vendor::str0m::Rtc;
/// # use crate::vendor::str0m::media::{MediaKind, Direction};
/// # use crate::vendor::str0m::change::SdpAnswer;
/// let mut rtc = Rtc::new(Instant::now());
///
/// let mut changes = rtc.sdp_api();
/// let mid = changes.add_media(MediaKind::Audio, Direction::SendOnly, None, None, None);
/// let (offer, pending) = changes.apply().unwrap();
///
/// // send offer to remote peer, receive answer back
/// let answer: SdpAnswer = todo!();
///
/// rtc.sdp_api().accept_answer(pending, answer).unwrap();
/// ```
pub struct SdpPendingOffer {
	change_id: usize,
	changes: Changes,
}

#[derive(Default)]
pub(crate) struct Changes(pub Vec<Change>);

impl Changes {
	/// Details of the active ICE restart, if any.
	///
	/// Returns the new local ICE credentials and the whether to keep local ICE candidates if an
	/// ICE restart has been initiated in the offer, otherwise [`None`].
	fn ice_restart(&self) -> Option<(IceCreds, bool)> {
		self.iter().find_map(|c| match c {
			Change::IceRestart(creds, keep_local_candidates) => Some((creds.clone(), *keep_local_candidates)),
			_ => None,
		})
	}
}

#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum Change {
	AddMedia(AddMedia),
	Direction(Mid, Direction),
	IceRestart(IceCreds, bool),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AddMedia {
	pub mid: Mid,
	pub cname: String,
	pub msid: Msid,
	pub kind: MediaKind,
	pub dir: Direction,
	pub ssrcs: Vec<(Ssrc, Option<Ssrc>)>,
	pub simulcast: Option<Simulcast>,

	// pts and index are filled in when creating the SDP OFFER.
	// The default PT order is set by the Session (BUNDLE).
	// TODO: We can make this configurable here too.
	pub pts: Vec<Pt>,
	pub exts: ExtensionMap,
	pub index: usize,
}

impl Deref for Changes {
	type Target = Vec<Change>;

	fn deref(&self) -> &Self::Target {
		&self.0
	}
}

impl DerefMut for Changes {
	fn deref_mut(&mut self) -> &mut Self::Target {
		&mut self.0
	}
}

fn create_offer(rtc: &mut Rtc, changes: &Changes) -> SdpOffer {
	if !rtc.dtls.is_inited() {
		// The side that makes the first offer is the controlling side, unless they
		// are ICE Lite, in which case the roles are reversed (see RFC 5245).
		rtc.ice.set_controlling(!rtc.ice.ice_lite());
	}

	let params = AsSdpParams::new(rtc, Some(changes));
	let sdp = as_sdp(&rtc.session, params);

	sdp.into()
}

fn add_ice_details(rtc: &mut Rtc, sdp: &Sdp, pending: Option<&SdpPendingOffer>) -> Result<(), RtcError> {
	let Some(creds) = sdp.ice_creds() else {
		return Err(RtcError::RemoteSdp("missing a=ice-ufrag/pwd".into()));
	};

	// If we are handling an **offer** from the remote, differing ICE credentials indicate an ICE
	// restart initiated by the remote.
	//
	// If we are handling an **answer** from the remote, differing ICE credentials indicate an
	// acceptance of an ICE restart we requested.
	let ice_restart = match rtc.ice.remote_credentials() {
		Some(v) => *v != creds,
		None => false,
	};
	if ice_restart {
		let (new_local_creds, keep_local_candidates) = if let Some(pending) = pending {
			// Since we have a pending, this is an answer to our offer.
			pending.changes.ice_restart().ok_or_else(||
                // Answer contained changed remote creds, indicating an ice restart
                // but since we have no pending ice-creds, we didn't initiate it
                // Ice restart in an ANSWER breaks spec.
                RtcError::RemoteSdp(
                    "Ice restart in answer without one in the preceeding offer".into(),
                ))?
		} else {
			// The remote OFFER had an ice restart, and we need to respond with
			// new credentials in the ANSWER.
			(IceCreds::new(), true)
		};

		rtc.ice.ice_restart(new_local_creds.clone(), keep_local_candidates);
	}

	rtc.ice.set_remote_credentials(creds);

	for r in sdp.ice_candidates() {
		rtc.ice.add_remote_candidate(r.clone());
	}

	Ok(())
}

fn init_dtls(rtc: &mut Rtc, remote_sdp: &Sdp) -> Result<(), RtcError> {
	let setup = match remote_sdp.setup() {
		Some(v) => match v {
			// Remote being ActPass, we take Passive role.
			Setup::ActPass => Setup::Passive,
			_ => v.invert(),
		},

		None => {
			warn!("Missing a=setup line");
			Setup::Passive
		}
	};

	let active = setup == Setup::Active;
	rtc.init_dtls(active)?;

	Ok(())
}

fn as_sdp(session: &Session, params: AsSdpParams) -> Sdp {
	let (media_lines, mids, stream_ids) = {
		let mut v = as_media_lines(session);

		let mut new_lines = vec![];

		// When creating new m-lines from the pending changes, the m-line index starts from this.
		let new_index_start = v.len();

		// If there are additions in the pending changes, prepend them now.
		if let Some(pending) = params.pending {
			new_lines = pending
				.as_new_medias(new_index_start, &session.codec_config, &session.exts)
				.collect();
		}

		// Add potentially new m-lines to the existing ones.
		v.extend(new_lines.iter().map(|n| n as &dyn AsSdpMediaLine));

		// Turn into sdp::MediaLine (m-line).
		let mut lines = v
			.iter()
			.map(|m| {
				// Candidates go on the first accepted line, added below.
				let attrs = params.media_attributes(false);

				// Already made send stream SSRCs
				let mut ssrcs = session.streams.ssrcs_tx(m.mid());

				// Merged with pending stream SSRCs
				if let Some(pending) = params.pending {
					ssrcs.extend(pending.ssrcs_for_mid(m.mid()))
				}

				let params: Vec<_> = session.codec_config.all_for_kind(m.kind()).cloned().collect();

				m.as_media_line(attrs, &ssrcs, &session.exts, &params)
			})
			.collect::<Vec<_>>();

		if let Some(pending) = params.pending {
			pending.apply_to(&mut lines);
		}

		// Candidates belong in the first BUNDLE mid, which is the first accepted line: a
		// rejected line ahead of it, such as an offered data channel, is out of the group.
		let first = lines.iter().position(|l| !l.disabled).unwrap_or(0);
		if let Some(line) = lines.get_mut(first) {
			let candidates = params.candidates.iter().map(|c| MediaAttribute::Candidate(c.clone()));
			line.attrs.splice(0..0, candidates);
		}

		// Mids go into the session part of the SDP.
		// Rejected (disabled) m-lines must not be part of the BUNDLE group.
		let mids = lines.iter().filter(|l| !l.disabled).map(|l| l.mid()).collect();

		let mut stream_ids = vec![];
		for msid in v.iter().filter_map(|v| v.msid()) {
			if !stream_ids.contains(&msid.stream_id) {
				stream_ids.push(msid.stream_id.clone());
			}
		}

		(lines, mids, stream_ids)
	};

	// AllowMixedExts adds "a=extmap-allow-mixed" at session level to signal
	// support for mixing one-byte and two-byte RTP header extensions.
	// TODO: It would make sense to perform an actual negotiation, however
	//       just adding this line should work fine:
	// https://github.com/meetecho/janus-gateway/blob/d2e74fdf9bb8aa7a39ed68ed28394afe1e0cd22d/src/sdp.c#L1519
	let mut attrs = vec![
		SessionAttribute::Group {
			typ: "BUNDLE".into(),
			mids,
		},
		SessionAttribute::AllowMixedExts,
		SessionAttribute::MsidSemantic {
			semantic: "WMS".to_string(),
			stream_ids,
		},
	];

	if session.ice_lite {
		attrs.push(SessionAttribute::IceLite);
	}

	Sdp {
		session: sdp::Session {
			id: session.id(),
			bw: None,
			attrs,
		},
		media_lines,
	}
}

fn apply_offer(session: &mut Session, offer: SdpOffer) -> Result<(), RtcError> {
	offer.assert_consistency()?;

	update_session(session, &offer);

	let bundle_mids = offer.bundle_mids();
	let new_lines = sync_medias(session, &offer, true).map_err(RtcError::RemoteSdp)?;

	add_new_lines(session, &new_lines, true, bundle_mids).map_err(RtcError::RemoteSdp)?;

	ensure_stream_tx(session);

	Ok(())
}

fn apply_answer(session: &mut Session, pending: Changes, answer: SdpAnswer) -> Result<(), RtcError> {
	answer.assert_consistency()?;

	update_session(session, &answer);

	let bundle_mids = answer.bundle_mids();
	let new_lines = sync_medias(session, &answer, false).map_err(RtcError::RemoteSdp)?;

	// The new_lines from the answer must correspond to what we sent in the offer.
	if let Some(err) = pending.ensure_correct_answer(&new_lines) {
		return Err(RtcError::RemoteSdp(err));
	}

	add_new_lines(session, &new_lines, false, bundle_mids).map_err(RtcError::RemoteSdp)?;

	// Add all pending changes (since we pre-allocated SSRC communicated in the Offer).
	add_pending_changes(session, pending);

	ensure_stream_tx(session);

	Ok(())
}

fn ensure_stream_tx(session: &mut Session) {
	for media in &session.medias {
		// Only make send streams when we have to.
		if !media.direction().is_sending() {
			continue;
		}

		let mut rids: Vec<Option<Rid>> = vec![];

		if let Some(sim) = media.simulcast() {
			for layer in &*sim.send {
				let rid: Rid = layer.restriction_id.0.as_str().into();
				rids.push(Some(rid));
			}
		} else {
			rids.push(None);
		}

		// If any payload param has RTX, we need to prepare for RTX. This is because we always
		// communicate a=ssrc lines, which need to be complete with main and RTX SSRC.
		let has_rtx = session
			.codec_config
			.iter()
			.filter(|p| media.remote_pts().contains(&p.pt))
			.any(|p| p.resend().is_some());

		for rid in rids {
			let midrid = MidRid(media.mid(), rid);

			// If we already have the stream, we don't make any new one.
			let has_stream = session.streams.stream_tx_by_midrid(midrid).is_some();

			if has_stream {
				continue;
			}

			let (ssrc, rtx) = if has_rtx {
				let (ssrc, rtx) = session.streams.new_ssrc_pair();
				(ssrc, Some(rtx))
			} else {
				let ssrc = session.streams.new_ssrc();
				(ssrc, None)
			};

			let stream = session.streams.declare_stream_tx(ssrc, rtx, midrid);

			// Configure cache size
			let size = if media.kind().is_audio() {
				session.send_buffer_audio
			} else {
				session.send_buffer_video
			};

			stream.set_rtx_cache(size, DEFAULT_RTX_CACHE_DURATION, DEFAULT_RTX_RATIO_CAP);
		}
	}
}

fn add_pending_changes(session: &mut Session, pending: Changes) {
	// For pending AddMedia, we have outgoing SSRC communicated that needs to be added.
	for change in pending.0 {
		let add_media = match change {
			Change::AddMedia(v) => v,
			_ => continue,
		};

		let media = session
			.medias
			.iter_mut()
			.find(|m| m.mid() == add_media.mid)
			.expect("Media to be added for pending mid");

		// the cname/msid has already been communicated in the offer, we need to kep
		// it the same once the m-line is created.
		media.set_cname(add_media.cname);
		media.set_msid(add_media.msid);

		// If there are RIDs, the SSRC order matches that of the rid order.
		let layers = add_media.simulcast.map(|x| x.send).unwrap_or(vec![]);

		for (i, (ssrc, rtx)) in add_media.ssrcs.into_iter().enumerate() {
			let maybe_layer = layers.get(i).cloned();
			let midrid = MidRid(add_media.mid, maybe_layer.map(|layer| layer.rid));

			let stream = session.streams.declare_stream_tx(ssrc, rtx, midrid);

			let size = if media.kind().is_audio() {
				session.send_buffer_audio
			} else {
				session.send_buffer_video
			};

			stream.set_rtx_cache(size, DEFAULT_RTX_CACHE_DURATION, DEFAULT_RTX_RATIO_CAP);
		}
	}
}

/// Compares m-lines in Sdp with that already in the session.
///
/// * Existing m-lines can apply changes (such as direction change).
/// * New m-lines are returned to the caller paired with the session
///   index they should occupy. The index is normally the next free slot,
///   but can be a recycled slot (RFC 8829 §5.2.2) when the remote has
///   replaced a previously disabled Media with a new mid.
fn sync_medias<'a>(session: &mut Session, sdp: &'a Sdp, is_offer: bool) -> Result<Vec<(usize, &'a MediaLine)>, String> {
	let mut new_lines = Vec::with_capacity(sdp.media_lines.len());
	let bundle_mids = sdp.bundle_mids();

	for (idx, m) in sdp.media_lines.iter().enumerate() {
		// First, match existing m-lines.
		match m.typ {
			MediaType::Application => {
				if let Some((_, index)) = session.app() {
					if idx != *index {
						return index_err(m.mid());
					}
					continue;
				}
			}
			MediaType::Audio | MediaType::Video => {
				if let Some(media) = session.medias.iter_mut().find(|l| l.mid() == m.mid()) {
					if idx != media.index() {
						return index_err(m.mid());
					}

					update_media(
						media,
						m,
						&session.codec_config,
						&session.exts,
						&mut session.streams,
						bundle_mids,
					);

					continue;
				}

				// Unknown mid at an index held by a disabled Media means
				// the remote has recycled the slot (RFC 8829 §5.2.2).
				// Recycling is only permitted in offers; an answer that
				// rewires a mid at a stopped slot is malformed.
				if let Some(pos) = session.medias.iter().position(|l| l.index() == idx) {
					if !is_offer {
						return Err(format!(
							"Answer recycles stopped m-line (not permitted per RFC 8829 §5.2.2): {}",
							m.mid()
						));
					}
					if !session.medias[pos].disabled() {
						return index_err(m.mid());
					}
					let retired_mid = session.medias[pos].mid();
					session.medias.swap_remove(pos);
					session.streams.remove_streams_by_mid(retired_mid);
				}
			}
			_ => {
				continue;
			}
		}

		// Second, discover new m-lines.
		new_lines.push((idx, m));
	}

	fn index_err<T>(mid: Mid) -> Result<T, String> {
		Err(format!("Changed order for m-line with mid: {mid}"))
	}

	Ok(new_lines)
}

/// Adds new m-lines as found in an offer or answer.
///
/// Each entry in `new_lines` pairs an m-line with the session index it
/// should occupy.
fn add_new_lines(
	session: &mut Session,
	new_lines: &[(usize, &MediaLine)],
	is_offer: bool,
	bundle_mids: Option<&[Mid]>,
) -> Result<(), String> {
	for (idx, m) in new_lines {
		let idx = *idx;

		if m.typ.is_media() {
			let mut media = Media::from_remote_media_line(m, idx, is_offer);

			// For disabled (rejected) m-lines, don't fire open event
			// and the direction will be set to Inactive by update_media.
			// In max-bundle, port=0 with the MID in BUNDLE group is NOT rejected.
			let is_in_bundle = bundle_mids.map(|mids| mids.contains(&m.mid())).unwrap_or(false);
			let is_rejected = m.disabled && !is_in_bundle;
			media.need_open_event = is_offer && !is_rejected;

			// Match/remap remote params.
			session.codec_config.update_params(&m.rtp_params(), m.direction());

			// Remap the extension to that of the answer.
			session.exts.remap(&m.extmaps());

			update_media(
				&mut media,
				m,
				&session.codec_config,
				&session.exts,
				&mut session.streams,
				bundle_mids,
			);

			session.add_media(media);
		} else if m.typ.is_channel() {
			session.set_app(m.mid(), idx)?;
		} else {
			return Err(format!("New m-line is neither media nor channel: {}", m.mid()));
		}
	}

	Ok(())
}

/// Update session level properties like
/// Extensions from offer or answer.
fn update_session(session: &mut Session, sdp: &Sdp) {
	// Does any m-line contain a a=rtcp-fb:xx transport-cc?
	let has_transport_cc = sdp
		.media_lines
		.iter()
		.any(|m| m.rtp_params().iter().any(|p| p.fb_transport_cc));

	// Is the session level sequence number enabled?
	let has_twcc_header = session.exts.id_of(Extension::TransportSequenceNumber).is_some();

	// Since twcc feedback is session wide we enable it if there are _any_
	// m-line with a a=rtcp-fb transport-cc parameter and the sequence number
	// header is enabled. It can later be disabled for specific m-lines based
	// on the extensions map.
	if has_transport_cc && has_twcc_header {
		session.enable_twcc_feedback();
	}
}

/// Returns all media/channels as `AsMediaLine` trait.
fn as_media_lines(session: &Session) -> Vec<&dyn AsSdpMediaLine> {
	let mut v = vec![];

	if let Some(app) = session.app() {
		v.push(app as &dyn AsSdpMediaLine);
	}
	v.extend(session.medias().iter().map(|m| m as &dyn AsSdpMediaLine));
	v.sort_by_key(|f| f.index());
	v
}

fn update_media(
	media: &mut Media,
	m: &MediaLine,
	config: &CodecConfig,
	exts: &ExtensionMap,
	streams: &mut Streams,
	bundle_mids: Option<&[Mid]>,
) {
	// If the m-line has port=0, it could mean:
	// 1. The m-line is rejected (not in BUNDLE group)
	// 2. The m-line is bundled with another m-line (max-bundle format, RFC 8843)
	//
	// In max-bundle, secondary m-lines use port=0 to indicate they share transport
	// with the first m-line. This is NOT a rejection.
	let is_in_bundle = bundle_mids.map(|mids| mids.contains(&m.mid())).unwrap_or(false);
	let is_rejected = m.disabled && !is_in_bundle;

	if is_rejected {
		if !media.disabled() {
			debug!(
				"Mid ({}) is rejected (port=0, not in BUNDLE), setting to Inactive",
				media.mid()
			);
		}
		media.mark_stopped();
		media.set_direction(Direction::Inactive);
		return;
	}

	// Direction changes
	//
	// All changes come from the other side, either via an incoming OFFER
	// or a ANSWER from our OFFER. Either way, the direction is inverted to
	// how we have it locally.
	let new_dir = m.direction().invert();
	//
	let change_direction_disallowed =
		!media.remote_created() && media.direction() == Direction::Inactive && new_dir == Direction::SendOnly;

	if change_direction_disallowed {
		debug!(
			"Ignore attempt to change inactive to recvonly by remote peer for locally created mid: {}",
			media.mid()
		);
	} else {
		media.set_direction(new_dir);
	}

	if new_dir.is_sending() {
		// The other side has declared how it EXPECTING to receive. We must only send
		// the RIDs declared in the answer.
		let rids = m.rids();
		let rid_tx = if rids.is_empty() {
			Rids::None
		} else {
			Rids::Specific(rids)
		};
		media.set_rid_tx(rid_tx);
	}
	if new_dir.is_receiving() {
		// The other side has declared what it proposes to send. We are accepting it.
		let rids = m.rids();
		let rid_rx = if rids.is_empty() {
			Rids::Any
		} else {
			Rids::Specific(rids)
		};
		media.set_rid_rx(rid_rx);
	}

	// Narrowing/ordering of of PT
	let pts: Vec<Pt> = m
		.rtp_params()
		.into_iter()
		.filter_map(|p| config.sdp_match_remote(p, m.direction()))
		.collect();
	media.set_remote_pts(pts);

	let mut remote_extmap = ExtensionMap::empty();
	for (id, ext) in m.extmaps().into_iter() {
		// The remapping of extensions should already have happened, which
		// means the ID are matching in the session to the remote.

		// Does the ID exist in session?
		let in_session = match exts.lookup(id) {
			Some(v) => v,
			None => continue,
		};

		if in_session != ext {
			// Don't set any extensions that aren't enabled in Session.
			continue;
		}

		// Use the Extension from session, since there might be a special
		// serializer for cases like VLA.
		remote_extmap.set(id, in_session.clone());
	}
	media.set_remote_extmap(remote_extmap);

	// Simulcast configuration. This applies to both directions: every later SDP for the
	// m-line is generated from the Media, and must repeat the negotiated a=rid and
	// a=simulcast lines (RFC 8829 5.2.2, RFC 8853 5.3.4).
	if let Some(s) = m.simulcast() {
		if s.is_munged {
			warn!("Not supporting simulcast via munging SDP");
		} else if media.simulcast().is_none() {
			// Invert before setting, since it has a recv and send config.
			media.set_simulcast(s.invert());
		}
	}

	// SSRC changes
	// This will always be for ReceiverSource since any incoming a=ssrc line will be
	// about the remote side's SSRC.
	if !new_dir.is_receiving() {
		return;
	}

	// Only use pre-communicated SSRC if we are running without simulcast.
	// We found a bug in FF where the order of the simulcast lines does not
	// correspond to the order of the simulcast declarations. In this case
	// it's better to fall back on mid/rid dynamic mapping.
	if m.simulcast().is_some() {
		return;
	}

	let infos = m.ssrc_info();
	let main = infos.iter().filter(|i| i.repairs.is_none());

	for i in main {
		// TODO: If the remote is communicating _BOTH_ rid and a=ssrc this will fail.
		debug!("Adding pre-communicated SSRC: {:?}", i);
		let repair_ssrc = infos.iter().find(|r| r.repairs == Some(i.ssrc)).map(|r| r.ssrc);

		// If remote communicated a main a=ssrc, but no RTX, we will not send nacks.
		let midrid = MidRid(media.mid(), None);
		let suppress_nack = repair_ssrc.is_none();
		streams.expect_stream_rx(i.ssrc, repair_ssrc, midrid, suppress_nack);
	}
}

trait AsSdpMediaLine {
	fn mid(&self) -> Mid;
	fn msid(&self) -> Option<&Msid>;
	fn index(&self) -> usize;
	fn kind(&self) -> MediaKind;
	fn as_media_line(
		&self,
		attrs: Vec<MediaAttribute>,
		ssrcs_tx: &[(Ssrc, Option<Ssrc>)],
		exts: &ExtensionMap,
		params: &[PayloadParams],
	) -> MediaLine;
}

impl AsSdpMediaLine for (Mid, usize) {
	fn mid(&self) -> Mid {
		self.0
	}
	fn msid(&self) -> Option<&Msid> {
		None
	}
	fn index(&self) -> usize {
		self.1
	}
	fn kind(&self) -> MediaKind {
		MediaKind::Audio // doesn't matter for App
	}
	fn as_media_line(
		&self,
		mut attrs: Vec<MediaAttribute>,
		_ssrcs_tx: &[(Ssrc, Option<Ssrc>)],
		_exts: &ExtensionMap,
		_params: &[PayloadParams],
	) -> MediaLine {
		attrs.push(MediaAttribute::Mid(self.0));

		// No SCTP runs here, so a remote data channel line is always rejected.
		MediaLine {
			typ: sdp::MediaType::Application,
			disabled: true,
			proto: Proto::Sctp,
			pts: vec![],
			bw: None,
			attrs,
		}
	}
}

impl AsSdpMediaLine for Media {
	fn mid(&self) -> Mid {
		Media::mid(self)
	}
	fn msid(&self) -> Option<&Msid> {
		(!self.disabled()).then(|| Media::msid(self))
	}
	fn index(&self) -> usize {
		Media::index(self)
	}
	fn kind(&self) -> MediaKind {
		Media::kind(self)
	}
	fn as_media_line(
		&self,
		mut attrs: Vec<MediaAttribute>,
		ssrcs_tx: &[(Ssrc, Option<Ssrc>)],
		exts: &ExtensionMap,
		params: &[PayloadParams],
	) -> MediaLine {
		attrs.push(MediaAttribute::Mid(self.mid()));

		let audio = self.kind() == MediaKind::Audio;
		for (id, ext) in self.remote_extmap().iter_by_media_type(audio) {
			attrs.push(MediaAttribute::ExtMap { id, ext: ext.clone() });
		}

		attrs.push(self.direction().into());
		if !self.disabled() {
			attrs.push(MediaAttribute::Msid(self.msid().clone()));
		}
		attrs.push(MediaAttribute::RtcpMux);

		// The effective params start from the Session::codec_config to retain the
		// user's configured preferred order, however they are narrowed only include
		// those the remote peer wants.
		let effective_params = params.iter().filter(|p| self.remote_pts().contains(&p.pt));

		let mut pts = vec![];

		for p in effective_params {
			let p = *p;
			p.as_media_attrs(&mut attrs);

			// The pts that will be advertised in the SDP.
			pts.push(p.pt());
			if let Some(rtx) = p.resend() {
				pts.push(rtx);
			}
		}

		if let Some(s) = self.simulcast() {
			fn to_rids<'a>(
				gs: &'a SimulcastGroups,
				direction: &'static str,
			) -> impl Iterator<Item = MediaAttribute> + 'a {
				gs.iter().map(move |layer| MediaAttribute::Rid {
					id: layer.restriction_id.clone(),
					direction,
					pt: vec![],
					restriction: layer.attributes.clone().unwrap_or_default(),
				})
			}
			attrs.extend(to_rids(&s.recv, "recv"));
			attrs.extend(to_rids(&s.send, "send"));
			attrs.push(MediaAttribute::Simulcast(s.clone()));
		}

		// Outgoing SSRCs
		let ssrcs_tx: &[(Ssrc, Option<Ssrc>)] = if self.disabled() { &[] } else { ssrcs_tx };
		let msid = format!("{} {}", self.msid().stream_id, self.msid().track_id);
		for (ssrc, ssrc_rtx) in ssrcs_tx {
			attrs.push(MediaAttribute::Ssrc {
				ssrc: *ssrc,
				attr: "cname".to_string(),
				value: self.cname().to_string(),
			});
			attrs.push(MediaAttribute::Ssrc {
				ssrc: *ssrc,
				attr: "msid".to_string(),
				value: msid.clone(),
			});
			if let Some(ssrc_rtx) = ssrc_rtx {
				attrs.push(MediaAttribute::Ssrc {
					ssrc: *ssrc_rtx,
					attr: "cname".to_string(),
					value: self.cname().to_string(),
				});
				attrs.push(MediaAttribute::Ssrc {
					ssrc: *ssrc_rtx,
					attr: "msid".to_string(),
					value: msid.clone(),
				});
			}
		}

		for (ssrc, ssrc_rtx) in ssrcs_tx {
			if let Some(ssrc_rtx) = ssrc_rtx {
				attrs.push(MediaAttribute::SsrcGroup {
					semantics: "FID".to_string(),
					ssrcs: vec![*ssrc, *ssrc_rtx],
				});
			}
		}

		MediaLine {
			typ: self.kind().into(),
			disabled: self.disabled(),
			proto: Proto::Srtp,
			pts,
			bw: None,
			attrs,
		}
	}
}

impl From<MediaKind> for MediaType {
	fn from(value: MediaKind) -> Self {
		match value {
			MediaKind::Audio => MediaType::Audio,
			MediaKind::Video => MediaType::Video,
		}
	}
}

struct AsSdpParams<'a, 'b> {
	pub candidates: Vec<Candidate>,
	pub creds: IceCreds,
	pub fingerprint: &'a Fingerprint,
	pub setup: Setup,
	pub pending: Option<&'b Changes>,
}

impl<'a, 'b> AsSdpParams<'a, 'b> {
	pub fn new(rtc: &'a Rtc, pending: Option<&'b Changes>) -> Self {
		let (creds, candidates) =
			if let Some((new_creds, keep_local_candidates)) = pending.and_then(|p| p.ice_restart()) {
				if keep_local_candidates {
					// If we are performing an ICE restart and we are keeping the same
					// candidates we need to use ufrag from the new ICE credentials
					// in our offer.
					let mut new_candidates = rtc.ice.local_candidates().collect::<Vec<_>>();
					for c in &mut new_candidates {
						c.set_ufrag(&new_creds.ufrag);
					}

					(new_creds, new_candidates)
				} else {
					(new_creds, vec![])
				}
			} else {
				(
					rtc.ice.local_credentials().clone(),
					rtc.ice.local_candidates().collect::<Vec<_>>(),
				)
			};

		// RFC 8842 Section 5.2/5.5: Offerers MUST always use a=setup:actpass.
		// Answerers use the negotiated role (active/passive).
		// We distinguish offer vs answer by the presence of `pending` (Some = offer).
		let setup = if pending.is_some() {
			// This is an offer - always actpass per RFC 8842
			Setup::ActPass
		} else {
			// This is an answer - use the negotiated DTLS role
			match rtc.dtls.is_active() {
				Some(true) => Setup::Active,
				Some(false) => Setup::Passive,
				None => Setup::ActPass,
			}
		};

		AsSdpParams {
			candidates,
			creds,
			fingerprint: rtc.dtls.local_fingerprint(),
			setup,
			pending,
		}
	}

	fn media_attributes(&self, include_candidates: bool) -> Vec<MediaAttribute> {
		use MediaAttribute::*;

		let mut v = if include_candidates {
			self.candidates.iter().map(|c| Candidate(c.clone())).collect()
		} else {
			vec![]
		};

		v.push(IceUfrag(self.creds.ufrag.clone()));
		v.push(IcePwd(self.creds.pass.clone()));
		v.push(IceOptions("trickle".into()));
		v.push(Fingerprint(self.fingerprint.clone()));
		v.push(Setup(self.setup));

		v
	}
}

impl fmt::Debug for SdpPendingOffer {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("SdpPendingOffer").finish()
	}
}

impl Changes {
	fn has_mid(&self, mid: Mid) -> bool {
		self.0.iter().any(|c| match c {
			Change::AddMedia(v) => v.mid == mid,
			Change::Direction(_, _) | Change::IceRestart(_, _) => false,
		})
	}

	/// Tests the given lines (from answer) corresponds to changes.
	fn ensure_correct_answer(&self, lines: &[(usize, &MediaLine)]) -> Option<String> {
		if self.count_new_medias() != lines.len() {
			return Some(format!(
				"Differing m-line count in offer vs answer: {} != {}",
				self.count_new_medias(),
				lines.len()
			));
		}

		'next: for (_, l) in lines {
			let mid = l.mid();

			for m in &self.0 {
				use Change::*;
				match m {
					AddMedia(v) if v.mid == mid => {
						if !l.typ.is_media() {
							return Some(format!(
								"Answer m-line for mid ({}) is not of media type: {:?}",
								mid, l.typ
							));
						}
						continue 'next;
					}
					_ => {}
				}
			}

			return Some(format!("Mid in answer is not in offer: {mid}"));
		}

		None
	}

	fn count_new_medias(&self) -> usize {
		self.0.iter().filter(|c| matches!(c, Change::AddMedia(_))).count()
	}

	pub fn as_new_medias<'a, 'b: 'a>(
		&'a self,
		index_start: usize,
		config: &'b CodecConfig,
		exts: &'b ExtensionMap,
	) -> impl Iterator<Item = Media> + 'a {
		// Use a separate counter that only advances for entries that actually produce
		// an m-line (AddMedia).  Non-media entries (Direction, IceRestart)
		// must not consume an index slot, otherwise the resulting m-line
		// indices would be non-contiguous and out of sync with their array positions.
		let mut media_idx = 0usize;
		self.0.iter().filter_map(move |c| {
			let result = c.as_new_media(index_start + media_idx, config, exts);
			if result.is_some() {
				media_idx += 1;
			}
			result
		})
	}

	pub(crate) fn apply_to(&self, lines: &mut [MediaLine]) {
		for change in &self.0 {
			if let Change::Direction(mid, dir) = change {
				if let Some(line) = lines.iter_mut().find(|l| l.mid() == *mid) {
					if let Some(dir_pos) = line.attrs.iter().position(|a| a.is_direction()) {
						line.attrs[dir_pos] = (*dir).into();
					}
				}
			}
		}
	}

	fn ssrcs_for_mid(&self, mid: Mid) -> &[(Ssrc, Option<Ssrc>)] {
		let maybe_add_media = self
			.0
			.iter()
			.filter_map(|c| if let Change::AddMedia(m) = c { Some(m) } else { None })
			.find(|m| m.mid == mid);

		let Some(m) = maybe_add_media else {
			return &[];
		};

		&m.ssrcs
	}
}

impl Change {
	fn as_new_media(&self, index: usize, config: &CodecConfig, exts: &ExtensionMap) -> Option<Media> {
		use Change::*;
		match self {
			AddMedia(v) => {
				// TODO can we avoid all this cloning?
				let mut add = v.clone();
				add.pts = config.all_for_kind(v.kind).map(|p| p.pt()).collect();
				add.exts = exts.cloned_with_type(v.kind.is_audio());
				add.index = index;

				Some(Media::from_add_media(add))
			}
			_ => None,
		}
	}
}

#[cfg(test)]
mod test {
	use std::time::Instant;

	use sdp::RestrictionId;
	use sdp::SimulcastLayer as SdpSimulcastLayer;

	use crate::vendor::str0m::format::Codec;
	use crate::vendor::str0m::media::{Simulcast, SimulcastLayer};
	use crate::vendor::str0m::sdp::RtpMap;

	use super::*;

	fn resolve_pt(m_line: &MediaLine, needle: Pt) -> RtpMap {
		m_line
			.attrs
			.iter()
			.find_map(|attr| match attr {
				MediaAttribute::RtpMap { pt, value } if *pt == needle => Some(*value),
				_ => None,
			})
			.unwrap_or_else(|| panic!("Expected to find RtpMap for {needle}"))
	}

	fn count_lines(lines: &str, what: &str) -> usize {
		lines.lines().filter(|l| l == &what).count()
	}

	fn get_setup_from_media_line(line: &MediaLine) -> Setup {
		line.setup().expect("Expected a=setup attribute in SDP")
	}

	/// RFC 8842 §5.2: an offer MUST use a=setup:actpass regardless of DTLS role.
	#[test]
	fn offer_uses_actpass() {
		crate::vendor::str0m::init_crypto_default();

		let now = Instant::now();
		let mut rtc = Rtc::new(now);

		let mut change = rtc.sdp_api();
		change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
		let (offer, _pending) = change.apply().unwrap();

		let setup = get_setup_from_media_line(&offer.media_lines[0]);
		assert_eq!(
			setup,
			Setup::ActPass,
			"Offer must use a=setup:actpass per RFC 8842 §5.2"
		);
	}

	/// RFC 8842 §5.5: the answerer uses the negotiated DTLS role (active or passive).
	/// When the offerer uses actpass, the answerer picks passive (DTLS server) and the
	/// offerer becomes active (DTLS client). The answerer's SDP must reflect passive.
	#[test]
	fn answer_uses_negotiated_dtls_role() {
		crate::vendor::str0m::init_crypto_default();

		let now = Instant::now();
		let mut offerer = Rtc::new(now);
		let mut answerer = Rtc::new(now);

		// Offerer creates the offer.
		let mut change = offerer.sdp_api();
		change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
		let (offer, pending) = change.apply().unwrap();

		// Verify the offer itself is actpass.
		assert_eq!(
			get_setup_from_media_line(&offer.media_lines[0]),
			Setup::ActPass,
			"Offer must use a=setup:actpass"
		);

		// Answerer accepts and generates the answer.
		let answer = answerer.sdp_api().accept_offer(offer).unwrap();

		// When the offerer sends actpass, the answerer takes the passive DTLS role
		// (see init_dtls: ActPass from remote → local takes Passive).
		// The answerer's SDP must reflect that with a=setup:passive.
		assert_eq!(
			get_setup_from_media_line(&answer.media_lines[0]),
			Setup::Passive,
			"Answerer's SDP must use a=setup:passive when responding to an actpass offer"
		);

		// Complete the exchange on the offerer side.
		offerer.sdp_api().accept_answer(pending, answer).unwrap();

		// Now generate a subsequent offer from the offerer (re-offer).
		// Even after the DTLS role is settled (offerer is now passive), a new offer
		// must still carry a=setup:actpass per RFC 8842 §5.2.
		let mut change = offerer.sdp_api();
		change.add_media(MediaKind::Video, Direction::SendRecv, None, None, None);
		let (reoffer, _) = change.apply().unwrap();

		assert_eq!(
			get_setup_from_media_line(&reoffer.media_lines[0]),
			Setup::ActPass,
			"Re-offer must still use a=setup:actpass even after DTLS role is settled"
		);
	}

	#[test]
	fn test_out_of_order_error() {
		crate::vendor::str0m::init_crypto_default();

		let now = Instant::now();
		let mut rtc1 = Rtc::new(now);
		let mut rtc2 = Rtc::new(now);

		let mut change1 = rtc1.sdp_api();
		change1.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
		let (offer1, pending1) = change1.apply().unwrap();

		let mut change2 = rtc2.sdp_api();
		change2.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
		let (offer2, _) = change2.apply().unwrap();

		// invalidates pending1
		let _ = rtc1.sdp_api().accept_offer(offer2).unwrap();
		let answer2 = rtc2.sdp_api().accept_offer(offer1).unwrap();

		let r = rtc1.sdp_api().accept_answer(pending1, answer2);

		assert!(matches!(r, Err(RtcError::ChangesOutOfOrder)));
	}

	// The indices assigned to Media objects by as_new_medias() must be contiguous starting
	// from index_start.  Only entries that produce m-lines (AddMedia) should consume an
	// index slot – non-media entries (Direction, IceRestart) must not advance the counter.
	//
	// We test this by building a Changes vector directly and inspecting the indices
	// returned by as_new_medias().
	#[test]
	fn as_new_medias_contiguous_indices_with_non_media_changes() {
		crate::vendor::str0m::init_crypto_default();

		let now = Instant::now();
		let rtc = Rtc::new(now);

		let add = |mid: Mid, kind: MediaKind| {
			Change::AddMedia(AddMedia {
				mid,
				cname: "test".into(),
				msid: Msid {
					stream_id: "stream".into(),
					track_id: "track".into(),
				},
				kind,
				dir: Direction::SendOnly,
				ssrcs: vec![],
				simulcast: None,
				pts: vec![],
				exts: ExtensionMap::empty(),
				index: 0, // will be overwritten by as_new_media()
			})
		};

		// Changes = [AddMedia(audio), IceRestart, AddMedia(video)]
		let mut changes = Changes::default();
		changes.0.push(add(rtc.new_mid(), MediaKind::Audio));
		changes.0.push(Change::IceRestart(IceCreds::new(), true));
		changes.0.push(add(rtc.new_mid(), MediaKind::Video));

		let config = CodecConfig::new_with_defaults();
		let exts = ExtensionMap::standard();

		let medias: Vec<Media> = changes.as_new_medias(0, &config, &exts).collect();

		assert_eq!(medias.len(), 2, "Expected 2 media-producing entries");
		assert_eq!(medias[0].index(), 0, "AddMedia(audio) should have index 0");
		// Index 2 would result from counting the non-media IceRestart entry as a slot.
		assert_eq!(
			medias[1].index(),
			1,
			"AddMedia(video) should have contiguous index 1, not 2"
		);
	}

	/// No SCTP runs here, so an offered data channel is answered rejected and left out of
	/// the BUNDLE group, while the media beside it is accepted.
	#[test]
	fn an_offered_data_channel_is_answered_rejected() {
		crate::vendor::str0m::init_crypto_default();

		let now = Instant::now();
		let mut offerer = Rtc::new(now);
		let mut change = offerer.sdp_api();
		let audio = change.add_media(MediaKind::Audio, Direction::SendOnly, None, None, None);
		let (offer, _) = change.apply().unwrap();
		let offer = offer.to_sdp_string().replace(
			&format!("a=group:BUNDLE {audio}\r\n"),
			&format!("a=group:BUNDLE {audio} dc\r\n"),
		) + "m=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n\
			c=IN IP4 0.0.0.0\r\n\
			a=mid:dc\r\n\
			a=sctp-port:5000\r\n";
		let offer = SdpOffer::from_sdp_string(&offer).expect("offer with a data channel");

		let answer = Rtc::new(now).sdp_api().accept_offer(offer).unwrap();

		let app = answer.media_lines.iter().find(|m| m.typ.is_channel()).unwrap();
		assert!(app.disabled, "the data channel was accepted");
		let media = answer.media_lines.iter().find(|m| m.mid() == audio).unwrap();
		assert!(!media.disabled, "the audio was rejected");
		let answer = answer.to_sdp_string();
		assert!(
			answer.contains("m=application 0 UDP/DTLS/SCTP webrtc-datachannel\r\n"),
			"{answer}"
		);
		assert!(answer.contains(&format!("a=group:BUNDLE {audio}\r\n")), "{answer}");
	}

	/// An offer that puts its data channel first still gets the answer's candidates on the
	/// accepted media, since the rejected data channel line is out of the BUNDLE group.
	#[test]
	fn candidates_skip_a_rejected_first_line() {
		crate::vendor::str0m::init_crypto_default();

		let now = Instant::now();
		let mut offerer = Rtc::new(now);
		let mut change = offerer.sdp_api();
		let audio = change.add_media(MediaKind::Audio, Direction::SendOnly, None, None, None);
		let (offer, _) = change.apply().unwrap();
		let offer = offer.to_sdp_string();
		let (session, media) = offer.split_at(offer.find("m=audio").unwrap());
		let session = session.replace(
			&format!("a=group:BUNDLE {audio}\r\n"),
			&format!("a=group:BUNDLE dc {audio}\r\n"),
		);
		let offer = format!(
			"{session}m=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n\
			c=IN IP4 0.0.0.0\r\n\
			a=mid:dc\r\n\
			a=sctp-port:5000\r\n{media}"
		);
		let offer = SdpOffer::from_sdp_string(&offer).expect("offer with a data channel first");

		let mut answerer = Rtc::new(now);
		let candidate = Candidate::host((std::net::Ipv4Addr::new(1, 1, 1, 1), 1000).into(), "udp").unwrap();
		answerer.add_local_candidate(candidate).unwrap();
		let answer = answerer.sdp_api().accept_offer(offer).unwrap();

		let has_candidate = |m: &MediaLine| m.attrs.iter().any(|a| matches!(a, MediaAttribute::Candidate(_)));
		assert!(answer.media_lines[0].disabled, "the data channel was accepted");
		assert!(
			!has_candidate(&answer.media_lines[0]),
			"a candidate sits on the rejected line"
		);
		assert_eq!(answer.media_lines[1].mid(), audio);
		assert!(
			has_candidate(&answer.media_lines[1]),
			"the accepted audio has no candidate"
		);
	}

	#[test]
	fn twcc_negotiation_uses_first_media_section_for_each_pt() {
		crate::vendor::str0m::init_crypto_default();
		for enabled_audio in [1, 2] {
			let expected_audio = enabled_audio == 1;
			let now = Instant::now();
			let mut local = Rtc::builder().build(now);
			let mut remote = Rtc::builder().build(now);
			let mut change = local.sdp_api();
			change.add_media(MediaKind::Audio, Direction::SendOnly, None, None, None);
			change.add_media(MediaKind::Audio, Direction::SendOnly, None, None, None);
			change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None);
			let (offer, pending) = change.apply().unwrap();
			let mut section = 0;
			let sdp = offer
				.to_sdp_string()
				.lines()
				.filter(|line| {
					if line.starts_with("m=") {
						section += 1;
					}
					section == enabled_audio || !line.contains("transport-cc")
				})
				.collect::<Vec<_>>()
				.join("\r\n")
				+ "\r\n";
			let offer = SdpOffer::from_sdp_string(&sdp).unwrap();
			let answer = remote.sdp_api().accept_offer(offer).unwrap();
			// Both audio sections use the first section's negotiated result.
			for m in &answer.media_lines[..2] {
				assert!(m.rtp_params().iter().all(|p| p.fb_transport_cc() == expected_audio));
			}
			assert!(answer.media_lines[2].rtp_params().iter().all(|p| !p.fb_transport_cc()));
			local.sdp_api().accept_answer(pending, answer).unwrap();
			for rtc in [&local, &remote] {
				assert!(
					rtc.codec_config()
						.params()
						.iter()
						.filter(|p| p.spec().codec.is_audio())
						.all(|p| p.fb_transport_cc() == expected_audio)
				);
				assert!(
					rtc.codec_config()
						.params()
						.iter()
						.filter(|p| p.spec().codec.is_video())
						.all(|p| !p.fb_transport_cc())
				);
			}

			// Existing m-lines retain their negotiated codec parameters.
			let sdp =
				sdp.lines()
					.filter(|line| !line.contains("transport-cc"))
					.collect::<Vec<_>>()
					.join("\r\n") + "\r\n";
			let offer = SdpOffer::from_sdp_string(&sdp).unwrap();
			let answer = remote.sdp_api().accept_offer(offer).unwrap();
			for m in &answer.media_lines[..2] {
				assert!(m.rtp_params().iter().all(|p| p.fb_transport_cc() == expected_audio));
			}
		}
	}

	#[test]
	fn test_rtp_payload_priority() {
		crate::vendor::str0m::init_crypto_default();

		let now = Instant::now();
		let mut rtc1 = Rtc::builder()
			.clear_codecs()
			.enable_h264(true)
			.enable_vp8(true)
			.enable_vp9(true)
			.build(now);
		let mut rtc2 = Rtc::builder()
			.clear_codecs()
			.enable_vp8(true)
			.enable_h264(true)
			.build(now);

		let mut change1 = rtc1.sdp_api();
		change1.add_media(MediaKind::Video, Direction::SendOnly, None, None, None);
		let (offer1, _) = change1.apply().unwrap();

		let answer = rtc2.sdp_api().accept_offer(offer1).unwrap();
		assert_eq!(answer.media_lines.len(), 1, "There should be one mline only");

		let first_mline = &answer.media_lines[0];
		let first_pt = resolve_pt(first_mline, first_mline.pts[0]);

		assert_eq!(
			first_pt.codec,
			Codec::Vp8,
			"The first PT returned should be the highest priority PT from the answer that is supported."
		);

		let vp9_unsupported = first_mline
			.pts
			.iter()
			.any(|pt| resolve_pt(first_mline, *pt).codec == Codec::Vp9);

		assert!(
			!vp9_unsupported,
			"VP9 was not offered, so it should not be present in the answer"
		);
	}

	#[test]
	fn non_simulcast_rids() {
		crate::vendor::str0m::init_crypto_default();

		let now = Instant::now();
		let mut rtc1 = Rtc::new(now);
		let mut rtc2 = Rtc::new(now);

		// Test initial media creation
		let mid = {
			let mut changes = rtc1.sdp_api();
			let mid = changes.add_media(MediaKind::Audio, Direction::SendOnly, None, None, None);
			let (offer, pending) = changes.apply().unwrap();
			let answer = rtc2.sdp_api().accept_offer(offer).unwrap();
			rtc1.sdp_api().accept_answer(pending, answer).unwrap();

			assert!(matches!(rtc1.media(mid).unwrap().rids_rx(), Rids::Any));
			assert!(matches!(rtc1.media(mid).unwrap().rids_tx(), Rids::None));
			assert!(matches!(rtc2.media(mid).unwrap().rids_rx(), Rids::Any));
			assert!(matches!(rtc2.media(mid).unwrap().rids_tx(), Rids::None));

			mid
		};

		// Test later updates to that media
		{
			let mut changes = rtc1.sdp_api();
			changes.set_direction(mid, Direction::Inactive);
			let (offer, pending) = changes.apply().unwrap();
			let answer = rtc2.sdp_api().accept_offer(offer).unwrap();
			rtc1.sdp_api().accept_answer(pending, answer).unwrap();

			assert!(matches!(rtc1.media(mid).unwrap().rids_rx(), Rids::Any));
			assert!(matches!(rtc1.media(mid).unwrap().rids_tx(), Rids::None));
			assert!(matches!(rtc2.media(mid).unwrap().rids_rx(), Rids::Any));
			assert!(matches!(rtc2.media(mid).unwrap().rids_tx(), Rids::None));
		}
	}

	#[test]
	fn simulcast_ssrc_allocation() {
		crate::vendor::str0m::init_crypto_default();

		let mut rtc1 = Rtc::new(Instant::now());

		let mut simulcast = Simulcast::new();

		simulcast.add_send_layer(SimulcastLayer::new("h"));
		simulcast.add_send_layer(SimulcastLayer::new("m"));
		simulcast.add_send_layer(SimulcastLayer::new("l"));

		let mut change = rtc1.sdp_api();
		change.add_media(MediaKind::Video, Direction::SendOnly, None, None, Some(simulcast));

		let Change::AddMedia(am) = &change.changes[0] else {
			panic!("Not AddMedia?!");
		};

		// these should be organized in order: m, h, l
		let pending_ssrcs = am.ssrcs.clone();
		assert_eq!(pending_ssrcs.len(), 3);

		for p in &pending_ssrcs {
			assert!(p.1.is_some()); // all should have rtx
		}

		let (offer, _) = change.apply().unwrap();
		let sdp = offer.into_inner();
		let line = &sdp.media_lines[0];

		assert_eq!(
			line.simulcast().unwrap().send,
			SimulcastGroups(vec![
				SdpSimulcastLayer {
					restriction_id: RestrictionId("h".into(), true),
					attributes: None,
				},
				SdpSimulcastLayer {
					restriction_id: RestrictionId("m".into(), true),
					attributes: None,
				},
				SdpSimulcastLayer {
					restriction_id: RestrictionId("l".into(), true),
					attributes: None,
				},
			])
		);

		// Each SSRC, both regular and RTX get their own a=ssrc line.
		assert_eq!(line.ssrc_info().len(), pending_ssrcs.len() * 2);

		let fids: Vec<_> = line
			.attrs
			.iter()
			.filter_map(|a| {
				if let MediaAttribute::SsrcGroup { semantics, ssrcs } = a {
					// We don't have any other semantics right now.
					assert_eq!(semantics, "FID");
					assert_eq!(ssrcs.len(), 2);
					Some((ssrcs[0], ssrcs[1]))
				} else {
					None
				}
			})
			.collect();

		assert_eq!(fids.len(), pending_ssrcs.len());

		for (a, b) in fids.iter().zip(pending_ssrcs.iter()) {
			assert_eq!(a.0, b.0);
			assert_eq!(Some(a.1), b.1);
		}

		let line_string = line.to_string();

		// The SDP offer should contain layers without any attributes
		assert_eq!(count_lines(&line_string, "a=rid:h send"), 1);
		assert_eq!(count_lines(&line_string, "a=rid:m send"), 1);
		assert_eq!(count_lines(&line_string, "a=rid:l send"), 1);
	}

	#[test]
	fn simulcast_attributes() {
		crate::vendor::str0m::init_crypto_default();

		let mut rtc1 = Rtc::new(Instant::now());

		let mut simulcast = Simulcast::new();

		// High layer
		simulcast.add_send_layer(
			SimulcastLayer::new_with_attributes("high")
				.max_width(1280)
				.max_height(720)
				.max_br(1100000)
				.max_br(1300000)
				.max_br(1500000) // the last one wins
				.max_fps(30)
				.build(),
		);

		// Medium layer
		simulcast.add_send_layer(
			SimulcastLayer::new_with_attributes("medium")
				.max_width(640)
				.max_height(360)
				.max_br(600000)
				// No max_fps
				.build(),
		);

		// Low layer
		simulcast.add_send_layer(
			SimulcastLayer::new_with_attributes("low")
				// No max_width
				.max_height(180)
				.max_br(200000)
				.max_fps(15)
				.build(),
		);

		// Custom attribute
		simulcast.add_send_layer(
			SimulcastLayer::new_with_attributes("custom")
				.custom("foo", "bar")
				.build(),
		);

		// No attributes
		simulcast.add_send_layer(SimulcastLayer::new_with_attributes("no_attrs").build());

		let mut change = rtc1.sdp_api();
		change.add_media(MediaKind::Video, Direction::SendOnly, None, None, Some(simulcast));

		let (offer, _) = change.apply().unwrap();
		let sdp = offer.into_inner();
		let line = &sdp.media_lines[0];

		fn pairs_to_some_vec(pairs: &[(&str, &str)]) -> Option<Vec<(String, String)>> {
			Some(pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect())
		}

		assert_eq!(
			line.simulcast().unwrap().send,
			SimulcastGroups(vec![
				SdpSimulcastLayer {
					restriction_id: RestrictionId("high".into(), true),
					attributes: pairs_to_some_vec(&[
						("max-width", "1280"),
						("max-height", "720"),
						("max-br", "1500000"),
						("max-fps", "30"),
					]),
				},
				SdpSimulcastLayer {
					restriction_id: RestrictionId("medium".into(), true),
					attributes: pairs_to_some_vec(
						&[("max-width", "640"), ("max-height", "360"), ("max-br", "600000"),]
					),
				},
				SdpSimulcastLayer {
					restriction_id: RestrictionId("low".into(), true),
					attributes: pairs_to_some_vec(&[("max-height", "180"), ("max-br", "200000"), ("max-fps", "15"),]),
				},
				SdpSimulcastLayer {
					restriction_id: RestrictionId("custom".into(), true),
					attributes: pairs_to_some_vec(&[("foo", "bar"),]),
				},
				SdpSimulcastLayer {
					restriction_id: RestrictionId("no_attrs".into(), true),
					attributes: None,
				},
			])
		);

		// The SDP offer should contain layers with our attributes
		let line_string = line.to_string();
		assert_eq!(
			count_lines(
				&line_string,
				"a=rid:high send max-width=1280;max-height=720;max-br=1500000;max-fps=30"
			),
			1
		);
		assert_eq!(
			count_lines(
				&line_string,
				"a=rid:medium send max-width=640;max-height=360;max-br=600000"
			),
			1
		);
		assert_eq!(
			count_lines(&line_string, "a=rid:low send max-height=180;max-br=200000;max-fps=15"),
			1
		);
		assert_eq!(count_lines(&line_string, "a=rid:custom send foo=bar"), 1);
		// No space at the end
		assert_eq!(count_lines(&line_string, "a=rid:no_attrs send"), 1);
	}
}
