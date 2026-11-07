use std::collections::HashSet;
use std::ops::{Deref, DerefMut};
use tracing::debug;

use crate::vendor::str0m::packet::MediaKind;
use crate::vendor::str0m::rtp_::Pt;
use crate::vendor::str0m::rtp_::{Direction, Frequency};

use super::codec::{Codec, CodecSpec};
use super::format_params::FormatParams;
use super::payload_params::{Claimed, PREFERED_RANGES, PayloadParams};

/// Default payload type for VP8.
pub(crate) const PT_VP8: Pt = Pt::new_with_value(96);

/// Default payload type for VP8 RTX.
pub(crate) const PT_VP8_RTX: Pt = Pt::new_with_value(97);

/// Default payload type for VP9 profile 0.
pub(crate) const PT_VP9: Pt = Pt::new_with_value(98);

/// Default payload type for VP9 profile 0 RTX.
pub(crate) const PT_VP9_RTX: Pt = Pt::new_with_value(99);

/// Default payload type for AV1.
pub(crate) const PT_AV1: Pt = Pt::new_with_value(45);

/// Default payload type for AV1 RTX.
pub(crate) const PT_AV1_RTX: Pt = Pt::new_with_value(46);

/// Default payload type for H265.
pub(crate) const PT_H265: Pt = Pt::new_with_value(102);

/// Default payload type for H265 RTX.
pub(crate) const PT_H265_RTX: Pt = Pt::new_with_value(103);

/// Default payload type for Opus.
pub(crate) const PT_OPUS: Pt = Pt::new_with_value(111);

/// Default payload type for AMR-WB.
pub(crate) const PT_AMR_WB: Pt = Pt::new_with_value(122);

/// Session config for all codecs.
#[derive(Debug, Clone, Default)]
pub struct CodecConfig {
	params: Vec<PayloadParams>,
}

impl CodecConfig {
	/// Creates a new empty config.
	pub fn empty() -> Self {
		CodecConfig::default()
	}

	/// Creates a new config from payload params
	pub fn new_from_payload_params(payload_params: Vec<PayloadParams>) -> Self {
		CodecConfig {
			params: payload_params,
			..Default::default()
		}
	}

	/// Creates a new config with all default configurations enabled.
	pub fn new_with_defaults() -> Self {
		let mut c = Self::empty();
		c.enable_opus(true);

		c.enable_vp8(true);
		c.enable_h264(true);
		c.enable_h265(true);
		c.enable_vp9(true);
		c.enable_av1(true);

		c
	}

	/// Returns a reference to the payload parameters.
	pub fn params(&self) -> &[PayloadParams] {
		&self.params
	}

	/// Clear all configured configs.
	pub fn clear(&mut self) {
		self.params.clear();
	}

	/// Manually configure a payload type.
	pub fn add_config(
		&mut self,
		pt: Pt,
		resend: Option<Pt>,
		codec: Codec,
		clock_rate: Frequency,
		channels: Option<u8>,
		format: FormatParams,
	) {
		let (fb_transport_cc, fb_fir, fb_nack, fb_pli, fb_remb) = if codec.is_video() {
			(true, true, true, true, true)
		} else {
			(true, false, false, false, false)
		};

		let p = PayloadParams {
			pt,
			spec: CodecSpec {
				codec,
				clock_rate,
				channels,
				format,
			},
			resend,
			fb_transport_cc,
			fb_fir,
			fb_nack,
			fb_pli,
			fb_remb,
			locked: false,
		};

		self.params.push(p);
	}

	/// Convenience for adding a h264 payload type.
	pub fn add_h264(&mut self, pt: Pt, resend: Option<Pt>, packetization_mode: bool, profile_level_id: u32) {
		self.add_config(
			pt,
			resend,
			Codec::H264,
			Frequency::NINETY_KHZ,
			None,
			FormatParams {
				level_asymmetry_allowed: Some(true),
				packetization_mode: if packetization_mode { Some(1) } else { Some(0) },
				profile_level_id: Some(profile_level_id),
				..Default::default()
			},
		)
	}

	/// Convenience for adding a h265 payload type.
	pub fn add_h265(&mut self, pt: Pt, resend: Option<Pt>, profile_id: u8, tier_flag: u8, level_id: u8) {
		use crate::vendor::str0m::packet::H265ProfileTierLevel;

		let ptl = H265ProfileTierLevel::new(profile_id, tier_flag, level_id).unwrap_or(H265ProfileTierLevel::FALLBACK);

		self.add_config(
			pt,
			resend,
			Codec::H265,
			Frequency::NINETY_KHZ,
			None,
			FormatParams {
				h265_profile_tier_level: Some(ptl),
				..Default::default()
			},
		)
	}

	/// Add a default AMR-WB payload type.
	pub fn enable_amr_wb(&mut self, enabled: bool) {
		self.params.retain(|c| c.spec.codec != Codec::AmrWb);
		if !enabled {
			return;
		}
		self.add_config(
			PT_AMR_WB,
			None,
			Codec::AmrWb,
			Frequency::SIXTEEN_KHZ,
			Some(1),
			FormatParams {
				octet_align: Some(true),
				..Default::default()
			},
		);
	}

	/// Add a default OPUS payload type.
	pub fn enable_opus(&mut self, enabled: bool) {
		self.params.retain(|c| c.spec.codec != Codec::Opus);
		if !enabled {
			return;
		}
		self.add_config(
			PT_OPUS,
			None,
			Codec::Opus,
			Frequency::FORTY_EIGHT_KHZ,
			Some(2),
			FormatParams {
				min_p_time: Some(10),
				use_inband_fec: Some(true),
				..Default::default()
			},
		);
	}

	/// Add a default VP8 payload type.
	pub fn enable_vp8(&mut self, enabled: bool) {
		self.params.retain(|c| c.spec.codec != Codec::Vp8);
		if !enabled {
			return;
		}
		self.add_config(
			PT_VP8,
			Some(PT_VP8_RTX),
			Codec::Vp8,
			Frequency::NINETY_KHZ,
			None,
			FormatParams::default(),
		)
	}

	/// Add a default H264 payload type.
	pub fn enable_h264(&mut self, enabled: bool) {
		self.params.retain(|c| c.spec.codec != Codec::H264);
		if !enabled {
			return;
		}
		const PARAMS: &[(u8, u8, bool, u32)] = &[
			(127, 121, true, 0x42001f),
			(125, 107, false, 0x42001f),
			(108, 109, true, 0x42e01f),
			(124, 120, false, 0x42e01f),
			(123, 119, true, 0x4d001f),
			(35, 36, false, 0x4d001f),
			(114, 115, true, 0x64001f),
		];

		for p in PARAMS {
			self.add_h264(p.0.into(), Some(p.1.into()), p.2, p.3)
		}
	}

	/// Add a default H265 payload type.
	/// This enables H265 as a video codec (clock rate 90kHz) with default RTX payload type.
	/// Note: H265 is still considered an experimental/hidden codec in parts of the public API,
	/// but it is supported internally for packetization/depacketization.
	pub fn enable_h265(&mut self, enabled: bool) {
		self.params.retain(|c| c.spec.codec != Codec::H265);
		if !enabled {
			return;
		}

		// Start with single default configuration
		// Profile/tier/level negotiation will happen via SDP fmtp parameters
		//
		// Level 6.0 (level_id=180) — a high capability level well supported by modern browsers.
		// Level-id formula per ITU-T H.265 Annex A: level_id = (a*10 + b) * 3
		//   e.g. Level 6.0 → (6*10 + 0) * 3 = 180
		//
		// Chromium H265Level enum definition (kLevel6 = 180):
		//   https://source.chromium.org/chromium/chromium/src/+/main:
		//   third_party/webrtc/api/video_codecs/h265_profile_tier_level.h
		self.add_h265(PT_H265, Some(PT_H265_RTX), 1, 0, 180); // Main, Main tier, Level 6.0
	}

	/// Add a default VP9 payload type.
	pub fn enable_vp9(&mut self, enabled: bool) {
		self.params.retain(|c| c.spec.codec != Codec::Vp9);
		if !enabled {
			return;
		}
		self.add_config(
			PT_VP9,
			Some(PT_VP9_RTX),
			Codec::Vp9,
			Frequency::NINETY_KHZ,
			None,
			FormatParams {
				profile_id: Some(0),
				..Default::default()
			},
		);
		self.add_config(
			100.into(),
			Some(101.into()),
			Codec::Vp9,
			Frequency::NINETY_KHZ,
			None,
			FormatParams {
				profile_id: Some(2),
				..Default::default()
			},
		);
	}

	/// Add a default AV1 payload type.
	pub fn enable_av1(&mut self, enabled: bool) {
		self.params.retain(|c| c.spec.codec != Codec::Av1);
		if !enabled {
			return;
		}
		self.add_config(
			PT_AV1,
			Some(PT_AV1_RTX),
			Codec::Av1,
			Frequency::NINETY_KHZ,
			None,
			FormatParams::default(),
		);
	}

	/// Match the given parameters to the configured parameters.
	///
	/// In a server scenario, a certain codec configuration might not have the same
	/// payload type (PT) for two different peers. We will have incoming data with one
	/// PT and need to match that against the PT of the outgoing.
	///
	/// This call performs matching and if a match is found, returns the _local_ PT
	/// that can be used for sending media.
	pub fn match_params(&self, params: PayloadParams) -> Option<&PayloadParams> {
		let c = self.params.iter().max_by_key(|p| p.match_score(&params))?;
		c.match_score(&params)?; // avoid None, which isn't a match.
		Some(c)
	}

	/// When we get remote payload parameters, we need to match differently depending on direction.
	pub(crate) fn sdp_match_remote(&self, remote_params: PayloadParams, remote_dir: Direction) -> Option<Pt> {
		// If we have no matching parameters locally, we can't accept the remote in any way.
		let our_params = self.match_params(remote_params)?;

		if remote_dir.sdp_is_receiving() {
			// The remote is talking about its own receive requirements. The PTs are not suggestions.
			Some(remote_params.pt())
		} else {
			// We can override the remote with our local config. We would have adjusted to
			// the remote earlier if we can.
			Some(our_params.pt())
		}
	}

	/// Find a payload parameter using a finder function.
	pub fn find(&self, mut f: impl FnMut(&PayloadParams) -> bool) -> Option<&PayloadParams> {
		self.params.iter().find(move |p| f(p))
	}

	/// Return configured payloads for a media kind.
	pub(crate) fn all_for_kind(&self, kind: MediaKind) -> impl Iterator<Item = &PayloadParams> {
		self.params.iter().filter(move |p| {
			if kind == MediaKind::Video {
				p.spec.codec.is_video()
			} else {
				p.spec.codec.is_audio()
			}
		})
	}

	pub(crate) fn update_params(&mut self, remote_params: &[PayloadParams], remote_dir: Direction) {
		// 0-128 of "claimed" PTs. I.e. PTs that we already allocated to something.
		let mut claimed: [bool; 128] = [false; 128];

		// Make a pass with all that are definitely confirmed by the remote, since these can't change.
		for p in self.params.iter() {
			if !p.locked {
				continue;
			}

			claimed.assert_claim_once(p.pt);

			if let Some(rtx) = p.resend {
				claimed.assert_claim_once(rtx);
			}
		}

		// Collect all currently unlocked PTs since we will need to avoid them when reassigning.
		let mut unlocked = self
			.params
			.iter()
			.filter(|p| !p.locked)
			.flat_map(|p| [Some(p.pt()), p.resend()])
			.flatten()
			.collect::<HashSet<_>>();

		// Now lock potential new parameters to remote.
		//
		// If the remote is doing `SendOnly`, we are receiving, so the PTs are suggestions,
		// and we are allowed to ANSWER with our own allocations as overrides.
		// If SendRecv or RecvOnly, the remote is talking about its own receiving capabilities
		// and we are not allowed to change it in the ANSWER.
		let local_is_controlling = !remote_dir.sdp_is_receiving();

		for p in self.params.iter_mut() {
			p.update_param(remote_params, &mut claimed, local_is_controlling, &mut unlocked);
		}

		// Make a pass to reassign unconfirmed payloads that have PT which are now claimed.
		for p in self.params.iter_mut() {
			if p.locked {
				continue;
			}

			if claimed.is_claimed(p.pt) {
				let Some(pt) = claimed.find_unclaimed(PREFERED_RANGES, &unlocked) else {
					// TODO: handle this gracefully.
					panic!("Exhausted all PT ranges, inconsistent PayloadParam state");
				};

				debug!("Reassigned PT {} => {}", p.pt, pt);
				p.pt = pt;

				claimed.assert_claim_once(pt);
				unlocked.remove(&pt);
			}

			let Some(rtx) = p.resend else {
				continue;
			};

			if claimed.is_claimed(rtx) {
				let Some(rtx) = claimed.find_unclaimed(PREFERED_RANGES, &unlocked) else {
					// TODO: handle this gracefully.
					panic!("Exhausted all PT ranges, inconsistent PayloadParam state");
				};

				debug!("Reassigned RTX PT {:?} => {:?}", p.resend, rtx);
				p.resend = Some(rtx);

				claimed.assert_claim_once(rtx);
				unlocked.remove(&rtx);
			}
		}
	}

	pub(crate) fn has_pt(&self, pt: Pt) -> bool {
		self.params.iter().any(|p| p.pt() == pt)
	}
}

impl Deref for CodecConfig {
	type Target = [PayloadParams];

	fn deref(&self) -> &Self::Target {
		&self.params
	}
}

impl DerefMut for CodecConfig {
	fn deref_mut(&mut self) -> &mut Self::Target {
		&mut self.params
	}
}

#[cfg(test)]
mod test {
	use crate::vendor::str0m::io::MultiplexKind;
	use crate::vendor::str0m::packet::MediaKind;
	use crate::vendor::str0m::rtp_::Direction;
	use crate::vendor::str0m::rtp_::Frequency;

	use super::*;
	use crate::vendor::str0m::format::{CodecSpec, FormatParams};

	#[test]
	fn transport_cc_uses_first_section_and_preserves_local_capability() {
		for direction in [Direction::SendOnly, Direction::RecvOnly] {
			for local_enabled in [false, true] {
				for sections in [[false, false], [false, true], [true, false]] {
					let mut config = CodecConfig::empty();
					config.enable_vp8(true);
					config.params[0].set_fb_transport_cc(local_enabled);
					let mut remote = config.params[0];
					let expected = local_enabled && sections[0];
					for enabled in sections {
						remote.set_fb_transport_cc(enabled);
						config.update_params(&[remote], direction);
						assert_eq!(config.params()[0].fb_transport_cc(), expected);
					}
				}
			}
		}
	}

	#[test]
	fn collision_preserves_configured_primary_and_rtx_pts() {
		let mut claimed = PayloadParams::new(
			96.into(),
			Some(97.into()),
			CodecSpec {
				codec: Codec::H264,
				clock_rate: Frequency::NINETY_KHZ,
				channels: None,
				format: FormatParams::default(),
			},
		);
		claimed.locked = true;

		let vp8 = CodecSpec {
			codec: Codec::Vp8,
			clock_rate: Frequency::NINETY_KHZ,
			channels: None,
			format: FormatParams::default(),
		};
		let local_vp8 = PayloadParams::new(110.into(), Some(111.into()), vp8);
		let remote_vp8 = PayloadParams::new(96.into(), Some(97.into()), vp8);

		let mut config = CodecConfig::new_from_payload_params(vec![claimed, local_vp8]);
		config.update_params(&[remote_vp8], Direction::SendOnly);

		let vp8 = config
			.iter()
			.find(|p| p.spec().codec == Codec::Vp8)
			.expect("configured VP8 payload");
		assert_eq!((*vp8.pt(), vp8.resend().map(|pt| *pt)), (110, Some(111)));
	}

	#[test]
	fn enable_amr_wb_uses_a_free_default_pt() {
		let mut config = CodecConfig::new_with_defaults();

		config.enable_amr_wb(true);

		let amr_wb = config
			.params()
			.iter()
			.find(|p| p.spec().codec == Codec::AmrWb)
			.expect("AMR-WB should be enabled");
		assert_eq!(amr_wb.pt(), PT_AMR_WB);

		let mut seen = std::collections::HashSet::new();
		for pt in config
			.params()
			.iter()
			.flat_map(|p| [Some(p.pt()), p.resend()])
			.flatten()
		{
			assert!(seen.insert(pt), "duplicate PT {pt}");
		}
	}

	#[test]
	fn test_pt_conflict_different_directions() {
		// Simulates:
		// 1. str0m OFFER sendonly H264 PT 108, RTX PT 109
		// 2. FF ANSWER recvonly (locks PT 109 as H264 RTX)
		// 3. FF OFFER sendonly Opus PT 109
		// 4. Crash on PT 109 conflict

		let mut config = CodecConfig::empty();

		// Initial local config: H264 with PT 108, RTX 109 and Opus with PT 111
		config.add_config(
			108.into(),
			Some(109.into()),
			Codec::H264,
			Frequency::NINETY_KHZ,
			None,
			FormatParams {
				packetization_mode: Some(1),
				profile_level_id: Some(0x42e01f),
				..Default::default()
			},
		);
		config.add_config(
			111.into(),
			None,
			Codec::Opus,
			Frequency::FORTY_EIGHT_KHZ,
			Some(2),
			FormatParams::default(),
		);

		// Step 2: Remote answers recvonly with our H264 PT 108/109
		// This locks PT 109 as RTX
		let remote_h264_params = vec![PayloadParams::new(
			108.into(),
			Some(109.into()),
			CodecSpec {
				codec: Codec::H264,
				clock_rate: Frequency::NINETY_KHZ,
				channels: None,
				format: FormatParams {
					packetization_mode: Some(1),
					profile_level_id: Some(0x42e01f),
					..Default::default()
				},
			},
		)];

		config.update_params(&remote_h264_params, Direction::RecvOnly);

		// Verify PT 109 is now locked as RTX for H264
		assert!(
			config
				.params()
				.iter()
				.any(|p| p.resend() == Some(109.into()) && p.locked)
		);

		// Step 3: Remote offers sendonly Opus PT 109
		// This should NOT crash - PT 109 should be remapped to 111 (Opus default)
		let remote_opus_params = vec![PayloadParams::new(
			109.into(),
			None,
			CodecSpec {
				codec: Codec::Opus,
				clock_rate: Frequency::FORTY_EIGHT_KHZ,
				channels: Some(2),
				format: FormatParams::default(),
			},
		)];

		config.update_params(&remote_opus_params, Direction::SendOnly);

		// Verify the fix: Opus should be remapped to PT_OPUS (its default)
		let opus_param = config.params().iter().find(|p| p.spec().codec == Codec::Opus);
		assert!(opus_param.is_some(), "Opus param should exist");
		assert_eq!(
			opus_param.unwrap().pt(),
			PT_OPUS,
			"Opus should be remapped to PT_OPUS (default)"
		);
		assert!(opus_param.unwrap().locked, "Opus param should be locked");

		// Verify H264 RTX PT 109 is still locked
		assert!(
			config
				.params()
				.iter()
				.any(|p| p.resend() == Some(109.into()) && p.locked)
		);
	}

	#[test]
	fn test_pt_conflict_same_mline_multiple_codecs() {
		// Reproduces a bug we have had where reassigned PTs were conflicting with unlocked PTs.
		//
		// SCENARIO & ROOT CAUSE:
		// This test reproduces the exact bug from sdp.json using str0m's default config.
		//
		// 1. str0m starts with DEFAULT codec configuration:
		//    - VP8 at PT 96/97 (unlocked)
		//    - H264 High (0x64001f) at PT 114/115 (unlocked)
		//    - Many other H264 profiles at various PTs (127, 125, 108, 124, 123, 35)
		//
		// 2. Chrome (peer A) offers video with H264 only - PTs: 103/104, 109/114, 117/118
		//    Chrome's OFFER direction: sendonly (Chrome sends video to us)
		//    **CRITICAL**: Chrome uses PT 114 as RTX for H264 42e01f (PT 109)
		//
		// 3. str0m ANSWERs accepting Chrome's H264 offer:
		//    - Chrome's H264 PTs (103, 109, 117) are LOCKED
		//    - Chrome's RTX PTs (104, 114, 118) are LOCKED
		//    - VP8 at PT 96 remains UNLOCKED (Chrome didn't negotiate it)
		//    - H264 High at PT 114 CONFLICTS with Chrome's RTX usage of PT 114!
		//
		// 4. THE BUG (before fix): update_params reassigns H264 High from PT 114:
		//    - Searches for available PT starting from 96
		//    - PT 96 appears "unclaimed" because it only checked LOCKED PTs
		//    - VP8 is at PT 96 but UNLOCKED, so wasn't checked!
		//    - H264 High gets reassigned to PT 96
		//    - Now BOTH VP8 and H264 High are at PT 96! ❌
		//
		// 5. Later, Firefox (peer B) joins - str0m creates NEW video m-line (mid KvE)
		//    When generating SDP, str0m collects ALL video codecs from codec_config
		//    Result (before fix): PT 96 appears TWICE in the same m-line!
		//    - Line 729 in sdp.json: a=rtpmap:96 VP8/90000
		//    - Line 791 in sdp.json: a=rtpmap:96 H264/90000 profile-level-id=64001f
		//
		// EXPECTED: Each PT in an m-line must be unique
		// THE FIX: Now checks BOTH locked and unlocked PTs when reassigning

		// Start with str0m's default codec configuration
		// This includes VP8 at 96/97 and H264 High at 114/115 (among others)
		let mut config = CodecConfig::new_with_defaults();

		// Verify H264 High starts at its default PT 114 BEFORE Chrome's offer
		let h264_high_before = config
			.params()
			.iter()
			.find(|p| p.spec().codec == Codec::H264 && p.spec().format.profile_level_id == Some(0x64001f))
			.expect("H264 High should exist");
		assert_eq!(h264_high_before.pt(), 114.into(), "H264 High should start at PT 114");

		// Simulate Chrome's OFFER with H264 only (sendonly from Chrome's perspective)
		// Chrome is sending video to us, so we're receiving
		// Direction::SendOnly means the remote is sending (we receive)
		let chrome_h264_params = vec![
			PayloadParams::new(
				103.into(),
				Some(104.into()),
				CodecSpec {
					codec: Codec::H264,
					clock_rate: Frequency::NINETY_KHZ,
					channels: None,
					format: FormatParams {
						packetization_mode: Some(1),
						profile_level_id: Some(0x42001f),
						..Default::default()
					},
				},
			),
			PayloadParams::new(
				109.into(),
				Some(114.into()),
				CodecSpec {
					codec: Codec::H264,
					clock_rate: Frequency::NINETY_KHZ,
					channels: None,
					format: FormatParams {
						packetization_mode: Some(1),
						profile_level_id: Some(0x42e01f),
						..Default::default()
					},
				},
			),
			PayloadParams::new(
				117.into(),
				Some(118.into()),
				CodecSpec {
					codec: Codec::H264,
					clock_rate: Frequency::NINETY_KHZ,
					channels: None,
					format: FormatParams {
						packetization_mode: Some(1),
						profile_level_id: Some(0x4d001f),
						..Default::default()
					},
				},
			),
		];

		// Process Chrome's offer - this locks the H264 PTs that Chrome offered
		// Chrome is SendOnly, so from our perspective we're receiving (recvonly)
		config.update_params(&chrome_h264_params, Direction::SendOnly);

		// Verify: Chrome's H264 profiles are locked at their specific PTs
		assert!(
			config.params().iter().any(|p| p.pt() == 103.into()
				&& p.spec().codec == Codec::H264
				&& p.spec().format.profile_level_id == Some(0x42001f)
				&& p.locked),
			"H264 42001f should be locked at PT 103"
		);
		assert!(
			config.params().iter().any(|p| p.pt() == 109.into()
				&& p.spec().codec == Codec::H264
				&& p.spec().format.profile_level_id == Some(0x42e01f)
				&& p.locked),
			"H264 42e01f should be locked at PT 109"
		);
		assert!(
			config.params().iter().any(|p| p.pt() == 117.into()
				&& p.spec().codec == Codec::H264
				&& p.spec().format.profile_level_id == Some(0x4d001f)
				&& p.locked),
			"H264 4d001f should be locked at PT 117"
		);

		// Verify: VP8 at PT 96 is NOT locked (Chrome didn't negotiate it)
		let vp8_param = config.params().iter().find(|p| p.spec().codec == Codec::Vp8);
		assert!(vp8_param.is_some(), "VP8 should still exist");
		assert_eq!(vp8_param.unwrap().pt(), 96.into(), "VP8 should be at PT 96");
		assert!(!vp8_param.unwrap().locked, "VP8 should NOT be locked");

		// THE FIX VERIFICATION:
		// H264 64001f was originally at PT 114, but Chrome used PT 114 for RTX
		// update_params should reassign H264 64001f to a PT that avoids BOTH:
		// - Locked PTs (103, 104, 109, 114, 117, 118)
		// - Unlocked PTs already in use (96 for VP8, 97 for VP8 RTX)
		let h264_high_param = config
			.params()
			.iter()
			.find(|p| p.spec().codec == Codec::H264 && p.spec().format.profile_level_id == Some(0x64001f));
		assert!(h264_high_param.is_some(), "H264 64001f should still exist");
		let h264_pt = h264_high_param.unwrap().pt();

		// Should NOT be at PT 96 (VP8 is there)
		assert_ne!(
			h264_pt,
			96.into(),
			"FIX WORKING: H264 64001f should NOT be at PT 96 (VP8 is there)"
		);

		// Should NOT be at any of the locked PTs
		assert!(
			![103, 104, 109, 114, 117, 118].contains(&{ *h264_pt }),
			"H264 64001f should not conflict with locked PTs"
		);

		assert!(
			!h264_high_param.unwrap().locked,
			"H264 64001f should NOT be locked (Chrome didn't offer it)"
		);

		// Now simulate creating a new video m-line (for Firefox's camera)
		// This is where the bug manifests: we collect ALL video codecs from config
		let all_video_params: Vec<_> = config.all_for_kind(MediaKind::Video).collect();

		// Extract just the PTs (including RTX PTs)
		let mut all_pts = Vec::new();
		for p in all_video_params {
			all_pts.push(p.pt());
			if let Some(rtx) = p.resend() {
				all_pts.push(rtx);
			}
		}

		// THE FIX: With the bug fixed, no PT should appear multiple times
		// Check for duplicate PTs - this should now PASS
		let mut seen = std::collections::HashSet::new();
		let mut duplicates = Vec::new();
		for pt in &all_pts {
			if !seen.insert(pt) {
				duplicates.push(*pt);
			}
		}

		assert!(
			duplicates.is_empty(),
			"FAILED: Found duplicate PTs in same m-line: {:?}\nAll PTs: {:?}\n
            This creates invalid SDP with multiple a=rtpmap lines for the same PT!",
			duplicates,
			all_pts
		);
	}

	// ── H.265 direction-based negotiation tests ──────────────────────────

	/// Helper: build a CodecConfig containing only H.265 at the given PTL.
	fn h265_config(profile_id: u8, tier_flag: u8, level_id: u8) -> CodecConfig {
		let mut config = CodecConfig::empty();
		config.add_h265(102.into(), Some(103.into()), profile_id, tier_flag, level_id);
		config
	}

	/// Helper: build a single-element remote PayloadParams for H.265 with full PTL.
	fn h265_remote_ptl(profile_id: u8, tier_flag: u8, level_id: u8) -> Vec<PayloadParams> {
		use crate::vendor::str0m::packet::H265ProfileTierLevel;
		vec![PayloadParams::new(
			102.into(),
			Some(103.into()),
			CodecSpec {
				codec: Codec::H265,
				clock_rate: Frequency::NINETY_KHZ,
				channels: None,
				format: FormatParams {
					h265_profile_tier_level: Some(H265ProfileTierLevel::new(profile_id, tier_flag, level_id).unwrap()),
					..Default::default()
				},
			},
		)]
	}

	/// Helper: build a single-element remote PayloadParams for H.265 with profile-id only.
	fn h265_remote_profile_only(profile_id: u32) -> Vec<PayloadParams> {
		vec![PayloadParams::new(
			102.into(),
			Some(103.into()),
			CodecSpec {
				codec: Codec::H265,
				clock_rate: Frequency::NINETY_KHZ,
				channels: None,
				format: FormatParams {
					profile_id: Some(profile_id),
					..Default::default()
				},
			},
		)]
	}

	/// Helper: build a single-element remote PayloadParams for H.265 with no fmtp.
	fn h265_remote_no_fmtp() -> Vec<PayloadParams> {
		vec![PayloadParams::new(
			102.into(),
			Some(103.into()),
			CodecSpec {
				codec: Codec::H265,
				clock_rate: Frequency::NINETY_KHZ,
				channels: None,
				format: FormatParams::default(),
			},
		)]
	}

	/// Helper: extract the H.265 param from a config (panics if missing).
	fn get_h265(config: &CodecConfig) -> &PayloadParams {
		config
			.params()
			.iter()
			.find(|p| p.spec().codec == Codec::H265)
			.expect("H.265 param missing")
	}

	// ── recvonly: remote sends to us ─────────────────────────────────────

	/// Remote sends at Level 5.2, we can receive at Level 6.0.
	/// Negotiated level should narrow to min(6.0, 5.2) = 5.2.
	#[test]
	fn test_h265_recvonly_remote_level_lower() {
		let mut config = h265_config(1, 0, 180); // Main, Main tier, Level 6.0
		let remote = h265_remote_ptl(1, 0, 156); // Level 5.2

		// Direction::SendOnly = remote is sending (we receive)
		config.update_params(&remote, Direction::SendOnly);

		let ptl = get_h265(&config).spec().format.h265_profile_tier_level.unwrap();
		assert_eq!(ptl.level_id(), 156, "Should narrow to min(180,156) = Level 5.2");
		assert_eq!(ptl.profile_id(), 1, "Profile should be preserved");
		assert_eq!(ptl.tier_flag(), 0, "Tier should be preserved");
	}

	/// Remote sends at Level 6.0, we can receive at Level 5.2.
	/// Negotiated level should narrow to min(5.2, 6.0) = 5.2.
	#[test]
	fn test_h265_recvonly_local_level_lower() {
		let mut config = h265_config(1, 0, 156); // Level 5.2
		let remote = h265_remote_ptl(1, 0, 180); // Level 6.0

		config.update_params(&remote, Direction::SendOnly);

		let ptl = get_h265(&config).spec().format.h265_profile_tier_level.unwrap();
		assert_eq!(ptl.level_id(), 156, "Should narrow to min(156,180) = Level 5.2");
	}

	// ── sendrecv: both sides send and receive ────────────────────────────

	/// Both sides sendrecv. Local Level 6.0, remote Level 5.2.
	/// Effective level should narrow to min(6.0, 5.2) = 5.2.
	#[test]
	fn test_h265_sendrecv_remote_level_lower() {
		let mut config = h265_config(1, 0, 180); // Level 6.0
		let remote = h265_remote_ptl(1, 0, 156); // Level 5.2

		config.update_params(&remote, Direction::SendRecv);

		let ptl = get_h265(&config).spec().format.h265_profile_tier_level.unwrap();
		assert_eq!(ptl.level_id(), 156, "sendrecv: should narrow to Level 5.2");
	}

	/// Both sides sendrecv. Local Level 5.2, remote Level 6.0.
	/// Effective level should narrow to min(5.2, 6.0) = 5.2.
	#[test]
	fn test_h265_sendrecv_local_level_lower() {
		let mut config = h265_config(1, 0, 156); // Level 5.2
		let remote = h265_remote_ptl(1, 0, 180); // Level 6.0

		config.update_params(&remote, Direction::SendRecv);

		let ptl = get_h265(&config).spec().format.h265_profile_tier_level.unwrap();
		assert_eq!(ptl.level_id(), 156, "sendrecv: should narrow to Level 5.2");
	}

	/// Same level on both sides. Should remain unchanged.
	#[test]
	fn test_h265_sendrecv_same_level() {
		let mut config = h265_config(1, 0, 156); // Level 5.2
		let remote = h265_remote_ptl(1, 0, 156); // Level 5.2

		config.update_params(&remote, Direction::SendRecv);

		let ptl = get_h265(&config).spec().format.h265_profile_tier_level.unwrap();
		assert_eq!(ptl.level_id(), 156, "Same levels should stay at Level 5.2");
	}

	// ── sendonly: we send, remote receives ───────────────────────────────

	/// Remote declares recvonly at Level 5.2, we offer sendonly at Level 6.0.
	/// Negotiated level should narrow to 5.2 (receiver wins).
	#[test]
	fn test_h265_sendonly_receiver_level_lower() {
		let mut config = h265_config(1, 0, 180); // Level 6.0
		let remote = h265_remote_ptl(1, 0, 156); // Level 5.2

		// Direction::RecvOnly = remote is receiving (we send)
		config.update_params(&remote, Direction::RecvOnly);

		let ptl = get_h265(&config).spec().format.h265_profile_tier_level.unwrap();
		assert_eq!(ptl.level_id(), 156, "sendonly: should narrow to receiver's Level 5.2");
	}

	/// Remote offers only profile-id=1, no tier/level.
	/// Local should drop its full PTL and echo back profile-id only.
	#[test]
	fn test_h265_profile_only_remote_mirrors() {
		let mut config = h265_config(1, 0, 180); // Full PTL
		let remote = h265_remote_profile_only(1); // profile-id only

		config.update_params(&remote, Direction::SendRecv);

		let h265 = get_h265(&config);
		assert!(
			h265.spec().format.h265_profile_tier_level.is_none(),
			"Full PTL should be cleared when remote only offers profile-id"
		);
		assert_eq!(
			h265.spec().format.profile_id,
			Some(1),
			"profile_id should mirror the remote's value"
		);
	}

	// ── no-fmtp remote ──────────────────────────────────────────────────

	/// Remote has no H.265 fmtp at all. Local should clear both fields.
	#[test]
	fn test_h265_no_fmtp_remote_clears() {
		let mut config = h265_config(1, 0, 180); // Full PTL
		let remote = h265_remote_no_fmtp();

		config.update_params(&remote, Direction::SendRecv);

		let h265 = get_h265(&config);
		assert!(
			h265.spec().format.h265_profile_tier_level.is_none(),
			"PTL should be cleared when remote has no fmtp"
		);
		assert!(
			h265.spec().format.profile_id.is_none(),
			"profile_id should be cleared when remote has no fmtp"
		);
	}

	// ── profile_id field cleanup ─────────────────────────────────────────

	/// When remote has full PTL, the `profile_id` field must be None
	/// to avoid duplicate serialization (profile-id appears in both
	/// h265_profile_tier_level AND the VP9-style profile_id field).
	#[test]
	fn test_h265_profile_id_cleared_on_ptl_match() {
		let mut config = CodecConfig::empty();
		// Artificially set both fields to simulate a misconfigured state
		config.add_config(
			102.into(),
			Some(103.into()),
			Codec::H265,
			Frequency::NINETY_KHZ,
			None,
			FormatParams {
				h265_profile_tier_level: Some(
					crate::vendor::str0m::packet::H265ProfileTierLevel::new(1, 0, 180).unwrap(),
				),
				profile_id: Some(1), // should be cleared after negotiation
				..Default::default()
			},
		);
		let remote = h265_remote_ptl(1, 0, 156);

		config.update_params(&remote, Direction::SendRecv);

		let h265 = get_h265(&config);
		assert!(
			h265.spec().format.profile_id.is_none(),
			"profile_id field must be None when full PTL is present"
		);
		assert!(
			h265.spec().format.h265_profile_tier_level.is_some(),
			"PTL should still be present"
		);
	}

	// ── default level ────────────────────────────────────────────────────

	/// `enable_h265` should produce Level 6.0 (level_id = 180).
	#[test]
	fn test_h265_default_level_is_6_0() {
		let mut config = CodecConfig::empty();
		config.enable_h265(true);

		let h265 = get_h265(&config);
		let ptl = h265.spec().format.h265_profile_tier_level.unwrap();
		assert_eq!(ptl.level_id(), 180, "Default H.265 level should be 6.0 (180)");
		assert_eq!(ptl.profile_id(), 1, "Default H.265 profile should be Main (1)");
		assert_eq!(ptl.tier_flag(), 0, "Default H.265 tier should be Main (0)");
	}

	// H.266 direction-based negotiation tests.

	// recvonly: remote sends to us.

	// sendrecv: both sides send and receive.

	// sendonly: we send, remote receives.

	// no-fmtp remote.

	// profile_id field cleanup.

	// H.266 default level.
}
