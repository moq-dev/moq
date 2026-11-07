#![allow(clippy::single_match)]

use combine::{EasyParser, Parser};
use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt::{self};
use std::ops::Deref;

use crate::crypto::Fingerprint;
use crate::format::Codec;
use crate::format::CodecSpec;
use crate::format::FormatParams;
use crate::format::PayloadParams;
use crate::packet::H265ProfileTierLevel;
use crate::packet::H266ProfileTierLevel;
use crate::rtp_::{Direction, Extension, Frequency, Mid, Pt, Rid, SessionId, Ssrc};
use crate::{Candidate, IceCreds, VERSION};
use str0m_proto::Id;

use super::SdpError;
use super::parser::sdp_parser;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sdp {
    pub session: Session,
    pub media_lines: Vec<MediaLine>,
}

impl Sdp {
    pub(crate) fn parse(input: &str) -> Result<Sdp, SdpError> {
        // `easy_parse` builds (allocates, merges, dedups) an error for every alternative tried,
        // which is most of the cost of parsing a valid SDP. Plain `&str` errors cost nothing; the
        // detailed message is only needed when the input is invalid, so parse again for it then.
        if let Ok((sdp, _)) = sdp_parser().parse(input) {
            return Ok(sdp);
        }
        sdp_parser()
            .easy_parse(input)
            .map(|(sdp, _)| sdp)
            .map_err(|e| SdpError::ParseError(e.to_string()))
    }

    /// Get the MIDs listed in the BUNDLE group, if any.
    pub(crate) fn bundle_mids(&self) -> Option<&[Mid]> {
        self.session.attrs.iter().find_map(|a| {
            if let SessionAttribute::Group { typ, mids } = a {
                if typ == "BUNDLE" {
                    return Some(mids.as_slice());
                }
            }
            None
        })
    }

    pub(crate) fn assert_consistency(&self) -> Result<(), SdpError> {
        match self.do_assert_consistency() {
            None => Ok(()),
            Some(error) => Err(SdpError::Inconsistent(error)),
        }
    }

    pub(crate) fn fingerprint(&self) -> Option<Fingerprint> {
        self.session
            .fingerprint()
            .or_else(|| self.media_lines.iter().find_map(|m| m.fingerprint()))
    }

    pub(crate) fn ice_creds(&self) -> Option<IceCreds> {
        self.session
            .ice_creds()
            .or_else(|| self.media_lines.iter().find_map(|m| m.ice_creds()))
    }

    pub(crate) fn ice_candidates(&self) -> impl Iterator<Item = &Candidate> {
        let mut candidates: HashSet<&Candidate> = HashSet::new();

        // Session level ice candidates.
        candidates.extend(self.session.ice_candidates());

        // Ice candidates.
        for m in &self.media_lines {
            candidates.extend(m.ice_candidates());
        }

        candidates.into_iter()
    }

    /// Get the `a=sctp-init` value from the application m-line, if present.
    ///
    /// Returns the base64-encoded SCTP INIT value.
    pub(crate) fn sctp_init(&self) -> Option<&str> {
        self.media_lines
            .iter()
            .find(|m| m.typ.is_channel())
            .and_then(|m| m.sctp_init())
    }

    pub(crate) fn setup(&self) -> Option<Setup> {
        self.session
            .setup()
            .or_else(|| self.media_lines.iter().find_map(|m| m.setup()))
    }

    fn do_assert_consistency(&self) -> Option<String> {
        // TODO: SDP assertions we need to make:
        // 1. Ensure that every m-line has the same PT configuration for a codec. I.e. if FIR is enabled
        //    for PT 96 in one m-line it must be in all m-lines.
        // 2. Compare with previous m-lines that remote isn't narrowing/expanding PT and/or extmaps.

        for media in &self.media_lines {
            if let Some(error) = media.check_consistent() {
                return Some(error);
            }
        }

        // SDP typically has few m-lines, so a pairwise scan avoids allocation and hashing
        // for a small number of comparisons (at most 45 for 10 m-lines).
        for (i, media) in self.media_lines.iter().enumerate() {
            let mid = media.mid();
            if self.media_lines.iter().skip(i + 1).any(|m| m.mid() == mid) {
                return Some(format!("Mid {mid} is used by more than one m-line"));
            }
        }

        let group = self
            .session
            .attrs
            .iter()
            .find(|a| matches!(a, SessionAttribute::Group { .. }));

        if let Some(SessionAttribute::Group { mids, .. }) = group {
            // For str0m's single BUNDLE transport, expect participating m-lines in
            // media-section order, as libwebrtc generates offers and answers in
            // pc/media_session.cc, omitting rejected sections from the group.
            // This ordering is a deliberate interoperability constraint, stricter
            // than RFC 9143 section 5, which permits BUNDLE MID order to differ
            // from media-section order. Rejecting a section does not remove its
            // m-line from the SDP or change the positions of the remaining m-lines.
            // Port 0 outside BUNDLE means rejected; inside BUNDLE it can be bundle-only.
            let media_lines = self
                .media_lines
                .iter()
                .filter(|m| !m.disabled || mids.contains(&m.mid()));
            // This clones only the iterator state: a slice cursor and a filter closure
            // borrowing `mids`. It does not clone any MediaLine, its attributes, or the
            // backing Vec, and it allocates nothing. Counting consumes the cloned
            // iterator so the original remains available for the order check below.
            let media_count = media_lines.clone().count();
            if mids.len() != media_count {
                return Some(format!(
                    "a=group mid count doesn't match m-line count {} != {}",
                    mids.len(),
                    media_count
                ));
            }
            for (media, mid) in media_lines.zip(mids.iter()) {
                let m = media.mid();
                if m != *mid {
                    return Some(format!("Mid order not matching a=group {m} != {mid}"));
                }
            }
        } else {
            return Some("Session attribute a=group missing".into());
        }

        None
    }
}

/// Session info, before the first m= line
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub id: SessionId,
    pub bw: Option<Bandwidth>,
    pub attrs: Vec<SessionAttribute>,
}

/// Bandwidth from b= line
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bandwidth {
    pub typ: String,
    pub val: String,
}

impl Session {
    pub fn setup(&self) -> Option<Setup> {
        let setup = self.attrs.iter().find_map(|m| {
            if let SessionAttribute::Setup(v) = m {
                Some(v)
            } else {
                None
            }
        })?;

        Some(*setup)
    }

    pub fn ice_creds(&self) -> Option<IceCreds> {
        let ufrag = self.attrs.iter().find_map(|m| {
            if let SessionAttribute::IceUfrag(v) = m {
                Some(v)
            } else {
                None
            }
        })?;

        let pass = self.attrs.iter().find_map(|m| {
            if let SessionAttribute::IcePwd(v) = m {
                Some(v)
            } else {
                None
            }
        })?;

        Some(IceCreds {
            ufrag: ufrag.to_string(),
            pass: pass.to_string(),
        })
    }

    pub fn fingerprint(&self) -> Option<Fingerprint> {
        self.attrs
            .iter()
            .filter_map(|a| match a {
                SessionAttribute::Fingerprint(v) => Some(v),
                _ => None,
            })
            .min_by_key(|v| v.hash_preference())
            .cloned()
    }

    pub fn ice_lite(&self) -> bool {
        self.attrs
            .iter()
            .any(|a| matches!(a, SessionAttribute::IceLite))
    }

    pub fn ice_candidates(&self) -> impl Iterator<Item = &Candidate> {
        self.attrs.iter().filter_map(|a| {
            if let SessionAttribute::Candidate(v) = a {
                Some(v)
            } else {
                None
            }
        })
    }

    pub fn end_of_candidates(&self) -> bool {
        self.attrs
            .iter()
            .any(|a| matches!(a, SessionAttribute::EndOfCandidates))
    }
}

/// Attributes before the first m= line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionAttribute {
    Group {
        typ: String,    // BUNDLE, LS etc
        mids: Vec<Mid>, // 0 1 2 3
    },
    MsidSemantic {
        semantic: String, // WMS
        stream_ids: Vec<String>,
    },
    AllowMixedExts,
    IceLite,
    IceUfrag(String),
    IcePwd(String),
    IceOptions(String),
    Fingerprint(Fingerprint),
    Setup(Setup), // active, passive, actpass, holdconn
    Candidate(Candidate),
    EndOfCandidates,
    Unused(String),
}

fn is_dir(a: &MediaAttribute) -> bool {
    use MediaAttribute::*;
    matches!(a, SendRecv | SendOnly | RecvOnly | Inactive)
}

/// An m-line
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MediaLine {
    pub typ: MediaType,
    pub disabled: bool,
    pub proto: Proto,
    pub pts: Vec<Pt>, // payload types 96 97 125 107 from the m= line
    pub bw: Option<Bandwidth>,
    pub attrs: Vec<MediaAttribute>,
}

impl MediaLine {
    pub fn mid(&self) -> Mid {
        self.attrs
            .iter()
            .find_map(|a| {
                if let MediaAttribute::Mid(m) = a {
                    Some(*m)
                } else {
                    None
                }
            })
            // We should only use `mid()` once we're certain there is
            // a mid line. This is checked by `check_consistent`.
            .expect("missing a=mid")
    }

    /// Get the sctp-init attribute value (base64-encoded string).
    ///
    /// Returns `None` if no `a=sctp-init` attribute is present.
    pub fn sctp_init(&self) -> Option<&str> {
        self.attrs.iter().find_map(|a| {
            if let MediaAttribute::SctpInit(v) = a {
                Some(v.as_str())
            } else {
                None
            }
        })
    }

    pub fn msid(&self) -> Option<Msid> {
        self.attrs.iter().find_map(|a| {
            if let MediaAttribute::Msid(m) = a {
                Some(m.clone())
            } else {
                None
            }
        })
    }

    pub fn direction(&self) -> Direction {
        for a in &self.attrs {
            match a {
                MediaAttribute::SendRecv => return Direction::SendRecv,
                MediaAttribute::SendOnly => return Direction::SendOnly,
                MediaAttribute::RecvOnly => return Direction::RecvOnly,
                MediaAttribute::Inactive => return Direction::Inactive,
                _ => {}
            }
        }
        // Should we error here?
        Direction::Inactive
    }

    pub fn set_direction(&mut self, dir: Direction) {
        let idx = self
            .attrs
            .iter()
            .position(is_dir)
            .expect("m-line must have direction");
        self.attrs[idx] = dir.into();
    }

    pub fn rtp_params(&self) -> Vec<PayloadParams> {
        let mut rtp_maps: Vec<_> = self
            .attrs
            .iter()
            .filter_map(|a| {
                if let MediaAttribute::RtpMap { pt, value: c } = a {
                    Some((*pt, *c))
                } else {
                    None
                }
            })
            .collect();

        for pt in &self.pts {
            if !rtp_maps.iter().any(|(mapped, _)| mapped == pt) {
                if let Some(spec) = CodecSpec::from_static_pt(*pt) {
                    rtp_maps.push((*pt, spec.into()));
                }
            }
        }

        let fmtps: Vec<_> = self
            .attrs
            .iter()
            .filter_map(|a| {
                if let MediaAttribute::Fmtp { pt, values } = a {
                    Some((pt, values))
                } else {
                    None
                }
            })
            .collect();

        let fbs: Vec<_> = self
            .attrs
            .iter()
            .filter_map(|a| {
                if let MediaAttribute::RtcpFb { pt, value } = a {
                    Some((pt, value))
                } else {
                    None
                }
            })
            .collect();

        let mut params: Vec<_> = rtp_maps
            .iter()
            .filter(|(pt, c)| {
                c.codec.is_audio()
                    || c.codec.is_video()
                    || (c.codec.is_tele() && self.pts.contains(pt))
            })
            .filter_map(|(pt, c)| {
                let mut p = PayloadParams::new(*pt, None, (*c).into());
                p.spec.format = FormatParams::from_sdp_fmtp(
                    c.codec,
                    fmtps
                        .iter()
                        .filter(|(fmtp_pt, _)| **fmtp_pt == *pt)
                        .map(|(_, values)| values.as_slice()),
                )?;
                // Remote TWCC feedback is supported only when explicitly advertised.
                p.set_fb_transport_cc(false);
                Some(p)
            })
            .collect();

        for p in &mut params {
            for (pt, values) in fmtps.iter() {
                // find resend pt, if there is one.
                for fp in values.iter() {
                    if let FormatParam::Apt(v) = fp {
                        if *v == p.pt {
                            // ensure this is a rtx
                            let is_rtx = rtp_maps
                                .iter()
                                .any(|(cpt, c)| cpt == *pt && c.codec == Codec::Rtx);
                            if is_rtx {
                                p.resend = Some(**pt);
                            }
                        }
                    }
                }
            }

            for (pt, values) in fmtps.iter() {
                // A slash-separated PT list is RED only when its rtpmap says RED.
                let is_red = rtp_maps
                    .iter()
                    .any(|(cpt, c)| cpt == *pt && c.codec == Codec::Red);
                if !is_red || !p.spec.codec.is_audio() {
                    continue;
                }
                for fp in values.iter() {
                    if let FormatParam::BarePtList(primary) | FormatParam::Red(primary) = fp {
                        if *primary == p.pt {
                            p.red = Some(**pt);
                        }
                    }
                }
            }

            // rtcp feedback mechanisms
            for (pt, value) in fbs.iter() {
                if **pt == p.pt {
                    match &value[..] {
                        "goog-remb" => {
                            p.fb_remb = true;
                        }
                        "transport-cc" => {
                            p.fb_transport_cc = true;
                        }
                        "ccm fir" => {
                            p.fb_fir = true;
                        }
                        "nack" => {
                            p.fb_nack = true;
                        }
                        "nack pli" => {
                            p.fb_pli = true;
                        }
                        _ => {
                            //
                        }
                    }
                }
            }
        }

        params
    }

    pub fn check_consistent(&self) -> Option<String> {
        use MediaAttribute::*;

        let mid_count = self.attrs.iter().filter(|a| matches!(a, Mid(_))).count();

        if mid_count == 0 {
            return Some(format!(
                "Media is missing a=mid: {} {}",
                self.typ, self.proto
            ));
        }

        if mid_count > 1 {
            return Some(format!(
                "Media has more than one a=mid: {} {}",
                self.typ, self.proto
            ));
        }

        let setup = self.attrs.iter().filter(|a| matches!(a, Setup(_))).count();

        if setup > 1 {
            return Some(format!(
                "Expected 0 or 1 a=setup: line for mid: {}",
                self.mid()
            ));
        }

        let dir_count = self.attrs.iter().filter(|a| is_dir(a)).count();

        if self.proto == Proto::Srtp && dir_count != 1 {
            return Some(format!(
                "Expected exactly one of a=sendrecv, a=sendonly, a=recvonly, a=inactive for mid: {}",
                self.mid()
            ));
        }

        if self.proto == Proto::Srtp && self.pts.is_empty() && !self.disabled {
            return Some(format!("Expected at least one PT for mid: {}", self.mid()));
        }

        for m in &self.pts {
            let rtp_count = self
                .attrs
                .iter()
                .filter(|a| {
                    if let MediaAttribute::RtpMap { pt, value: _ } = a {
                        pt == m
                    } else {
                        false
                    }
                })
                .count();
            if rtp_count == 0 && CodecSpec::from_static_pt(*m).is_none() {
                return Some(format!("Missing a=rtp_map:{} for mid: {}", m, self.mid()));
            }
            if rtp_count > 1 {
                return Some(format!(
                    "More than one a=rtp_map:{} for mid: {}",
                    m,
                    self.mid()
                ));
            }
        }

        // An RTX PT must identify only one primary PT, including across multiple fmtp lines.
        let mut rtx_apts = HashMap::new();
        for attr in &self.attrs {
            let Fmtp { pt, values } = attr else {
                continue;
            };
            for value in values {
                let FormatParam::Apt(apt) = value else {
                    continue;
                };
                if let Some(previous) = rtx_apts.insert(*pt, *apt) {
                    if previous != *apt {
                        return Some(format!(
                            "PT {pt} has conflicting apt values {previous} and {apt} for mid: {}",
                            self.mid()
                        ));
                    }
                }
            }
        }
        None
    }

    pub fn setup(&self) -> Option<Setup> {
        let setup = self.attrs.iter().find_map(|m| {
            if let MediaAttribute::Setup(v) = m {
                Some(v)
            } else {
                None
            }
        })?;

        Some(*setup)
    }

    pub fn ice_creds(&self) -> Option<IceCreds> {
        let ufrag = self.attrs.iter().find_map(|m| {
            if let MediaAttribute::IceUfrag(v) = m {
                Some(v)
            } else {
                None
            }
        })?;

        let pass = self.attrs.iter().find_map(|m| {
            if let MediaAttribute::IcePwd(v) = m {
                Some(v)
            } else {
                None
            }
        })?;

        Some(IceCreds {
            ufrag: ufrag.to_string(),
            pass: pass.to_string(),
        })
    }
    pub fn fingerprint(&self) -> Option<Fingerprint> {
        self.attrs
            .iter()
            .filter_map(|a| match a {
                MediaAttribute::Fingerprint(v) => Some(v),
                _ => None,
            })
            .min_by_key(|v| v.hash_preference())
            .cloned()
    }

    /// This hoovers the ice candidates from all m-lines, lots of dupes.
    /// For WebRTC we don't expect different ice states per media line.
    pub fn ice_candidates(&self) -> impl Iterator<Item = &Candidate> {
        self.attrs.iter().filter_map(|a| {
            if let MediaAttribute::Candidate(v) = a {
                Some(v)
            } else {
                None
            }
        })
    }

    /// Any end-of-candidate in any m-line.
    /// For WebRTC we don't expect different ice states per media line.
    pub fn end_of_candidates(&self) -> bool {
        self.attrs
            .iter()
            .any(|a| matches!(a, MediaAttribute::EndOfCandidates))
    }

    pub fn extmaps(&self) -> Vec<(u8, &Extension)> {
        let mut ret = vec![];

        for a in &self.attrs {
            if let MediaAttribute::ExtMap { id, ext } = a {
                ret.push((*id, ext));
            }
        }

        ret
    }

    pub fn rids(&self) -> Vec<Rid> {
        let mut ret = vec![];
        for a in &self.attrs {
            if let MediaAttribute::Rid { id, .. } = a {
                ret.push(id.0.as_str().into())
            }
        }
        ret
    }

    pub fn simulcast(&self) -> Option<Simulcast> {
        let mut found = None;

        for a in &self.attrs {
            if let MediaAttribute::Simulcast(s) = a {
                found = Some(s.clone());
            }
            if let MediaAttribute::Rid {
                pt, restriction, ..
            } = a
            {
                if !pt.is_empty() {
                    warn!("Not currently supporting PT via a=rid");
                }
                if !restriction.is_empty() {
                    warn!("Not currently supporting restrictions via a=rid");
                }
            }
        }

        if found.is_some() {
            return found;
        }

        // Here we could handle munged SDPs and we used to, but we have dropped support for this.

        None
    }

    pub fn ssrc_info(&self) -> Vec<SsrcInfo> {
        let mut v = vec![];

        fn by_ssrc(v: &mut Vec<SsrcInfo>, ssrc: Ssrc) -> &mut SsrcInfo {
            if let Some(pos) = v.iter().position(|i| i.ssrc == ssrc) {
                &mut v[pos]
            } else {
                v.push(SsrcInfo {
                    ssrc,
                    ..Default::default()
                });
                v.last_mut().unwrap()
            }
        }

        for a in &self.attrs {
            match a {
                MediaAttribute::Ssrc { ssrc, attr, value } => {
                    let info = by_ssrc(&mut v, *ssrc);

                    // a=ssrc:2147603131 cname:TbS1Ajv9obq6/63I
                    // a=ssrc:2147603131 msid:- 7a08dda6-518f-4027-b707-410a6d414176
                    match attr.to_lowercase().as_str() {
                        "cname" => info.cname = Some(value.clone()),
                        "msid" => {
                            let mut iter = value.split(' ');

                            fn trim_and_no_minus(s: &str) -> Option<String> {
                                if is_no_stream(s) {
                                    None
                                } else {
                                    Some(s.trim().into())
                                }
                            }

                            if let Some(stream_id) = iter.next() {
                                info.stream_id = trim_and_no_minus(stream_id);
                            }
                            if let Some(track_id) = iter.next() {
                                info.track_id = trim_and_no_minus(track_id);
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        // Match this second to ensure we preserve order of a=ssrc.
        for a in &self.attrs {
            match a {
                MediaAttribute::SsrcGroup { semantics, ssrcs } => {
                    if semantics.to_lowercase() != "fid" {
                        continue;
                    }

                    // a=ssrc-group:FID 659652645 98148385
                    // Should be two SSRC after FID.
                    if ssrcs.len() != 2 {
                        continue;
                    }

                    let info = by_ssrc(&mut v, ssrcs[1]);
                    info.repairs = Some(ssrcs[0]);
                }
                _ => {}
            }
        }

        v
    }

    /// Get the max-message-size attribute value, if present
    pub fn max_message_size(&self) -> Option<usize> {
        self.attrs.iter().find_map(|a| {
            if let MediaAttribute::MaxMessageSize(size) = a {
                Some(*size)
            } else {
                None
            }
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SsrcInfo {
    pub ssrc: Ssrc,
    /// the other ssrc this ssrc is repairing
    pub repairs: Option<Ssrc>,
    pub cname: Option<String>,
    pub stream_id: Option<String>,
    pub track_id: Option<String>,
}

impl Default for SsrcInfo {
    fn default() -> Self {
        Self {
            ssrc: 0.into(),
            repairs: None,
            cname: None,
            stream_id: None,
            track_id: None,
        }
    }
}

impl From<Direction> for MediaAttribute {
    fn from(v: Direction) -> Self {
        match v {
            Direction::SendOnly => MediaAttribute::SendOnly,
            Direction::RecvOnly => MediaAttribute::RecvOnly,
            Direction::SendRecv => MediaAttribute::SendRecv,
            Direction::Inactive => MediaAttribute::Inactive,
        }
    }
}

/// Identifier of an `a=rid` restriction.
///
/// Defined in https://tools.ietf.org/html/draft-ietf-avtext-rid-09
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestrictionId(pub String, pub bool);

impl RestrictionId {
    pub fn new(rid: String, active: bool) -> Self {
        RestrictionId(rid, active)
    }

    pub fn new_active(rid: String) -> Self {
        RestrictionId(rid, true)
    }

    pub fn to_sdp(&self) -> String {
        format!("{}{}", if self.1 { "" } else { "~" }, self.0)
    }
}

impl Default for RestrictionId {
    fn default() -> Self {
        RestrictionId("".to_string(), true)
    }
}

/// "audio", "video", "application"
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum MediaType {
    #[default]
    Audio,
    Video,
    Application,
    // If a parsed SDP has a value we don't recognize, we stick it
    // in here. We could consider making that a parse exception instead.
    #[doc(hidden)]
    Unknown(String),
}

impl MediaType {
    pub fn is_media(&self) -> bool {
        matches!(self, MediaType::Audio | MediaType::Video)
    }

    pub fn is_channel(&self) -> bool {
        matches!(self, MediaType::Application)
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Proto {
    #[default]
    Srtp,
    Sctp,
}

impl Proto {
    pub fn proto_line(&self) -> &str {
        match self {
            Proto::Srtp => "UDP/TLS/RTP/SAVPF",
            Proto::Sctp => "UDP/DTLS/SCTP",
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Setup {
    #[default]
    ActPass,
    Active,
    Passive,
}

impl fmt::Display for Setup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Setup::ActPass => "actpass",
                Setup::Active => "active",
                Setup::Passive => "passive",
            }
        )
    }
}

impl Setup {
    pub fn setup_line(&self) -> &str {
        match self {
            Setup::ActPass => "actpass",
            Setup::Active => "active",
            Setup::Passive => "passive",
        }
    }

    pub fn compare_to_remote(&self, remote: Setup) -> Option<Setup> {
        use Setup::*;
        match (self, remote) {
            (ActPass, ActPass) => None,
            (ActPass, Active) => Some(Passive),
            (ActPass, Passive) => Some(Active),
            (Active, ActPass) => Some(Active),
            (Active, Active) => None,
            (Active, Passive) => Some(Active),
            (Passive, ActPass) => Some(Passive),
            (Passive, Active) => Some(Passive),
            (Passive, Passive) => None,
        }
    }

    pub fn invert(&self) -> Setup {
        match self {
            Setup::ActPass => Setup::ActPass,
            Setup::Active => Setup::Passive,
            Setup::Passive => Setup::Active,
        }
    }
}

/// Attributes before the first m= line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaAttribute {
    // The "a=rtcp" line MUST NOT be added if the most recent answer included an "a=rtcp-mux" line.
    Rtcp(String),
    IceUfrag(String),
    IcePwd(String),
    IceOptions(String),
    Fingerprint(Fingerprint),
    Setup(Setup), // active, passive, actpass, holdconn
    Mid(Mid),     // 0, 1, 2
    SctpPort(u16),
    MaxMessageSize(usize),
    /// a=sctp-init:<base64-encoded SCTP INIT chunk>
    /// See draft-hancke-tsvwg-snap
    SctpInit(String),
    // a=extmap:1 urn:ietf:params:rtp-hdrext:ssrc-audio-level
    // a=extmap:2 http://www.webrtc.org/experiments/rtp-hdrext/abs-send-time
    ExtMap {
        id: u8, // 1-14 inclusive,
        ext: Extension,
    },
    RecvOnly, // a=recvonly
    SendRecv, // a=sendrecv
    SendOnly, // a=sendonly
    Inactive, // a=inactive
    // a=msid:5UUdwiuY7OML2EkQtF38pJtNP5v7In1LhjEK f78dde68-7055-4e20-bb37-433803dd1ed1
    // a=msid:- 78dde68-7055-4e20-bb37-433803dd1ed1
    Msid(Msid),
    RtcpMux,     //
    RtcpMuxOnly, // only in offer, answer with a=rtcp-mux
    // reduced size rtcp. remove this if not supported.
    RtcpRsize,
    Candidate(Candidate),
    EndOfCandidates,
    RtpMap {
        pt: Pt,
        value: RtpMap,
    },
    // rtcp-fb RTCP feedback parameters, repeated
    RtcpFb {
        pt: Pt,        // 111
        value: String, // nack, nack pli, ccm fir...
    },
    // format parameters, seems to be one of these
    Fmtp {
        pt: Pt,                   // 111
        values: Vec<FormatParam>, // minptime=10;useinbandfec=1
    },
    // a=rid:<rid-id> <direction> [pt=<fmt-list>;]<restriction>=<value>
    // a=rid:hi send pt=111,112;max-br=64000;max-height=360
    // https://tools.ietf.org/html/draft-ietf-mmusic-rid-15
    Rid {
        id: RestrictionId,       //
        direction: &'static str, // send or recv
        // No pt means the rid applies to all
        pt: Vec<Pt>, // 111, 112 (rtpmap no)
        restriction: Vec<(String, String)>,
    },
    // a=rid:hi send
    // a=rid:lo send
    // a=simulcast:send hi;lo
    // https://tools.ietf.org/html/draft-ietf-mmusic-sdp-simulcast-14
    // a=simulcast:<send/recv> <alt A>;<alt B>,<or C> <send/recv> [same]
    Simulcast(Simulcast),
    SsrcGroup {
        semantics: String, // i.e. "FID"
        ssrcs: Vec<Ssrc>,  // <normal stream> <repair stream>
    },
    Ssrc {
        ssrc: Ssrc, // synchronization source id
        attr: String,
        value: String,
    },
    Unused(String),
}

impl MediaAttribute {
    pub fn is_direction(&self) -> bool {
        use MediaAttribute::*;
        matches!(self, RecvOnly | SendRecv | SendOnly | Inactive)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatParam {
    /// The minimum duration of media represented by a packet.
    ///
    /// Default 3. Max 120.
    MinPTime(u8),

    /// Decoder-side preference for stereo audio. Default is mono per
    /// RFC 7587 Section 7.1: <https://datatracker.ietf.org/doc/html/rfc7587#section-7.1>
    Stereo(bool),

    /// Sender-side stereo hint for Opus. This is the symmetric m-line companion
    /// to `stereo` and signals the sender's intention to transmit stereo.
    SpropStereo(bool),

    /// Specifies that the decoder can do Opus in-band FEC
    UseInbandFec(bool),

    /// Specifies that the decoder can do Opus DTX
    UseDtx(bool),

    /// Whether h264 sending media encoded at a different level in the offerer-to-answerer
    /// direction than the level in the answerer-to-offerer direction, is allowed.
    LevelAsymmetryAllowed(bool),

    /// What h264 packetization mode is used.
    ///
    /// * 0 - single nal.
    /// * 1 - STAP-A, FU-A is allowed. Non-interleaved.
    PacketizationMode(u8),

    /// H264 profile level.
    ///
    /// * 42 00 1f - 4200=baseline (B)              1f=level 3.1
    /// * 42 e0 1f - 42e0=constrained baseline (CB) 1f=level 3.1
    /// * 4d 00 1f - 4d00=main (M)                  1f=level 3.1
    /// * 64 00 1f - 6400=high (H)                  1f=level 3.1
    ProfileLevelId(u32),

    /// VP9 profile id
    ProfileId(u32),

    /// AV1 profile
    Profile(u8),

    /// AV1 level-idx
    LevelIdx(u8),

    /// AV1 tier
    Tier(u8),

    /// H.265/HEVC profile, tier, and level.
    H265ProfileTierLevel(crate::packet::H265ProfileTierLevel),

    /// H.266/VVC profile, tier, and level.
    H266ProfileTierLevel(crate::packet::H266ProfileTierLevel),

    /// H.265 sprop-max-don-diff parameter (RFC 7798 §7.1).
    /// When > 0, DONL fields are included in RTP packets to support
    /// out-of-order NAL unit decoding. Valid range: 0–32767.
    SpropMaxDonDiff(u16),

    /// Highest event code in an inclusive `0-X` telephone-event range.
    TelephoneEvents(u8),

    /// Bare `0-X` fmtp syntax, interpreted only with a telephone-event rtpmap.
    BareRange(u8),

    /// Bare slash-separated payload list, interpreted only with a RED rtpmap.
    BarePtList(Pt),

    /// AMR-WB octet-aligned (`1`) or bandwidth-efficient (`0`, default) payloads.
    OctetAlign(bool),

    /// AMR-WB allowed speech modes as a bitmask.
    ModeSet(u16),

    /// An AMR-WB parameter that needs a feature str0m lacks, such as `crc=1`.
    /// Declines the payload type.
    AmrWbUnsupported,

    /// RTX (resend) codecs, which PT it concerns.
    Apt(Pt),

    /// RFC 2198 RED redundancy. Carries the primary PT this RED stream wraps
    /// (the first entry of the `<primary>/<redundant>...` fmtp payload-type list).
    Red(Pt),

    /// Unrecognized fmtp.
    Unknown,
}

fn parse_amr_wb_mode_set(value: &str) -> Option<u16> {
    value.split(',').try_fold(0u16, |mask, mode| {
        let mode: u8 = mode.trim().parse().ok()?;
        (mode <= 8).then(|| mask | (1 << mode))
    })
}

impl FormatParam {
    pub(crate) fn parse_telephone_events(value: &str) -> Self {
        value
            .strip_prefix("0-")
            .filter(|end| end.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|end| end.parse::<u8>().ok())
            .map(Self::TelephoneEvents)
            .unwrap_or(Self::Unknown)
    }

    pub fn parse(k: &str, v: &str) -> Self {
        use FormatParam::*;
        match k.to_ascii_lowercase().as_str() {
            "minptime" => v.parse().map(MinPTime).ok(),
            "useinbandfec" => Some(UseInbandFec(v == "1")),
            "usedtx" => Some(UseDtx(v == "1")),
            "stereo" => Some(Stereo(v == "1")),
            "sprop-stereo" => Some(SpropStereo(v == "1")),
            "level-asymmetry-allowed" => Some(LevelAsymmetryAllowed(v == "1")),
            "packetization-mode" => v.parse().map(PacketizationMode).ok(),
            "profile-level-id" => u32::from_str_radix(v, 16)
                .or_else(|_| v.parse())
                .map(ProfileLevelId)
                .ok(),
            "profile-id" => v.parse().map(ProfileId).ok(),
            "profile" => v.parse().map(Profile).ok(),
            "level-idx" => v.parse().map(LevelIdx).ok(),
            "tier" => v.parse().map(Tier).ok(),
            "sprop-max-don-diff" => v
                .parse::<u16>()
                .ok()
                .filter(|&v| v <= 32767)
                .map(SpropMaxDonDiff),
            "octet-align" => Some(OctetAlign(v == "1")),
            "mode-set" => parse_amr_wb_mode_set(v).map(ModeSet),
            "crc" | "robust-sorting" if v == "1" => Some(AmrWbUnsupported),
            "mode-change-period" if v == "2" => Some(AmrWbUnsupported),
            "interleaving" => Some(AmrWbUnsupported),
            "apt" => v.parse::<u8>().map(|v| Apt(Pt::from(v))).ok(),
            _ => None,
        }
        .unwrap_or_else(|| {
            trace!("Failed to parse FormatParam: {k}={v}");
            Unknown
        })
    }

    /// Parse multiple format parameters from key-value pairs.
    /// Handles the H.265/H.266 special case where three params combine into
    /// one composite ProfileTierLevel.
    pub fn parse_pairs(mut pairs: Vec<(String, String)>) -> Vec<FormatParam> {
        // Parameter names are case-insensitive (RFC 4855 §3).
        for (k, _) in &mut pairs {
            k.make_ascii_lowercase();
        }

        // Check if this looks like H.265/H.266 by presence of tier-flag or level-id.
        // Both codecs use the same three fmtp keys (RFC 7798 / RFC 9328);
        // their level-id value spaces are disjoint (H.265: 30×level, e.g. 93;
        // H.266: 16×major + 3×minor, e.g. 51), which disambiguates them.
        let is_ptl = pairs
            .iter()
            .any(|(k, _)| k == "tier-flag" || k == "level-id");

        if is_ptl {
            // Build composite ProfileTierLevel: H.265 first, else H.266.
            let map: HashMap<String, String> = pairs.into_iter().collect();

            let mut result = vec![];
            if let Some(ptl) = H265ProfileTierLevel::from_fmtp(&map) {
                result.push(FormatParam::H265ProfileTierLevel(ptl));
            } else if let Some(ptl) = H266ProfileTierLevel::from_fmtp(&map) {
                result.push(FormatParam::H266ProfileTierLevel(ptl));
            }

            // Include non-PTL parameters.
            for (k, v) in map.iter() {
                if k != "profile-id" && k != "tier-flag" && k != "level-id" {
                    result.push(FormatParam::parse(k, v));
                }
            }

            result
        } else {
            // Standard parsing for non-PTL codecs.
            pairs
                .into_iter()
                .map(|(k, v)| FormatParam::parse(&k, &v))
                .collect()
        }
    }
}

impl fmt::Display for FormatParam {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use FormatParam::*;
        match self {
            MinPTime(v) => write!(f, "minptime={v}"),
            UseInbandFec(v) => write!(f, "useinbandfec={}", i32::from(*v)),
            UseDtx(v) => write!(f, "usedtx={}", i32::from(*v)),
            Stereo(v) => write!(f, "stereo={}", i32::from(*v)),
            SpropStereo(v) => write!(f, "sprop-stereo={}", i32::from(*v)),
            LevelAsymmetryAllowed(v) => {
                write!(f, "level-asymmetry-allowed={}", i32::from(*v))
            }
            PacketizationMode(v) => write!(f, "packetization-mode={}", *v),
            ProfileLevelId(v) => write!(f, "profile-level-id={:06x}", *v),
            ProfileId(v) => write!(f, "profile-id={}", *v),
            Profile(v) => write!(f, "profile={}", v),
            LevelIdx(v) => write!(f, "level-idx={}", *v),
            Tier(v) => write!(f, "tier={}", v),
            H265ProfileTierLevel(ptl) => {
                write!(
                    f,
                    "profile-id={};tier-flag={};level-id={}",
                    ptl.profile_id(),
                    ptl.tier_flag(),
                    ptl.level_id()
                )
            }
            H266ProfileTierLevel(ptl) => {
                write!(
                    f,
                    "profile-id={};tier-flag={};level-id={}",
                    ptl.profile_id(),
                    ptl.tier_flag(),
                    ptl.level_id()
                )
            }
            SpropMaxDonDiff(v) => write!(f, "sprop-max-don-diff={}", *v),
            TelephoneEvents(v) => write!(f, "0-{v}"),
            BareRange(v) => write!(f, "0-{v}"),
            BarePtList(v) => write!(f, "{v}/{v}"),
            OctetAlign(v) => write!(f, "octet-align={}", i32::from(*v)),
            ModeSet(mask) => {
                write!(f, "mode-set=")?;
                let mut separator = "";
                for mode in (0..=8).filter(|mode| mask & (1 << mode) != 0) {
                    write!(f, "{separator}{mode}")?;
                    separator = ",";
                }
                Ok(())
            }
            AmrWbUnsupported => Ok(()),
            Apt(v) => write!(f, "apt={v}"),
            Red(v) => write!(f, "{v}/{v}"),
            Unknown => Ok(()),
        }
    }
}

impl PayloadParams {
    pub(crate) fn as_media_attrs(&self, attrs: &mut Vec<MediaAttribute>) {
        attrs.push(MediaAttribute::RtpMap {
            pt: self.pt,
            value: self.spec.into(),
        });

        if self.fb_transport_cc {
            attrs.push(MediaAttribute::RtcpFb {
                pt: self.pt,
                value: "transport-cc".into(),
            });
        }
        if self.fb_remb {
            attrs.push(MediaAttribute::RtcpFb {
                pt: self.pt,
                value: "goog-remb".into(),
            });
        }
        if self.fb_fir {
            attrs.push(MediaAttribute::RtcpFb {
                pt: self.pt,
                value: "ccm fir".into(),
            });
        }
        if self.fb_nack {
            attrs.push(MediaAttribute::RtcpFb {
                pt: self.pt,
                value: "nack".into(),
            });
        }
        if self.fb_pli {
            attrs.push(MediaAttribute::RtcpFb {
                pt: self.pt,
                value: "nack pli".into(),
            });
        }

        let fmtps = if self.spec.codec.is_tele() {
            vec![FormatParam::TelephoneEvents(
                self.spec
                    .format
                    .telephone_event_max
                    .unwrap_or(FormatParams::DEFAULT_TELEPHONE_EVENT_MAX),
            )]
        } else {
            self.spec.format.to_format_param()
        };
        if !fmtps.is_empty() {
            attrs.push(MediaAttribute::Fmtp {
                pt: self.pt,
                values: fmtps,
            });
        }

        if let Some(pt) = self.resend {
            attrs.push(MediaAttribute::RtpMap {
                pt,
                value: RtpMap {
                    codec: Codec::Rtx,
                    clock_rate: self.spec.rtp_clock_rate(),
                    channels: None,
                },
            });
            attrs.push(MediaAttribute::Fmtp {
                pt,
                values: vec![FormatParam::Apt(self.pt)],
            });
        }

        if let Some(pt) = self.red {
            attrs.push(MediaAttribute::RtpMap {
                pt,
                value: RtpMap {
                    codec: Codec::Red,
                    clock_rate: self.spec.rtp_clock_rate(),
                    channels: self.spec.channels,
                },
            });
            attrs.push(MediaAttribute::Fmtp {
                pt,
                values: vec![FormatParam::Red(self.pt)],
            });
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RtpMap {
    pub codec: Codec,
    pub clock_rate: Frequency,
    pub channels: Option<u8>,
}

impl From<RtpMap> for CodecSpec {
    fn from(v: RtpMap) -> Self {
        // G722 uses an 8000 Hz RTP clock rate in SDP (RFC 3551 §4.5.2), but str0m
        // treats it as a 16 kHz mono codec everywhere user facing. Normalize on the way in.
        // See https://en.wikipedia.org/wiki/RTP_payload_formats#cite_note-55
        let (clock_rate, channels) = if v.codec == Codec::G722 {
            let channels = if v.channels == Some(1) {
                None
            } else {
                v.channels
            };
            (Frequency::SIXTEEN_KHZ, channels)
        } else {
            (v.clock_rate, v.channels)
        };
        CodecSpec {
            codec: v.codec,
            clock_rate,
            channels,
            format: FormatParams::default(),
        }
    }
}

impl From<CodecSpec> for RtpMap {
    fn from(v: CodecSpec) -> Self {
        RtpMap {
            codec: v.codec,
            // G722 is signalled with an 8000 Hz clock rate in SDP (RFC 3551 §4.5.2),
            // even though it samples at 16 kHz. See
            // https://en.wikipedia.org/wiki/RTP_payload_formats#cite_note-55
            clock_rate: v.rtp_clock_rate(),
            channels: v.channels,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Simulcast {
    pub send: SimulcastGroups,
    pub recv: SimulcastGroups,
    /// If this is created synthetically for a munged SDP.
    pub is_munged: bool,
}

impl Simulcast {
    pub fn invert(self) -> Self {
        Simulcast {
            send: self.recv,
            recv: self.send,
            is_munged: self.is_munged,
        }
    }
}

/// RID organization inside a=simulcast line.
///
/// `a=simulcast send 2;3,4` would result in
/// `SimulcastGroups(vec![SimulcastLayer("2"), SimulcastLayer("3")])`
///
/// The choice between 3 and 4 for the rid is currently ignored and the first option is always
/// selected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimulcastGroups(pub Vec<SimulcastLayer>);

/// Represents a simulcast layer in a group
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimulcastLayer {
    /// The layer's rid
    pub restriction_id: RestrictionId,
    /// Optional attributes per RFC 8851
    pub attributes: Option<Vec<(String, String)>>,
}

impl Deref for SimulcastGroups {
    type Target = [SimulcastLayer];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Msid {
    pub stream_id: String,
    pub track_id: String,
}

/// RFC 8830's "no MediaStream" sentinel: a stream or track id of `-` means the
/// media is not associated with a MediaStream.
pub(crate) fn is_no_stream(s: &str) -> bool {
    s.trim() == "-"
}

impl Msid {
    pub fn random() -> Self {
        Msid {
            stream_id: Id::<30>::random().to_string(),
            track_id: Id::<30>::random().to_string(),
        }
    }
}

impl fmt::Display for SimulcastGroups {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (idx, layer) in self.0.iter().enumerate() {
            if idx + 1 == self.0.len() {
                write!(f, "{}", layer.restriction_id.to_sdp())?;
            } else {
                write!(f, "{};", layer.restriction_id.to_sdp())?;
            }
        }
        Ok(())
    }
}

impl fmt::Display for Sdp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.session)?;
        for m in &self.media_lines {
            write!(f, "{m}")?;
        }
        Ok(())
    }
}

impl fmt::Display for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v=0\r\n")?;
        write!(f, "o=str0m-{} {} 2 IN IP4 0.0.0.0\r\n", VERSION, self.id)?;
        write!(f, "s=-\r\n")?;
        if let Some(bw) = &self.bw {
            write!(f, "b={}:{}\r\n", bw.typ, bw.val)?;
        }
        write!(f, "t=0 0\r\n")?;
        for a in &self.attrs {
            write!(f, "{a}")?;
        }
        Ok(())
    }
}

impl fmt::Display for SessionAttribute {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use SessionAttribute::*;
        match self {
            Group { typ, mids } => {
                write!(f, "a=group:{typ}")?;
                for mid in mids {
                    write!(f, " {mid}")?;
                }
                write!(f, "\r\n")?;
            }
            AllowMixedExts => write!(f, "a=extmap-allow-mixed\r\n")?,
            MsidSemantic {
                semantic,
                stream_ids,
            } => {
                write!(
                    f,
                    "a=msid-semantic: {} {}\r\n",
                    semantic,
                    stream_ids.join(" ")
                )?;
            }
            IceLite => write!(f, "a=ice-lite\r\n")?,
            IceUfrag(v) => write!(f, "a=ice-ufrag:{v}\r\n")?,
            IcePwd(v) => write!(f, "a=ice-pwd:{v}\r\n")?,
            IceOptions(v) => write!(f, "a=ice-options:{v}\r\n")?,
            Fingerprint(v) => {
                write!(
                    f,
                    "a=fingerprint:{} {}\r\n",
                    v.hash_func,
                    FingerprintFmt(&v.bytes)
                )?;
            }
            Setup(v) => write!(f, "a=setup:{}\r\n", v.setup_line())?,
            Candidate(c) => write!(f, "a={}\r\n", c.to_sdp_string())?,
            EndOfCandidates => write!(f, "a=end-of-candidates\r\n")?,
            Unused(v) => write!(f, "a={v}\r\n")?,
        }
        Ok(())
    }
}

impl fmt::Display for MediaLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let port = if self.disabled { 0 } else { 9 };
        write!(f, "m={} {} {} ", self.typ, port, self.proto)?;
        let len = self.pts.len();
        if self.typ.is_channel() {
            write!(f, "webrtc-datachannel\r\n")?;
        } else if self.pts.is_empty() {
            write!(f, "0\r\n")?;
        } else {
            for (idx, m) in self.pts.iter().enumerate() {
                if idx + 1 < len {
                    write!(f, "{m} ")?;
                } else {
                    write!(f, "{m}")?;
                }
            }
            write!(f, "\r\n")?;
        }
        write!(f, "c=IN IP4 0.0.0.0\r\n")?;
        if let Some(bw) = &self.bw {
            write!(f, "b={}:{}\r\n", bw.typ, bw.val)?;
        }
        for a in &self.attrs {
            write!(f, "{a}")?;
        }
        Ok(())
    }
}

impl fmt::Display for MediaType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MediaType::Audio => write!(f, "audio"),
            MediaType::Video => write!(f, "video"),
            MediaType::Application => write!(f, "application"),
            MediaType::Unknown(v) => write!(f, "{v}"),
        }
    }
}

impl fmt::Display for Proto {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.proto_line())
    }
}

impl fmt::Display for MediaAttribute {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use MediaAttribute::*;
        match self {
            Rtcp(v) => write!(f, "a=rtcp:{v}\r\n")?,
            IceUfrag(v) => write!(f, "a=ice-ufrag:{v}\r\n")?,
            IcePwd(v) => write!(f, "a=ice-pwd:{v}\r\n")?,
            IceOptions(v) => write!(f, "a=ice-options:{v}\r\n")?,
            Fingerprint(v) => {
                write!(
                    f,
                    "a=fingerprint:{} {}\r\n",
                    v.hash_func,
                    FingerprintFmt(&v.bytes)
                )?;
            }
            Setup(v) => write!(f, "a=setup:{}\r\n", v.setup_line())?,
            Mid(v) => write!(f, "a=mid:{v}\r\n")?,
            SctpPort(v) => write!(f, "a=sctp-port:{v}\r\n")?,
            MaxMessageSize(v) => write!(f, "a=max-message-size:{v}\r\n")?,
            SctpInit(v) => write!(f, "a=sctp-init:{v}\r\n")?,
            ExtMap { id, ext } => {
                if !ext.is_serialized() {
                    return Ok(());
                }
                write!(f, "a=extmap:{}", id)?;
                // if let Some(d) = &e.direction {
                //     write!(f, "/{d}")?;
                // }
                write!(f, " {}", ext.as_uri())?;
                // if let Some(e) = &e.ext {
                //     write!(f, " {}", e)?;
                // }
                write!(f, "\r\n")?;
            }
            RecvOnly => write!(f, "a=recvonly\r\n")?,
            SendRecv => write!(f, "a=sendrecv\r\n")?,
            SendOnly => write!(f, "a=sendonly\r\n")?,
            Inactive => write!(f, "a=inactive\r\n")?,
            // a=msid:5UUdwiuY7OML2EkQtF38pJtNP5v7In1LhjEK f78dde68-7055-4e20-bb37-433803dd1ed1
            // a=msid:- 78dde68-7055-4e20-bb37-433803dd1ed1
            Msid(v) => write!(f, "a=msid:{} {}\r\n", v.stream_id, v.track_id)?,
            RtcpMux => write!(f, "a=rtcp-mux\r\n")?,
            RtcpMuxOnly => write!(f, "a=rtcp-mux-only\r\n")?,
            RtcpRsize => write!(f, "a=rtcp-rsize\r\n")?,
            Candidate(c) => write!(f, "a={}\r\n", c.to_sdp_string())?,
            EndOfCandidates => write!(f, "a=end-of-candidates\r\n")?,
            RtpMap { pt, value: c } => {
                write!(f, "a=rtpmap:{} {}/{}", pt, c.codec, c.clock_rate)?;
                if let Some(e) = c.channels {
                    write!(f, "/{e}")?;
                }
                write!(f, "\r\n")?;
            }
            RtcpFb { pt, value } => write!(f, "a=rtcp-fb:{pt} {value}\r\n")?,
            Fmtp { pt, values } => {
                if values.is_empty() {
                    // Skip empty fmtp lines - they're invalid SDP
                    return Ok(());
                }
                write!(f, "a=fmtp:{pt} ")?;
                for (idx, v) in values.iter().enumerate() {
                    if idx + 1 < values.len() {
                        write!(f, "{v};")?;
                    } else {
                        write!(f, "{v}\r\n")?;
                    }
                }
            }
            // a=rid:hi send pt=111,112;max-br=64000;max-height=360
            Rid {
                id,
                direction,
                pt,
                restriction,
            } => {
                write!(f, "a=rid:{} {}", id.0, direction)?;
                for (idx, p) in pt.iter().enumerate() {
                    if idx == 0 {
                        write!(f, " pt=")?;
                    }
                    if idx + 1 == pt.len() {
                        write!(f, "{p}")?;
                    } else {
                        write!(f, "{p},")?;
                    }
                }
                for (idx, (k, v)) in restriction.iter().enumerate() {
                    if idx == 0 {
                        if pt.is_empty() {
                            write!(f, " ")?;
                        } else {
                            write!(f, ";")?;
                        }
                    }
                    if idx + 1 == restriction.len() {
                        write!(f, "{k}={v}")?;
                    } else {
                        write!(f, "{k}={v};")?;
                    }
                }
                write!(f, "\r\n")?;
            }
            // a=simulcast:<send/recv> <alt A>;<alt B>,<or C> <send/recv> [same]
            Simulcast(x) => {
                let self::Simulcast {
                    send,
                    recv,
                    is_munged,
                } = x;
                assert!(
                    !(send.0.is_empty() && recv.0.is_empty()),
                    "Empty a=simulcast"
                );
                if *is_munged {
                    // don't write
                    return Ok(());
                }
                write!(f, "a=simulcast:")?;
                if !send.0.is_empty() {
                    write!(f, "send {send}")?;
                }
                if !recv.0.is_empty() {
                    if !send.0.is_empty() {
                        write!(f, " ")?;
                    }
                    write!(f, "recv {recv}")?;
                }
                write!(f, "\r\n")?;
            }
            SsrcGroup { semantics, ssrcs } => {
                write!(f, "a=ssrc-group:{semantics} ")?;
                for (idx, ssrc) in ssrcs.iter().enumerate() {
                    if idx + 1 < ssrcs.len() {
                        write!(f, "{} ", **ssrc)?;
                    } else {
                        write!(f, "{}\r\n", **ssrc)?;
                    }
                }
            }
            Ssrc { ssrc, attr, value } => {
                write!(f, "a=ssrc:{} {}:{}\r\n", **ssrc, attr, value)?;
            }
            Unused(v) => write!(f, "a={v}\r\n")?,
        }
        Ok(())
    }
}

pub struct FingerprintFmt<'a>(pub &'a [u8]);

impl<'a> std::fmt::Display for FingerprintFmt<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let last = self.0.len() - 1;
        for (idx, b) in self.0.iter().enumerate() {
            if idx < last {
                write!(f, "{b:02X}:")?;
            } else {
                write!(f, "{b:02X}")?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use crate::VERSION;
    use crate::packet::H265ProfileTierLevel;
    use crate::rtp_::{Extension, Frequency};

    use super::*;

    /// The captured Chrome offer in `docs/chrome-sdp.json`, as an SDP string.
    fn chrome_offer() -> String {
        let json: serde_json::Value =
            serde_json::from_str(include_str!("../../docs/chrome-sdp.json")).unwrap();
        let lines = json["sdp"].as_array().unwrap();
        let sdp: String = lines
            .iter()
            .map(|l| format!("{}\r\n", l.as_str().unwrap()))
            .collect();
        sdp
    }

    #[test]
    fn fast_and_easy_parse_agree() {
        let input = chrome_offer();
        let fast = Sdp::parse(&input).unwrap();
        let (easy, _) = sdp_parser().easy_parse(input.as_str()).unwrap();
        assert_eq!(fast, easy);
        assert!(fast.media_lines.len() >= 2);
    }

    #[test]
    fn invalid_sdp_keeps_the_detailed_error() {
        let input = chrome_offer().replacen("v=0", "v=x", 1);
        let Err(SdpError::ParseError(msg)) = Sdp::parse(&input) else {
            panic!("a bad version line parses");
        };
        assert!(msg.contains("Parse error at"), "{msg}");
    }

    #[test]
    fn fingerprint_hash_preference_at_session_and_media_level() {
        for (hashes, expected) in [
            (vec!["sha-1", "SHA-256"], "SHA-256"),
            (vec!["sha-256", "SHA-1"], "sha-256"),
            (vec!["sha-512", "SHA-1"], "SHA-1"),
            (vec!["sha-512", "sha-256"], "sha-256"),
            (vec!["sha-512", "md5"], "sha-512"),
        ] {
            for session_level in [false, true] {
                let fingerprints = hashes
                    .iter()
                    .map(|hash| format!("a=fingerprint:{hash} 00:01\r\n"))
                    .collect::<String>();
                let header = "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\n";
                let media = "m=audio 9 UDP/TLS/RTP/SAVPF 0\r\na=mid:0\r\na=sendrecv\r\n";
                let sdp = if session_level {
                    format!("{header}{fingerprints}{media}")
                } else {
                    format!("{header}{media}{fingerprints}")
                };
                let parsed = Sdp::parse(&sdp).unwrap();
                assert_eq!(parsed.fingerprint().unwrap().hash_func, expected);
            }
        }
    }

    #[test]
    fn remote_twcc_feedback_is_explicit_and_per_payload_type() {
        let sdp = Sdp::parse(concat!(
            "v=0\r\n",
            "o=- 1 1 IN IP4 127.0.0.1\r\n",
            "s=-\r\n",
            "t=0 0\r\n",
            "m=video 9 UDP/TLS/RTP/SAVPF 96 98\r\n",
            "a=mid:0\r\n",
            "a=sendrecv\r\n",
            "a=setup:actpass\r\n",
            "a=ice-ufrag:test\r\n",
            "a=ice-pwd:testpassword\r\n",
            "a=rtpmap:96 VP8/90000\r\n",
            "a=rtpmap:98 VP9/90000\r\n",
            "a=rtcp-fb:98 transport-cc\r\n",
        ))
        .unwrap();
        let params = sdp.media_lines[0].rtp_params();
        for (pt, expected) in [(96, false), (98, true)] {
            let p = params.iter().find(|p| p.pt == pt.into()).unwrap();
            assert_eq!(p.fb_transport_cc(), expected);
        }
    }

    #[test]
    fn a_group_is_written_without_a_trailing_space() {
        let empty = SessionAttribute::Group {
            typ: "BUNDLE".into(),
            mids: vec![],
        };
        assert_eq!(empty.to_string(), "a=group:BUNDLE\r\n");

        let two = SessionAttribute::Group {
            typ: "BUNDLE".into(),
            mids: vec!["0".into(), "1".into()],
        };
        assert_eq!(two.to_string(), "a=group:BUNDLE 0 1\r\n");
    }

    fn sdp_with_mids(group: &str, mids: &[&str]) -> Sdp {
        let mut sdp = format!(
            "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\na=group:BUNDLE {group}\r\n"
        );
        for mid in mids {
            sdp.push_str("m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n");
            sdp.push_str(&format!("a=mid:{mid}\r\n"));
            sdp.push_str("a=sendrecv\r\na=rtpmap:111 opus/48000/2\r\n");
        }
        Sdp::parse(&sdp).unwrap()
    }

    #[test]
    fn an_invalid_m_line_after_a_valid_one_is_inconsistent() {
        let mut missing_mid = sdp_with_mids("0 1", &["0", "1"]);
        missing_mid.media_lines[1]
            .attrs
            .retain(|a| !matches!(a, MediaAttribute::Mid(_)));

        let mut two_directions = sdp_with_mids("0 1", &["0", "1"]);
        two_directions.media_lines[1]
            .attrs
            .push(MediaAttribute::RecvOnly);

        for sdp in [missing_mid, two_directions] {
            let expected = sdp.media_lines[1].check_consistent().unwrap();
            let err = sdp.assert_consistency().unwrap_err();
            let SdpError::Inconsistent(message) = err else {
                panic!("expected an inconsistent SDP error, got {err:?}");
            };
            assert_eq!(message, expected);
        }
    }

    #[test]
    fn group_mids_match_participating_m_lines_in_order() {
        for group in ["1 0", "0", "0 1 2", "0 2"] {
            assert!(
                sdp_with_mids(group, &["0", "1"])
                    .assert_consistency()
                    .is_err()
            );
        }
        sdp_with_mids("0 1", &["0", "1"])
            .assert_consistency()
            .unwrap();

        for (group, rejected_index) in [("1", 0), ("0", 1)] {
            let mut rejected = sdp_with_mids(group, &["0", "1"]);
            rejected.media_lines[rejected_index].disabled = true;
            rejected.assert_consistency().unwrap();
        }

        let mut bundle_only = sdp_with_mids("0 1", &["0", "1"]);
        bundle_only.media_lines[1].disabled = true;
        bundle_only.assert_consistency().unwrap();
    }

    #[test]
    fn a_rejected_m_line_without_formats_is_consistent() {
        let mut built = sdp_with_mids("0", &["0", "1"]);
        built.media_lines[1].disabled = true;
        built.media_lines[1].pts.clear();
        built.assert_consistency().unwrap();

        let parsed = Sdp::parse(
            "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\na=group:BUNDLE 0\r\n\
             m=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=mid:0\r\na=sendrecv\r\na=rtpmap:111 opus/48000/2\r\n\
             m=video 0 UDP/TLS/RTP/SAVPF \r\na=mid:1\r\na=inactive\r\n",
        )
        .unwrap();
        assert!(parsed.media_lines[1].disabled);
        assert!(parsed.media_lines[1].pts.is_empty());
        parsed.assert_consistency().unwrap();
    }

    #[test]
    fn duplicate_mids_are_inconsistent() {
        let cases: [(&str, &[&str]); 3] = [
            ("0 0", &["0", "0"]),
            ("0", &["0", "0"]),
            ("1 0 0", &["1", "0", "0"]),
        ];
        for (group, mids) in cases {
            let err = sdp_with_mids(group, mids).assert_consistency().unwrap_err();
            let SdpError::Inconsistent(message) = err else {
                panic!("expected an inconsistent SDP error, got {err:?}");
            };
            assert_eq!(message, "Mid 0 is used by more than one m-line");
        }
    }

    /// Tests for general format parameter serialization and parsing.
    /// These tests verify that format parameters can be correctly converted to/from strings.
    mod format_params {
        use super::*;

        #[test]
        fn fmtp_param_to_string() {
            let f = FormatParams {
                min_p_time: Some(10),
                stereo: Some(true),
                use_inband_fec: Some(true),
                ..Default::default()
            };
            assert_eq!(f.to_string(), "minptime=10;stereo=1;useinbandfec=1");
        }

        #[test]
        fn fmtp_param_names_are_case_insensitive() {
            let pairs = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
                pairs
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect()
            };

            let opus = pairs(&[("MinPTime", "10"), ("UseInbandFEC", "1")]);
            assert_eq!(
                FormatParam::parse_pairs(opus),
                [FormatParam::MinPTime(10), FormatParam::UseInbandFec(true)]
            );

            let h265 = pairs(&[("Profile-Id", "1"), ("TIER-FLAG", "0"), ("Level-Id", "93")]);
            let ptl = H265ProfileTierLevel::new(1, 0, 93).unwrap();
            assert_eq!(
                FormatParam::parse_pairs(h265),
                [FormatParam::H265ProfileTierLevel(ptl)]
            );
        }
    }

    /// Tests for SDP parsing, structure validation, and serialization.
    /// Covers error handling, SDP generation, BUNDLE groups, and media line configuration.
    mod sdp_parsing {
        use super::*;

        #[test]
        fn parse_error() {
            let input = "v=0\r\n\
        o=mozilla...THIS_IS_SDPARTA-99.0 7710052215259647220 2 IN IP4 0.0.0.0\r\n\
        s=-\r\n\
        t=0 0\r\n\
        a=fingerprint:sha-256 A6:64:23:37:94:7E:4B:40:F6:62:86:8C:DD:09:D5:08:\
7E:D4:0E:68:58:93:45:EC:99:F2:91:F7:19:72:E7:BB\r\n\
        a=group:BUNDLE 0 hxI i1X mxk B3D kNI nbB xIZ bKm Hkn\r\n\
        a=ice-options:trickle\r\n\
        a=msid-semantic:WMS *\r\n\
        m=audio 0 UDP/TLS/RTP/SAVPF 0\r\n\
        c=IN IP4 0.0.0.0\r\n\
        a=setup:actpass\r\n\
        a=mid:1\r\n\
        a=rtpmap:0 PCMU/8000\r\n\
        ";

            let sdp = Sdp::parse(input);

            match sdp {
                Err(SdpError::ParseError(out)) => {
                    assert!(out.starts_with(&"Parse error at ".to_string()));
                    assert!(out.contains(
                        &"Expected exactly one of a=sendrecv, a=sendonly, a=recvonly, a=inactive for mid: 1"
                            .to_string()
                    ));
                }
                _ => panic!(),
            }
        }

        #[test]
        fn write_sdp() {
            let sdp = Sdp {
            session: Session {
                id: 5_058_682_828_002_148_772.into(),
                bw: None,
                attrs: vec![
                    SessionAttribute::Group {
                        typ: "BUNDLE".into(),
                        mids: vec!["0".into()],
                    },
                    SessionAttribute::Unused(
                        "msid-semantic: WMS 5UUdwiuY7OML2EkQtF38pJtNP5v7In1LhjEK".into(),
                    ),
                ],
            },
            media_lines: vec![
                MediaLine {
                    typ: MediaType::Audio,
                    disabled: false,
                    proto: Proto::Srtp,
                    pts: vec![111, 103, 104, 9, 0, 8, 106, 105, 13, 110, 112, 113, 126]
                        .into_iter()
                        .map(Pt::from)
                        .collect(),
                    bw: None,
                    attrs: vec![
                        MediaAttribute::Rtcp("9 IN IP4 0.0.0.0".into()),
                        MediaAttribute::IceUfrag("S5hk".into()),
                        MediaAttribute::IcePwd("0zV/Yu3y8aDzbHgqWhnVQhqP".into()),
                        MediaAttribute::IceOptions("trickle".into()),
                        MediaAttribute::Fingerprint(Fingerprint {
                            hash_func: "sha-256".into(),
                            bytes: vec![
                                140, 100, 237, 3, 118, 208, 61, 180, 136, 8, 145, 100, 8, 128,
                                168, 198, 90, 191, 139, 78, 56, 39, 150, 202, 8, 73, 37, 115,
                                70, 96, 32, 220,
                            ],
                        }),
                        MediaAttribute::Setup(Setup::ActPass),
                        MediaAttribute::Mid("0".into()),
                        MediaAttribute::ExtMap{ id: 1, ext: Extension::AudioLevel },
                        MediaAttribute::ExtMap{ id: 2, ext: Extension::AbsoluteSendTime },
                        MediaAttribute::ExtMap{ id: 3, ext: Extension::TransportSequenceNumber },
                        MediaAttribute::ExtMap{ id: 4, ext: Extension::RtpMid },
                        MediaAttribute::ExtMap{ id: 5, ext: Extension::RtpStreamId },
                        MediaAttribute::ExtMap{ id: 6, ext: Extension::RepairedRtpStreamId },
                        MediaAttribute::SendRecv,
                        MediaAttribute::Msid(Msid {
                            stream_id: "5UUdwiuY7OML2EkQtF38pJtNP5v7In1LhjEK".into(),
                            track_id: "f78dde68-7055-4e20-bb37-433803dd1ed1".into(),
                        }),
                        MediaAttribute::RtcpMux,
                        MediaAttribute::RtpMap {
                            pt: 111.into(),
                            value: RtpMap {
                                codec: "opus".into(),
                                clock_rate: Frequency::FORTY_EIGHT_KHZ,
                                channels: Some(2),
                            },
                        },
                        MediaAttribute::RtcpFb { pt: 111.into(), value: "transport-cc".into() },
                        MediaAttribute::Fmtp {
                            pt: 111.into(),
                            values: vec![FormatParam::MinPTime(10), FormatParam::UseInbandFec(true)],
                        },
                        MediaAttribute::Ssrc {
                            ssrc: 3_948_621_874.into(),
                            attr: "cname".into(),
                            value: "xeXs3aE9AOBn00yJ".into(),
                        },
                        MediaAttribute::Ssrc {
                            ssrc: 3_948_621_874.into(),
                            attr: "msid".into(),
                            value: "5UUdwiuY7OML2EkQtF38pJtNP5v7In1LhjEK f78dde68-7055-4e20-bb37-433803dd1ed1"
                                .into(),
                        },
                        MediaAttribute::Ssrc {
                            ssrc: 3_948_621_874.into(),
                            attr: "mslabel".into(),
                            value: "5UUdwiuY7OML2EkQtF38pJtNP5v7In1LhjEK".into(),
                        },
                        MediaAttribute::Ssrc {
                            ssrc: 3_948_621_874.into(),
                            attr: "label".into(),
                            value: "f78dde68-7055-4e20-bb37-433803dd1ed1".into(),
                        },
                    ],
                },
                MediaLine {
                    typ: MediaType::Video,
                    disabled: false,
                    proto: Proto::Srtp,
                    pts: vec![45.into(), 46.into()],
                    bw: None,
                    attrs: vec![
                        MediaAttribute::Rtcp("9 IN IP4 0.0.0.0".into()),
                        MediaAttribute::IceUfrag("S5hk".into()),
                        MediaAttribute::IcePwd("0zV/Yu3y8aDzbHgqWhnVQhqP".into()),
                        MediaAttribute::IceOptions("trickle".into()),
                        MediaAttribute::Fingerprint(Fingerprint {
                            hash_func: "sha-256".into(),
                            bytes: vec![
                                140, 100, 237, 3, 118, 208, 61, 180, 136, 8, 145, 100, 8, 128,
                                168, 198, 90, 191, 139, 78, 56, 39, 150, 202, 8, 73, 37, 115,
                                70, 96, 32, 220,
                            ],
                        }),
                        MediaAttribute::Setup(Setup::ActPass),
                        MediaAttribute::Mid("1".into()),
                        MediaAttribute::ExtMap{ id: 14, ext: Extension::TransmissionTimeOffset },
                        MediaAttribute::ExtMap{ id: 2, ext: Extension::AbsoluteSendTime },
                        MediaAttribute::ExtMap{ id: 13, ext: Extension::VideoOrientation },
                        MediaAttribute::ExtMap{ id: 3, ext: Extension::TransportSequenceNumber },
                        MediaAttribute::ExtMap{ id: 5, ext: Extension::PlayoutDelay },
                        MediaAttribute::ExtMap{ id: 6, ext: Extension::VideoContentType },
                        MediaAttribute::ExtMap{ id: 7, ext: Extension::VideoTiming },
                        MediaAttribute::ExtMap{ id: 8, ext: Extension::ColorSpace },
                        MediaAttribute::ExtMap{ id: 4, ext: Extension::RtpMid },
                        MediaAttribute::ExtMap{ id: 10, ext: Extension::RtpStreamId },
                        MediaAttribute::ExtMap{ id: 11, ext: Extension::RepairedRtpStreamId },
                        MediaAttribute::SendRecv,
                        MediaAttribute::Msid(Msid {
                            stream_id: "-".into(),
                            track_id: "4018fd65-ac50-4861-89a4-1f2cc35bbb5e".into(),
                        }),
                        MediaAttribute::RtcpMux,
                        MediaAttribute::RtcpRsize,
                        MediaAttribute::RtpMap {
                            pt: 45.into(),
                            value: RtpMap {
                                codec: Codec::Av1,
                                clock_rate: Frequency::NINETY_KHZ,
                                channels: None,
                            },
                        },
                        MediaAttribute::RtcpFb { pt: 45.into(), value: "goog-remb".into() },
                        MediaAttribute::RtcpFb { pt: 45.into(), value: "transport-cc".into() },
                        MediaAttribute::RtcpFb { pt: 45.into(), value: "ccm fir".into() },
                        MediaAttribute::RtcpFb { pt: 45.into(), value: "nack".into() },
                        MediaAttribute::RtcpFb { pt: 45.into(), value: "nack pli".into() },
                        MediaAttribute::Fmtp {
                            pt: 45.into(),
                            values: vec![
                                FormatParam::LevelIdx(5),
                                FormatParam::Profile(0),
                                FormatParam::Tier(0),
                            ],
                        },
                        MediaAttribute::RtpMap {
                            pt: 46.into(),
                            value: RtpMap {
                                codec: Codec::Rtx,
                                clock_rate: Frequency::NINETY_KHZ,
                                channels: None,
                            },
                        },
                        MediaAttribute::Fmtp { pt: 46.into(), values: vec![FormatParam::Apt(45.into())] }
                    ],
                }
            ],
        };
            assert_eq!(
                &format!("{sdp}"),
                &format!(
                    "v=0\r\n\
            o=str0m-{VERSION} 5058682828002148772 2 IN IP4 0.0.0.0\r\n\
            s=-\r\n\
            t=0 0\r\n\
            a=group:BUNDLE 0\r\n\
            a=msid-semantic: WMS 5UUdwiuY7OML2EkQtF38pJtNP5v7In1LhjEK\r\n\
            m=audio 9 UDP/TLS/RTP/SAVPF 111 103 104 9 0 8 106 105 13 110 112 113 126\r\n\
            c=IN IP4 0.0.0.0\r\n\
            a=rtcp:9 IN IP4 0.0.0.0\r\n\
            a=ice-ufrag:S5hk\r\n\
            a=ice-pwd:0zV/Yu3y8aDzbHgqWhnVQhqP\r\n\
            a=ice-options:trickle\r\n\
            a=fingerprint:sha-256 8C:64:ED:03:76:D0:3D:B4:88:08:91:64:08:80:A8:C6:\
5A:BF:8B:4E:38:27:96:CA:08:49:25:73:46:60:20:DC\r\n\
            a=setup:actpass\r\n\
            a=mid:0\r\n\
            a=extmap:1 urn:ietf:params:rtp-hdrext:ssrc-audio-level\r\n\
            a=extmap:2 http://www.webrtc.org/experiments/rtp-hdrext/abs-send-time\r\n\
            a=extmap:3 http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01\r\n\
            a=extmap:4 urn:ietf:params:rtp-hdrext:sdes:mid\r\n\
            a=extmap:5 urn:ietf:params:rtp-hdrext:sdes:rtp-stream-id\r\n\
            a=extmap:6 urn:ietf:params:rtp-hdrext:sdes:repaired-rtp-stream-id\r\n\
            a=sendrecv\r\n\
            a=msid:5UUdwiuY7OML2EkQtF38pJtNP5v7In1LhjEK f78dde68-7055-4e20-bb37-433803dd1ed1\r\n\
            a=rtcp-mux\r\n\
            a=rtpmap:111 opus/48000/2\r\n\
            a=rtcp-fb:111 transport-cc\r\n\
            a=fmtp:111 minptime=10;useinbandfec=1\r\n\
            a=ssrc:3948621874 cname:xeXs3aE9AOBn00yJ\r\n\
            a=ssrc:3948621874 msid:5UUdwiuY7OML2EkQtF38pJtNP5v7In1LhjEK \
f78dde68-7055-4e20-bb37-433803dd1ed1\r\n\
            a=ssrc:3948621874 mslabel:5UUdwiuY7OML2EkQtF38pJtNP5v7In1LhjEK\r\n\
            a=ssrc:3948621874 label:f78dde68-7055-4e20-bb37-433803dd1ed1\r\n\
            m=video 9 UDP/TLS/RTP/SAVPF 45 46\r\n\
            c=IN IP4 0.0.0.0\r\n\
            a=rtcp:9 IN IP4 0.0.0.0\r\n\
            a=ice-ufrag:S5hk\r\n\
            a=ice-pwd:0zV/Yu3y8aDzbHgqWhnVQhqP\r\n\
            a=ice-options:trickle\r\n\
            a=fingerprint:sha-256 8C:64:ED:03:76:D0:3D:B4:88:08:91:64:08:80:A8:C6:\
5A:BF:8B:4E:38:27:96:CA:08:49:25:73:46:60:20:DC\r\n\
            a=setup:actpass\r\n\
            a=mid:1\r\n\
            a=extmap:14 urn:ietf:params:rtp-hdrext:toffset\r\n\
            a=extmap:2 http://www.webrtc.org/experiments/rtp-hdrext/abs-send-time\r\n\
            a=extmap:13 urn:3gpp:video-orientation\r\n\
            a=extmap:3 http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01\r\n\
            a=extmap:5 http://www.webrtc.org/experiments/rtp-hdrext/playout-delay\r\n\
            a=extmap:6 http://www.webrtc.org/experiments/rtp-hdrext/video-content-type\r\n\
            a=extmap:7 http://www.webrtc.org/experiments/rtp-hdrext/video-timing\r\n\
            a=extmap:8 http://www.webrtc.org/experiments/rtp-hdrext/color-space\r\n\
            a=extmap:4 urn:ietf:params:rtp-hdrext:sdes:mid\r\n\
            a=extmap:10 urn:ietf:params:rtp-hdrext:sdes:rtp-stream-id\r\n\
            a=extmap:11 urn:ietf:params:rtp-hdrext:sdes:repaired-rtp-stream-id\r\n\
            a=sendrecv\r\n\
            a=msid:- 4018fd65-ac50-4861-89a4-1f2cc35bbb5e\r\n\
            a=rtcp-mux\r\n\
            a=rtcp-rsize\r\n\
            a=rtpmap:45 AV1/90000\r\n\
            a=rtcp-fb:45 goog-remb\r\n\
            a=rtcp-fb:45 transport-cc\r\n\
            a=rtcp-fb:45 ccm fir\r\n\
            a=rtcp-fb:45 nack\r\n\
            a=rtcp-fb:45 nack pli\r\n\
            a=fmtp:45 level-idx=5;profile=0;tier=0\r\n\
            a=rtpmap:46 rtx/90000\r\n\
            a=fmtp:46 apt=45\r\n\
            "
                )
            );
        }

        #[test]
        fn bundle_mids_returns_correct_mids() {
            let input = "v=0\r\n\
            o=- 123456 2 IN IP4 127.0.0.1\r\n\
            s=-\r\n\
            t=0 0\r\n\
            a=group:BUNDLE 0 1 2\r\n\
            a=msid-semantic:WMS *\r\n\
            m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n\
            c=IN IP4 0.0.0.0\r\n\
            a=mid:0\r\n\
            a=sendrecv\r\n\
            a=rtpmap:111 opus/48000/2\r\n\
            a=setup:actpass\r\n\
            a=ice-ufrag:test\r\n\
            a=ice-pwd:testpassword1234\r\n\
            m=video 0 UDP/TLS/RTP/SAVPF 96\r\n\
            c=IN IP4 0.0.0.0\r\n\
            a=mid:1\r\n\
            a=sendrecv\r\n\
            a=rtpmap:96 VP8/90000\r\n\
            a=setup:actpass\r\n\
            a=ice-ufrag:test\r\n\
            a=ice-pwd:testpassword1234\r\n\
            m=application 0 UDP/DTLS/SCTP webrtc-datachannel\r\n\
            c=IN IP4 0.0.0.0\r\n\
            a=mid:2\r\n\
            a=setup:actpass\r\n\
            a=ice-ufrag:test\r\n\
            a=ice-pwd:testpassword1234\r\n\
            a=sctp-port:5000\r\n\
            ";

            let sdp = Sdp::parse(input).expect("should parse");

            // bundle_mids should return all MIDs from the BUNDLE group
            let mids = sdp.bundle_mids().expect("should have BUNDLE group");
            assert_eq!(mids.len(), 3);
            assert_eq!(mids[0], "0".into());
            assert_eq!(mids[1], "1".into());
            assert_eq!(mids[2], "2".into());

            // The m-lines with port=0 should have disabled=true at parsing level
            // (interpretation of whether this means "rejected" happens later)
            assert!(!sdp.media_lines[0].disabled, "audio has port=9");
            assert!(sdp.media_lines[1].disabled, "video has port=0");
            assert!(sdp.media_lines[2].disabled, "application has port=0");
        }
    }

    /// Opus codec-specific tests.
    /// These tests are mainly verify SDP format params.
    mod opus_codec {
        use super::*;

        #[test]
        fn opus_fmtp_round_trip() {
            let f = FormatParams {
                use_dtx: Some(true),
                stereo: Some(true),
                sprop_stereo: Some(true),
                ..Default::default()
            };

            let fmtp_str = f.to_string();
            assert_eq!(fmtp_str, "stereo=1;sprop-stereo=1;usedtx=1");

            let parsed = FormatParams::parse_line(&fmtp_str);
            assert_eq!(parsed, f);
        }

        #[test]
        fn opus_default_to_mono() {
            let f = FormatParams {
                ..Default::default()
            };
            let fmtp_str = f.to_string();
            assert_eq!(fmtp_str, "");

            let parsed = FormatParams::parse_line(&fmtp_str);
            assert!(parsed.stereo.is_none());
            assert!(!parsed.stereo.unwrap_or(false));
            assert!(!parsed.sprop_stereo.unwrap_or(false));
        }
    }

    /// Core H.265 (HEVC) codec-specific tests.
    /// Tests basic H.265 parameter handling including profile-tier-level parsing,
    /// serialization, and format parameter combinations.
    mod h265_codec {
        use super::*;

        /// Test that H.265 format parameters can be serialized to a string and parsed back
        /// without loss of information. Verifies the round-trip conversion works correctly.
        #[test]
        fn h265_fmtp_round_trip() {
            // Test that H.265 fmtp params round-trip correctly
            let ptl = H265ProfileTierLevel::new(1, 0, 93).unwrap(); // Main, Main tier, Level 3.1

            let f = FormatParams {
                h265_profile_tier_level: Some(ptl),
                ..Default::default()
            };

            // Serialize to string
            let fmtp_str = f.to_string();
            assert_eq!(fmtp_str, "profile-id=1;tier-flag=0;level-id=93");

            // Parse back
            let parsed = FormatParams::parse_line(&fmtp_str);
            assert_eq!(parsed.h265_profile_tier_level, Some(ptl));
        }

        /// Test different combinations of H.265 profiles, tiers, and levels to ensure
        /// they are all serialized correctly. Covers Main, Main 10, different tiers, and levels.
        #[test]
        fn h265_different_profiles() {
            // Test Main profile (1)
            let main = H265ProfileTierLevel::new(1, 0, 93).unwrap();
            let f = FormatParams {
                h265_profile_tier_level: Some(main),
                ..Default::default()
            };
            assert_eq!(f.to_string(), "profile-id=1;tier-flag=0;level-id=93");

            // Test Main 10 profile (2)
            let main10 = H265ProfileTierLevel::new(2, 0, 93).unwrap();
            let f = FormatParams {
                h265_profile_tier_level: Some(main10),
                ..Default::default()
            };
            assert_eq!(f.to_string(), "profile-id=2;tier-flag=0;level-id=93");

            // Test different tier (High tier)
            let high_tier = H265ProfileTierLevel::new(1, 1, 93).unwrap();
            let f = FormatParams {
                h265_profile_tier_level: Some(high_tier),
                ..Default::default()
            };
            assert_eq!(f.to_string(), "profile-id=1;tier-flag=1;level-id=93");

            // Test different level (Level 5.1 = 153)
            let level_5_1 = H265ProfileTierLevel::new(1, 0, 153).unwrap();
            let f = FormatParams {
                h265_profile_tier_level: Some(level_5_1),
                ..Default::default()
            };
            assert_eq!(f.to_string(), "profile-id=1;tier-flag=0;level-id=153");
        }

        /// Test parsing H.265 parameters from key-value pairs as they would appear in SDP.
        /// Verifies that the three separate params (profile-id, tier-flag, level-id) are
        /// correctly combined into a single H265ProfileTierLevel composite.
        #[test]
        fn h265_parse_from_pairs() {
            // Test parsing from key-value pairs (as would come from SDP)
            let pairs = vec![
                ("profile-id".to_string(), "1".to_string()),
                ("tier-flag".to_string(), "0".to_string()),
                ("level-id".to_string(), "93".to_string()),
            ];

            let params = FormatParam::parse_pairs(pairs);

            // Should have exactly one H265ProfileTierLevel param
            let h265_params: Vec<_> = params
                .iter()
                .filter(|p| matches!(p, FormatParam::H265ProfileTierLevel(_)))
                .collect();
            assert_eq!(h265_params.len(), 1);

            if let FormatParam::H265ProfileTierLevel(ptl) = h265_params[0] {
                assert_eq!(ptl.profile_id(), 1);
                assert_eq!(ptl.tier_flag(), 0);
                assert_eq!(ptl.level_id(), 93);
            } else {
                panic!("Expected H265ProfileTierLevel");
            }
        }

        /// Test that H.265 profile-tier-level params can coexist with other H.265-specific
        /// parameters like sprop-max-don-diff without interfering with each other.
        #[test]
        fn h265_mixed_with_other_params() {
            // Test H.265 params mixed with other format params
            let pairs = vec![
                ("profile-id".to_string(), "1".to_string()),
                ("tier-flag".to_string(), "0".to_string()),
                ("level-id".to_string(), "93".to_string()),
                ("sprop-max-don-diff".to_string(), "0".to_string()),
            ];

            let params = FormatParam::parse_pairs(pairs);

            // Should have H265ProfileTierLevel and SpropMaxDonDiff
            assert!(
                params
                    .iter()
                    .any(|p| matches!(p, FormatParam::H265ProfileTierLevel(_)))
            );
            assert!(
                params
                    .iter()
                    .any(|p| matches!(p, FormatParam::SpropMaxDonDiff(0)))
            );
        }

        /// Test sprop-max-don-diff parsing, display, and round-trip for various values.
        #[test]
        fn sprop_max_don_diff_parse_and_display() {
            // Parse standalone sprop-max-don-diff
            let param = FormatParam::parse("sprop-max-don-diff", "32");
            assert!(matches!(param, FormatParam::SpropMaxDonDiff(32)));

            // Display format
            assert_eq!(param.to_string(), "sprop-max-don-diff=32");

            // Parse value 0 (DONL disabled)
            let param0 = FormatParam::parse("sprop-max-don-diff", "0");
            assert!(matches!(param0, FormatParam::SpropMaxDonDiff(0)));

            // Parse max valid value (RFC 7798 §7.1: 0..32767)
            let param_max = FormatParam::parse("sprop-max-don-diff", "32767");
            assert!(matches!(param_max, FormatParam::SpropMaxDonDiff(32767)));

            // Out-of-range value (32768) rejected per RFC 7798 §7.1
            let param_over = FormatParam::parse("sprop-max-don-diff", "32768");
            assert!(matches!(param_over, FormatParam::Unknown));

            // Invalid value falls back to Unknown
            let param_bad = FormatParam::parse("sprop-max-don-diff", "notanumber");
            assert!(matches!(param_bad, FormatParam::Unknown));
        }

        /// Test that sprop-max-don-diff > 0 round-trips through H.265 fmtp with PTL params.
        #[test]
        fn h265_sprop_max_don_diff_nonzero_with_ptl() {
            let pairs = vec![
                ("profile-id".to_string(), "1".to_string()),
                ("tier-flag".to_string(), "0".to_string()),
                ("level-id".to_string(), "93".to_string()),
                ("sprop-max-don-diff".to_string(), "32".to_string()),
            ];

            let params = FormatParam::parse_pairs(pairs);

            // Should have both H265ProfileTierLevel and SpropMaxDonDiff(32)
            assert!(
                params
                    .iter()
                    .any(|p| matches!(p, FormatParam::H265ProfileTierLevel(_)))
            );
            assert!(
                params
                    .iter()
                    .any(|p| matches!(p, FormatParam::SpropMaxDonDiff(32)))
            );

            // Verify PTL values are correct
            if let Some(FormatParam::H265ProfileTierLevel(ptl)) = params
                .iter()
                .find(|p| matches!(p, FormatParam::H265ProfileTierLevel(_)))
            {
                assert_eq!(ptl.profile_id(), 1);
                assert_eq!(ptl.tier_flag(), 0);
                assert_eq!(ptl.level_id(), 93);
            }
        }
    }

    /// Tests ensuring H.265 parameters are not confused with other codecs.
    /// Verifies that H.265's three-parameter format (profile-id, tier-flag, level-id)
    /// is correctly distinguished from H.264's profile-level-id, VP9's profile-id,
    /// and AV1's profile/tier/level-idx parameters.
    mod no_confusion {
        use super::*;

        /// Test that H.264's profile-level-id parameter (single hex value) is not confused
        /// with H.265's three separate parameters. H.264 uses a different format entirely.
        #[test]
        fn h265_no_confusion_with_h264() {
            // H.264 uses profile-level-id (single param)
            let h264_pairs = vec![
                ("profile-level-id".to_string(), "42e01f".to_string()),
                ("packetization-mode".to_string(), "1".to_string()),
            ];

            let h264_params = FormatParam::parse_pairs(h264_pairs);

            // Should NOT have H265ProfileTierLevel
            assert!(
                !h264_params
                    .iter()
                    .any(|p| matches!(p, FormatParam::H265ProfileTierLevel(_)))
            );

            // Should have ProfileLevelId (H.264)
            assert!(
                h264_params
                    .iter()
                    .any(|p| matches!(p, FormatParam::ProfileLevelId(_)))
            );
            assert!(
                h264_params
                    .iter()
                    .any(|p| matches!(p, FormatParam::PacketizationMode(1)))
            );
        }

        /// Test that VP9's profile-id parameter (standalone) is not confused with H.265's
        /// profile-id which must be combined with tier-flag and level-id.
        #[test]
        fn h265_no_confusion_with_vp9() {
            // VP9 uses profile-id (single param, not combined)
            let vp9_pairs = vec![("profile-id".to_string(), "0".to_string())];

            let vp9_params = FormatParam::parse_pairs(vp9_pairs);

            // Should NOT have H265ProfileTierLevel (missing tier-flag and level-id)
            assert!(
                !vp9_params
                    .iter()
                    .any(|p| matches!(p, FormatParam::H265ProfileTierLevel(_)))
            );

            // Should have ProfileId (VP9)
            assert!(
                vp9_params
                    .iter()
                    .any(|p| matches!(p, FormatParam::ProfileId(0)))
            );
        }

        /// Test that AV1's parameters (profile, tier, level-idx) use different naming than
        /// H.265 (profile-id, tier-flag, level-id) and are not confused during parsing.
        #[test]
        fn h265_no_confusion_with_av1() {
            // AV1 uses profile, level-idx, tier (but should not be confused with H.265)
            let av1_pairs = vec![
                ("profile".to_string(), "0".to_string()),
                ("level-idx".to_string(), "5".to_string()),
                ("tier".to_string(), "0".to_string()),
            ];

            let av1_params = FormatParam::parse_pairs(av1_pairs);

            // Should NOT have H265ProfileTierLevel
            // (AV1 uses different param names: profile vs profile-id,
            // tier vs tier-flag, level-idx vs level-id)
            assert!(
                !av1_params
                    .iter()
                    .any(|p| matches!(p, FormatParam::H265ProfileTierLevel(_)))
            );

            // Should have AV1-specific params
            assert!(
                av1_params
                    .iter()
                    .any(|p| matches!(p, FormatParam::Profile(0)))
            );
            assert!(
                av1_params
                    .iter()
                    .any(|p| matches!(p, FormatParam::LevelIdx(5)))
            );
            assert!(av1_params.iter().any(|p| matches!(p, FormatParam::Tier(0))));
        }
    }

    /// Additional H.265 parsing and validation tests.
    /// Covers incomplete parameter handling and complete SDP integration scenarios.
    mod h265_additional {
        use super::*;

        /// Test that incomplete H.265 parameter sets do not create a composite ProfileTierLevel.
        /// All three parameters (profile-id, tier-flag, level-id) must be present.
        #[test]
        fn h265_partial_params_no_composite() {
            // If only some H.265 params are present, should not create composite
            let partial_pairs = vec![
                ("profile-id".to_string(), "1".to_string()),
                ("tier-flag".to_string(), "0".to_string()),
                // Missing level-id
            ];

            let params = FormatParam::parse_pairs(partial_pairs);

            // Should NOT have H265ProfileTierLevel (incomplete)
            assert!(
                !params
                    .iter()
                    .any(|p| matches!(p, FormatParam::H265ProfileTierLevel(_)))
            );
        }

        /// Test H.265 parameters in a complete SDP offer/answer scenario.
        /// Verifies that H.265 rtpmap and fmtp lines are correctly parsed along with RTX.
        #[test]
        fn h265_in_complete_sdp() {
            let input = "v=0\r\n\
            o=- 123456 2 IN IP4 127.0.0.1\r\n\
            s=-\r\n\
            t=0 0\r\n\
            a=group:BUNDLE 0\r\n\
            m=video 9 UDP/TLS/RTP/SAVPF 96 97\r\n\
            c=IN IP4 0.0.0.0\r\n\
            a=mid:0\r\n\
            a=sendrecv\r\n\
            a=rtpmap:96 H265/90000\r\n\
            a=fmtp:96 profile-id=1;tier-flag=0;level-id=93\r\n\
            a=rtpmap:97 rtx/90000\r\n\
            a=fmtp:97 apt=96\r\n\
            a=setup:actpass\r\n\
            a=ice-ufrag:test\r\n\
            a=ice-pwd:testpassword\r\n\
            ";

            let sdp = Sdp::parse(input).expect("should parse");

            // Get RTP params
            let params = sdp.media_lines[0].rtp_params();

            // Find H.265 payload
            let h265_payload = params
                .iter()
                .find(|p| p.spec.codec == Codec::H265)
                .expect("should have H.265");

            assert_eq!(h265_payload.pt, 96.into());
            assert_eq!(h265_payload.spec.clock_rate, Frequency::NINETY_KHZ);

            // Check H.265 params
            let ptl = h265_payload
                .spec
                .format
                .h265_profile_tier_level
                .expect("should have H.265 PTL");
            assert_eq!(ptl.profile_id(), 1);
            assert_eq!(ptl.tier_flag(), 0);
            assert_eq!(ptl.level_id(), 93);

            // Check RTX
            assert_eq!(h265_payload.resend, Some(97.into()));
        }

        #[test]
        fn red_fmtp_is_interpreted_with_red_rtpmap() {
            // Single-level same-codec RED renders as "<primary>/<primary>".
            assert_eq!(FormatParam::Red(111.into()).to_string(), "111/111");

            let input = "v=0\r\n\
            o=- 1 2 IN IP4 127.0.0.1\r\n\
            s=-\r\n\
            t=0 0\r\n\
            a=group:BUNDLE 0\r\n\
            m=audio 9 UDP/TLS/RTP/SAVPF 111 63\r\n\
            c=IN IP4 0.0.0.0\r\n\
            a=mid:0\r\n\
            a=sendrecv\r\n\
            a=rtpmap:111 opus/48000/2\r\n\
            a=rtpmap:63 red/48000/2\r\n\
            a=fmtp:63 111/111\r\n\
            a=setup:actpass\r\n\
            a=ice-ufrag:test\r\n\
            a=ice-pwd:testpassword\r\n\
            ";
            let sdp = Sdp::parse(input).expect("should parse");
            assert!(sdp.media_lines[0].attrs.iter().any(|a| {
                matches!(a, MediaAttribute::Fmtp { pt, values }
                    if *pt == 63.into() && values == &vec![FormatParam::BarePtList(111.into())])
            }));
            let params = sdp.media_lines[0].rtp_params();
            assert_eq!(params[0].red, Some(63.into()));

            // The same bare fmtp text must not be treated as RED without a RED rtpmap.
            let other_codec = input.replace("a=rtpmap:63 red/48000/2", "a=rtpmap:63 opus/48000/2");
            let parsed = Sdp::parse(&other_codec).expect("should parse");
            assert_eq!(parsed.media_lines[0].rtp_params()[0].red, None);
        }

        #[test]
        fn red_folds_onto_opus_and_emits() {
            let input = "v=0\r\n\
            o=- 1 2 IN IP4 127.0.0.1\r\n\
            s=-\r\n\
            t=0 0\r\n\
            a=group:BUNDLE 0\r\n\
            m=audio 9 UDP/TLS/RTP/SAVPF 111 63\r\n\
            c=IN IP4 0.0.0.0\r\n\
            a=mid:0\r\n\
            a=sendrecv\r\n\
            a=rtpmap:111 opus/48000/2\r\n\
            a=rtpmap:63 red/48000/2\r\n\
            a=fmtp:63 111/111\r\n\
            a=setup:actpass\r\n\
            a=ice-ufrag:test\r\n\
            a=ice-pwd:testpassword\r\n\
            ";
            let sdp = Sdp::parse(input).expect("should parse");
            let params = sdp.media_lines[0].rtp_params();

            let opus = params
                .iter()
                .find(|p| p.spec.codec == Codec::Opus)
                .expect("opus");
            assert_eq!(opus.red, Some(63.into()));
            // RED is folded onto the primary, never a standalone payload.
            assert!(!params.iter().any(|p| p.pt == 63.into()));

            // Emit: opus re-emits the red rtpmap + fmtp.
            let mut attrs = Vec::new();
            opus.as_media_attrs(&mut attrs);
            let has_red_rtpmap = attrs.iter().any(|a| {
                matches!(a, MediaAttribute::RtpMap { pt, value }
                    if *pt == 63.into() && value.codec == Codec::Red && value.channels == Some(2))
            });
            let has_red_fmtp = attrs.iter().any(|a| {
                matches!(a, MediaAttribute::Fmtp { pt, values }
                    if *pt == 63.into() && values == &vec![FormatParam::Red(111.into())])
            });
            assert!(has_red_rtpmap, "should emit red rtpmap");
            assert!(has_red_fmtp, "should emit red fmtp");
        }

        #[test]
        fn red_folds_in_real_chrome_audio_mline() {
            // Real captured Chrome offer audio section (see docs/chrome-sdp.json): RED (63) sits in
            // the m-line fmt list right after Opus (111), among 16 payload types.
            let input = "v=0\r\n\
            o=- 2954344436352400798 4 IN IP4 127.0.0.1\r\n\
            s=-\r\n\
            t=0 0\r\n\
            a=group:BUNDLE 0\r\n\
            m=audio 9 UDP/TLS/RTP/SAVPF 111 63 103 104 9 0 8 106 105 13 110 112 113 126\r\n\
            c=IN IP4 0.0.0.0\r\n\
            a=mid:0\r\n\
            a=sendrecv\r\n\
            a=rtcp-mux\r\n\
            a=rtpmap:111 opus/48000/2\r\n\
            a=rtcp-fb:111 transport-cc\r\n\
            a=fmtp:111 minptime=10;useinbandfec=1\r\n\
            a=rtpmap:63 red/48000/2\r\n\
            a=fmtp:63 111/111\r\n\
            a=rtpmap:103 ISAC/16000\r\n\
            a=rtpmap:104 ISAC/32000\r\n\
            a=rtpmap:9 G722/8000\r\n\
            a=rtpmap:0 PCMU/8000\r\n\
            a=rtpmap:8 PCMA/8000\r\n\
            a=rtpmap:106 CN/32000\r\n\
            a=rtpmap:105 CN/16000\r\n\
            a=rtpmap:13 CN/8000\r\n\
            a=rtpmap:110 telephone-event/48000\r\n\
            a=rtpmap:112 telephone-event/32000\r\n\
            a=rtpmap:113 telephone-event/16000\r\n\
            a=rtpmap:126 telephone-event/8000\r\n\
            a=setup:actpass\r\n\
            a=ice-ufrag:test\r\n\
            a=ice-pwd:testpassword\r\n\
            ";
            let sdp = Sdp::parse(input).expect("parse real chrome audio m-line");
            let params = sdp.media_lines[0].rtp_params();

            let opus = params
                .iter()
                .find(|p| p.spec.codec == Codec::Opus)
                .expect("opus");
            assert_eq!(
                opus.red,
                Some(63.into()),
                "RED must fold onto Opus among real Chrome PTs"
            );
            assert!(
                !params.iter().any(|p| p.pt == 63.into()),
                "RED is folded, not a standalone payload"
            );
            // Opus fmtp is not clobbered by the RED fold.
            assert_eq!(opus.spec.format.use_inband_fec, Some(true));
            // Real-world neighbours still parse alongside.
            assert!(params.iter().any(|p| p.spec.codec == Codec::PCMU));
            assert!(params.iter().any(|p| p.spec.codec == Codec::PCMA));
        }

        #[test]
        fn red_not_folded_onto_video() {
            // str0m supports RED only for Opus. A video m-line that links red to a video codec
            // must not have red folded onto it; the video param's `red` stays None.
            let input = "v=0\r\n\
            o=- 1 2 IN IP4 127.0.0.1\r\n\
            s=-\r\n\
            t=0 0\r\n\
            a=group:BUNDLE 0\r\n\
            m=video 9 UDP/TLS/RTP/SAVPF 96 116\r\n\
            c=IN IP4 0.0.0.0\r\n\
            a=mid:0\r\n\
            a=sendrecv\r\n\
            a=rtpmap:96 VP8/90000\r\n\
            a=rtpmap:116 red/90000\r\n\
            a=fmtp:116 96/96\r\n\
            a=setup:actpass\r\n\
            a=ice-ufrag:test\r\n\
            a=ice-pwd:testpassword\r\n\
            ";
            let sdp = Sdp::parse(input).expect("parse video m-line with red");
            let params = sdp.media_lines[0].rtp_params();

            let vp8 = params
                .iter()
                .find(|p| p.spec.codec == Codec::Vp8)
                .expect("vp8");
            assert_eq!(vp8.red, None, "RED must not fold onto a video codec");
        }

        #[test]
        fn red_folds_onto_non_opus_audio() {
            // RED is audio-agnostic: a remote that links red to any audio codec (here PCMU)
            // has red folded onto that codec's params.
            let input = "v=0\r\n\
            o=- 1 2 IN IP4 127.0.0.1\r\n\
            s=-\r\n\
            t=0 0\r\n\
            a=group:BUNDLE 0\r\n\
            m=audio 9 UDP/TLS/RTP/SAVPF 0 63\r\n\
            c=IN IP4 0.0.0.0\r\n\
            a=mid:0\r\n\
            a=sendrecv\r\n\
            a=rtpmap:0 PCMU/8000\r\n\
            a=rtpmap:63 red/8000\r\n\
            a=fmtp:63 0/0\r\n\
            a=setup:actpass\r\n\
            a=ice-ufrag:test\r\n\
            a=ice-pwd:testpassword\r\n\
            ";
            let sdp = Sdp::parse(input).expect("parse audio m-line with red on PCMU");
            let params = sdp.media_lines[0].rtp_params();

            let pcmu = params
                .iter()
                .find(|p| p.spec.codec == Codec::PCMU)
                .expect("pcmu");
            assert_eq!(
                pcmu.red,
                Some(63.into()),
                "RED folds onto any audio codec, not just Opus"
            );
        }

        #[test]
        fn g722_red_emits_wire_clock_rate() {
            // G722 has a 16 kHz media clock but an 8 kHz RTP clock (RFC 3551). Its RED rtpmap
            // must advertise 8000 (the wire rate), not 16000, or the peer derives the wrong
            // timestamps.
            let input = "v=0\r\n\
            o=- 1 2 IN IP4 127.0.0.1\r\n\
            s=-\r\n\
            t=0 0\r\n\
            a=group:BUNDLE 0\r\n\
            m=audio 9 UDP/TLS/RTP/SAVPF 9 63\r\n\
            c=IN IP4 0.0.0.0\r\n\
            a=mid:0\r\n\
            a=sendrecv\r\n\
            a=rtpmap:9 G722/8000\r\n\
            a=rtpmap:63 red/8000\r\n\
            a=fmtp:63 9/9\r\n\
            a=setup:actpass\r\n\
            a=ice-ufrag:test\r\n\
            a=ice-pwd:testpassword\r\n\
            ";
            let sdp = Sdp::parse(input).expect("parse G722 + red");
            let params = sdp.media_lines[0].rtp_params();
            let g722 = params
                .iter()
                .find(|p| p.spec.codec == Codec::G722)
                .expect("g722");
            assert_eq!(g722.red, Some(63.into()), "RED folds onto G722");

            let mut attrs = Vec::new();
            g722.as_media_attrs(&mut attrs);
            let red_clock = attrs.iter().find_map(|a| match a {
                MediaAttribute::RtpMap { value, .. } if value.codec == Codec::Red => {
                    Some(value.clock_rate)
                }
                _ => None,
            });
            assert_eq!(
                red_clock,
                Some(Frequency::EIGHT_KHZ),
                "G722 RED rtpmap must advertise the 8 kHz wire clock"
            );
        }
    }

    /// Multi-codec integration tests.
    /// Verifies that multiple video codecs (H.264, H.265, VP8, VP9) can coexist
    /// in the same SDP without parameter conflicts or interference.
    mod multi_codec {
        use super::*;

        /// Test that H.264, H.265, and other video codecs can coexist in the same SDP
        /// without their format parameters interfering with each other.
        #[test]
        fn h265_and_h264_coexist() {
            let input = "v=0\r\n\
            o=- 123456 2 IN IP4 127.0.0.1\r\n\
            s=-\r\n\
            t=0 0\r\n\
            a=group:BUNDLE 0\r\n\
            m=video 9 UDP/TLS/RTP/SAVPF 96 97 98\r\n\
            c=IN IP4 0.0.0.0\r\n\
            a=mid:0\r\n\
            a=sendrecv\r\n\
            a=rtpmap:96 H264/90000\r\n\
            a=fmtp:96 profile-level-id=42e01f;packetization-mode=1\r\n\
            a=rtpmap:97 H265/90000\r\n\
            a=fmtp:97 profile-id=1;tier-flag=0;level-id=93\r\n\
            a=rtpmap:98 VP8/90000\r\n\
            a=setup:actpass\r\n\
            a=ice-ufrag:test\r\n\
            a=ice-pwd:testpassword\r\n\
            ";

            let sdp = Sdp::parse(input).expect("should parse");
            let params = sdp.media_lines[0].rtp_params();

            // Check H.264
            let h264 = params
                .iter()
                .find(|p| p.spec.codec == Codec::H264)
                .expect("should have H.264");
            assert_eq!(h264.pt, 96.into());
            assert_eq!(h264.spec.format.profile_level_id, Some(0x42e01f));
            assert_eq!(h264.spec.format.packetization_mode, Some(1));
            assert!(h264.spec.format.h265_profile_tier_level.is_none());

            // Check H.265
            let h265 = params
                .iter()
                .find(|p| p.spec.codec == Codec::H265)
                .expect("should have H.265");
            assert_eq!(h265.pt, 97.into());
            let ptl = h265
                .spec
                .format
                .h265_profile_tier_level
                .expect("should have H.265 PTL");
            assert_eq!(ptl.profile_id(), 1);
            assert!(h265.spec.format.profile_level_id.is_none());

            // Check VP8
            let vp8 = params
                .iter()
                .find(|p| p.spec.codec == Codec::Vp8)
                .expect("should have VP8");
            assert_eq!(vp8.pt, 98.into());
            assert!(vp8.spec.format.h265_profile_tier_level.is_none());
            assert!(vp8.spec.format.profile_level_id.is_none());
        }
    }

    /// H.265 serialization and edge case tests.
    /// Tests media attribute generation, empty parameter handling, extreme values,
    /// parameter ordering independence, DONL parameters, and invalid value handling.
    mod h265_serialization {
        use super::*;

        /// Test that H.265 parameters are correctly serialized as media attributes,
        /// including rtpmap, fmtp with ProfileTierLevel, rtcp-fb, and RTX payload.
        #[test]
        fn h265_serialization_in_media_attrs() {
            let ptl = H265ProfileTierLevel::new(2, 1, 153).unwrap(); // Main 10, High tier, Level 5.1

            let mut payload = PayloadParams::new(
                100.into(),
                Some(101.into()),
                CodecSpec {
                    codec: Codec::H265,
                    clock_rate: Frequency::NINETY_KHZ,
                    channels: None,
                    format: FormatParams {
                        h265_profile_tier_level: Some(ptl),
                        ..Default::default()
                    },
                },
            );
            payload.fb_transport_cc = true;
            payload.fb_nack = true;
            payload.fb_pli = true;

            let mut attrs = vec![];
            payload.as_media_attrs(&mut attrs);

            // Should have rtpmap for H.265
            assert!(attrs.iter().any(|a| matches!(a,
                MediaAttribute::RtpMap { pt, value }
                if *pt == 100.into() && value.codec == Codec::H265
            )));

            // Should have fmtp with H.265 params
            let fmtp = attrs.iter().find(|a| {
                matches!(a,
                    MediaAttribute::Fmtp { pt, .. } if *pt == 100.into()
                )
            });
            assert!(fmtp.is_some());

            if let Some(MediaAttribute::Fmtp { values, .. }) = fmtp {
                assert!(
                    values
                        .iter()
                        .any(|v| matches!(v, FormatParam::H265ProfileTierLevel(p)
                            if p.profile_id() == 2 && p.tier_flag() == 1 && p.level_id() == 153
                        ))
                );
            }

            // Should have rtx mapping
            assert!(attrs.iter().any(|a| matches!(a,
                MediaAttribute::RtpMap { pt, value }
                if *pt == 101.into() && value.codec == Codec::Rtx
            )));
        }

        /// Test that empty format parameters do not generate an a=fmtp line in SDP,
        /// avoiding invalid SDP syntax with empty fmtp attributes.
        #[test]
        fn h265_empty_fmtp_not_serialized() {
            // Empty fmtp should not be written
            let payload = PayloadParams::new(
                100.into(),
                None,
                CodecSpec {
                    codec: Codec::H265,
                    clock_rate: Frequency::NINETY_KHZ,
                    channels: None,
                    format: FormatParams::default(),
                },
            );

            let mut attrs = vec![];
            payload.as_media_attrs(&mut attrs);

            // Should NOT have fmtp line (it would be empty)
            assert!(
                !attrs
                    .iter()
                    .any(|a| matches!(a, MediaAttribute::Fmtp { .. }))
            );
        }

        /// Test H.265 with extreme profile/tier/level values at boundaries.
        /// Verifies parsing and serialization of edge case values.
        #[test]
        fn h265_extreme_values() {
            // Test minimum level (Level 1.0 = 30)
            let min_level = H265ProfileTierLevel::new(1, 0, 30).unwrap();
            let f = FormatParams {
                h265_profile_tier_level: Some(min_level),
                ..Default::default()
            };
            assert_eq!(f.to_string(), "profile-id=1;tier-flag=0;level-id=30");

            // Test maximum level (Level 6.2 = 186)
            let max_level = H265ProfileTierLevel::new(1, 0, 186).unwrap();
            let f = FormatParams {
                h265_profile_tier_level: Some(max_level),
                ..Default::default()
            };
            assert_eq!(f.to_string(), "profile-id=1;tier-flag=0;level-id=186");

            // Test Main10 with High tier
            let main10_high = H265ProfileTierLevel::new(2, 1, 93).unwrap();
            let f = FormatParams {
                h265_profile_tier_level: Some(main10_high),
                ..Default::default()
            };
            assert_eq!(f.to_string(), "profile-id=2;tier-flag=1;level-id=93");
        }

        /// Test H.265 parameter ordering doesn't affect parsing.
        /// SDP parameters can appear in any order.
        #[test]
        fn h265_parameter_order_independence() {
            // Normal order
            let normal_order = vec![
                ("profile-id".to_string(), "1".to_string()),
                ("tier-flag".to_string(), "0".to_string()),
                ("level-id".to_string(), "93".to_string()),
            ];

            // Reversed order
            let reversed_order = vec![
                ("level-id".to_string(), "93".to_string()),
                ("tier-flag".to_string(), "0".to_string()),
                ("profile-id".to_string(), "1".to_string()),
            ];

            // Mixed order
            let mixed_order = vec![
                ("tier-flag".to_string(), "0".to_string()),
                ("profile-id".to_string(), "1".to_string()),
                ("level-id".to_string(), "93".to_string()),
            ];

            let normal_params = FormatParam::parse_pairs(normal_order);
            let reversed_params = FormatParam::parse_pairs(reversed_order);
            let mixed_params = FormatParam::parse_pairs(mixed_order);

            // All should create H265ProfileTierLevel
            assert!(
                normal_params
                    .iter()
                    .any(|p| matches!(p, FormatParam::H265ProfileTierLevel(_)))
            );
            assert!(
                reversed_params
                    .iter()
                    .any(|p| matches!(p, FormatParam::H265ProfileTierLevel(_)))
            );
            assert!(
                mixed_params
                    .iter()
                    .any(|p| matches!(p, FormatParam::H265ProfileTierLevel(_)))
            );

            // All should have the same values
            for params in [&normal_params, &reversed_params, &mixed_params] {
                if let Some(FormatParam::H265ProfileTierLevel(ptl)) = params
                    .iter()
                    .find(|p| matches!(p, FormatParam::H265ProfileTierLevel(_)))
                {
                    assert_eq!(ptl.profile_id(), 1);
                    assert_eq!(ptl.tier_flag(), 0);
                    assert_eq!(ptl.level_id(), 93);
                }
            }
        }

        #[test]
        fn h265_invalid_parameter_values() {
            // Invalid profile-id
            let invalid_profile = vec![
                ("profile-id".to_string(), "999".to_string()),
                ("tier-flag".to_string(), "0".to_string()),
                ("level-id".to_string(), "93".to_string()),
            ];
            let params = FormatParam::parse_pairs(invalid_profile);
            assert!(
                !params
                    .iter()
                    .any(|p| matches!(p, FormatParam::H265ProfileTierLevel(_)))
            );

            // Invalid tier-flag
            let invalid_tier = vec![
                ("profile-id".to_string(), "1".to_string()),
                ("tier-flag".to_string(), "5".to_string()),
                ("level-id".to_string(), "93".to_string()),
            ];
            let params = FormatParam::parse_pairs(invalid_tier);
            assert!(
                !params
                    .iter()
                    .any(|p| matches!(p, FormatParam::H265ProfileTierLevel(_)))
            );

            // Invalid level-id
            let invalid_level = vec![
                ("profile-id".to_string(), "1".to_string()),
                ("tier-flag".to_string(), "0".to_string()),
                ("level-id".to_string(), "999".to_string()),
            ];
            let params = FormatParam::parse_pairs(invalid_level);
            assert!(
                !params
                    .iter()
                    .any(|p| matches!(p, FormatParam::H265ProfileTierLevel(_)))
            );
        }
    }

    /// H.265 integration with RTCP feedback mechanisms.
    /// Tests H.265 behavior with multiple codecs and various RTCP feedback types
    /// including NACK, PLI, FIR, and transport-cc.
    mod h265_integration {
        use super::*;

        /// Test H.265 in SDP with multiple video codecs and feedback mechanisms.
        /// Verifies proper parsing of complex multi-codec scenarios.
        #[test]
        fn h265_multi_codec_with_feedback() {
            let input = "v=0\r\n\
            o=- 123456 2 IN IP4 127.0.0.1\r\n\
            s=-\r\n\
            t=0 0\r\n\
            a=group:BUNDLE 0\r\n\
            m=video 9 UDP/TLS/RTP/SAVPF 96 97 98 99\r\n\
            c=IN IP4 0.0.0.0\r\n\
            a=mid:0\r\n\
            a=sendrecv\r\n\
            a=rtpmap:96 H265/90000\r\n\
            a=fmtp:96 profile-id=1;tier-flag=0;level-id=93\r\n\
            a=rtcp-fb:96 nack\r\n\
            a=rtcp-fb:96 nack pli\r\n\
            a=rtcp-fb:96 ccm fir\r\n\
            a=rtcp-fb:96 transport-cc\r\n\
            a=rtpmap:97 rtx/90000\r\n\
            a=fmtp:97 apt=96\r\n\
            a=rtpmap:98 VP9/90000\r\n\
            a=fmtp:98 profile-id=0\r\n\
            a=rtpmap:99 rtx/90000\r\n\
            a=fmtp:99 apt=98\r\n\
            a=setup:actpass\r\n\
            a=ice-ufrag:test\r\n\
            a=ice-pwd:testpassword\r\n\
            ";

            let sdp = Sdp::parse(input).expect("should parse");
            let params = sdp.media_lines[0].rtp_params();

            // Check H.265 with all feedback mechanisms
            let h265 = params
                .iter()
                .find(|p| p.spec.codec == Codec::H265)
                .expect("should have H.265");
            assert_eq!(h265.pt, 96.into());
            assert!(h265.fb_nack);
            assert!(h265.fb_pli);
            assert!(h265.fb_fir);
            assert!(h265.fb_transport_cc);
            assert_eq!(h265.resend, Some(97.into()));

            // Verify H.265 PTL is correct
            let ptl = h265
                .spec
                .format
                .h265_profile_tier_level
                .expect("should have PTL");
            assert_eq!(ptl.profile_id(), 1);

            // Check VP9 is separate and correct
            let vp9 = params
                .iter()
                .find(|p| p.spec.codec == Codec::Vp9)
                .expect("should have VP9");
            assert_eq!(vp9.pt, 98.into());
            assert_eq!(vp9.spec.format.profile_id, Some(0));
            assert!(vp9.spec.format.h265_profile_tier_level.is_none());
        }
    }

    /// Advanced H.265 functionality tests.
    /// Covers SDP round-trip serialization, incomplete parameter permutations,
    /// browser compatibility (Chrome profile-only mode), and validation of all
    /// standard H.265 level values (1.0 through 6.2).
    mod h265_advanced {
        use super::*;

        /// Test H.265 SDP serialization maintains parameter integrity.
        /// Ensures that serialized SDP can be parsed back with identical values.
        #[test]
        fn h265_sdp_serialization_round_trip() {
            let ptl = H265ProfileTierLevel::new(2, 0, 153).unwrap();

            let payload = PayloadParams::new(
                100.into(),
                Some(101.into()),
                CodecSpec {
                    codec: Codec::H265,
                    clock_rate: Frequency::NINETY_KHZ,
                    channels: None,
                    format: FormatParams {
                        h265_profile_tier_level: Some(ptl),
                        ..Default::default()
                    },
                },
            );

            let mut attrs = vec![];
            payload.as_media_attrs(&mut attrs);

            // Find the fmtp line
            let fmtp = attrs
                .iter()
                .find_map(|a| match a {
                    MediaAttribute::Fmtp { pt, values } if *pt == 100.into() => Some(values),
                    _ => None,
                })
                .expect("should have fmtp");

            // Should contain H265ProfileTierLevel
            assert!(
                fmtp.iter().any(
                    |v| matches!(v, FormatParam::H265ProfileTierLevel(p) if p.profile_id() == 2)
                )
            );
        }

        /// Test that only two of three H.265 params doesn't create PTL.
        /// Each permutation of missing parameter should fail.
        #[test]
        fn h265_incomplete_params_all_permutations() {
            // Missing level-id
            let missing_level = vec![
                ("profile-id".to_string(), "1".to_string()),
                ("tier-flag".to_string(), "0".to_string()),
            ];
            let params = FormatParam::parse_pairs(missing_level);
            assert!(
                !params
                    .iter()
                    .any(|p| matches!(p, FormatParam::H265ProfileTierLevel(_)))
            );

            // Missing tier-flag
            let missing_tier = vec![
                ("profile-id".to_string(), "1".to_string()),
                ("level-id".to_string(), "93".to_string()),
            ];
            let params = FormatParam::parse_pairs(missing_tier);
            assert!(
                !params
                    .iter()
                    .any(|p| matches!(p, FormatParam::H265ProfileTierLevel(_)))
            );

            // Missing profile-id
            let missing_profile = vec![
                ("tier-flag".to_string(), "0".to_string()),
                ("level-id".to_string(), "93".to_string()),
            ];
            let params = FormatParam::parse_pairs(missing_profile);
            assert!(
                !params
                    .iter()
                    .any(|p| matches!(p, FormatParam::H265ProfileTierLevel(_)))
            );

            // Only one parameter
            let only_profile = vec![("profile-id".to_string(), "1".to_string())];
            let params = FormatParam::parse_pairs(only_profile);
            assert!(
                !params
                    .iter()
                    .any(|p| matches!(p, FormatParam::H265ProfileTierLevel(_)))
            );
        }

        /// Test H.265 with Chrome-style profile-only in complete SDP.
        /// Verifies that browser compatibility mode works in real SDP.
        #[test]
        fn h265_chrome_profile_only_sdp() {
            let input = "v=0\r\n\
            o=- 123456 2 IN IP4 127.0.0.1\r\n\
            s=-\r\n\
            t=0 0\r\n\
            a=group:BUNDLE 0\r\n\
            m=video 9 UDP/TLS/RTP/SAVPF 96\r\n\
            c=IN IP4 0.0.0.0\r\n\
            a=mid:0\r\n\
            a=sendrecv\r\n\
            a=rtpmap:96 H265/90000\r\n\
            a=fmtp:96 profile-id=1\r\n\
            a=setup:actpass\r\n\
            a=ice-ufrag:test\r\n\
            a=ice-pwd:testpassword\r\n\
            ";

            let sdp = Sdp::parse(input).expect("should parse");
            let params = sdp.media_lines[0].rtp_params();

            let h265 = params
                .iter()
                .find(|p| p.spec.codec == Codec::H265)
                .expect("should have H.265");

            // Should NOT have full PTL
            assert!(h265.spec.format.h265_profile_tier_level.is_none());

            // Should have profile_id field instead
            assert_eq!(h265.spec.format.profile_id, Some(1));
        }

        /// Test H.265 different level values in valid range.
        /// Ensures all valid level IDs are properly handled.
        #[test]
        fn h265_all_valid_levels() {
            let valid_levels = vec![
                30,  // Level 1.0
                60,  // Level 2.0
                63,  // Level 2.1
                90,  // Level 3.0
                93,  // Level 3.1
                120, // Level 4.0
                123, // Level 4.1
                150, // Level 5.0
                153, // Level 5.1
                156, // Level 5.2
                180, // Level 6.0
                183, // Level 6.1
                186, // Level 6.2
            ];

            for level_id in valid_levels {
                let ptl = H265ProfileTierLevel::new(1, 0, level_id);
                assert!(ptl.is_some(), "Level ID {} should be valid", level_id);

                let f = FormatParams {
                    h265_profile_tier_level: ptl,
                    ..Default::default()
                };

                let serialized = f.to_string();
                assert!(serialized.contains(&format!("level-id={}", level_id)));
            }
        }
    }

    /// Core H.266 (VVC) codec-specific tests, mirrored from h265_codec.
    /// Tests basic H.266 parameter handling including profile-tier-level parsing,
    /// serialization, and format parameter combinations.
    mod h266_codec {
        use super::*;

        /// Test that H.266 format parameters can be serialized to a string and parsed back
        /// without loss of information. Verifies the round-trip conversion works correctly.
        #[test]
        fn h266_fmtp_round_trip() {
            // Test that H.266 fmtp params round-trip correctly
            let ptl = H266ProfileTierLevel::new(1, 0, 51).unwrap(); // Main, Main tier, Level 3.1

            let f = FormatParams {
                h266_profile_tier_level: Some(ptl),
                ..Default::default()
            };

            // Serialize to string
            let fmtp_str = f.to_string();
            assert_eq!(fmtp_str, "profile-id=1;tier-flag=0;level-id=51");

            // Parse back
            let parsed = FormatParams::parse_line(&fmtp_str);
            assert_eq!(parsed.h266_profile_tier_level, Some(ptl));
        }

        /// Test different combinations of H.266 profiles, tiers, and levels to ensure
        /// they are all serialized correctly. Covers Main, Main 10, different tiers, and levels.
        #[test]
        fn h266_different_profiles() {
            // Test Main profile (1)
            let main = H266ProfileTierLevel::new(1, 0, 51).unwrap();
            let f = FormatParams {
                h266_profile_tier_level: Some(main),
                ..Default::default()
            };
            assert_eq!(f.to_string(), "profile-id=1;tier-flag=0;level-id=51");

            // Test Main 10 profile (2)
            let main10 = H266ProfileTierLevel::new(33, 0, 51).unwrap();
            let f = FormatParams {
                h266_profile_tier_level: Some(main10),
                ..Default::default()
            };
            assert_eq!(f.to_string(), "profile-id=33;tier-flag=0;level-id=51");

            // Test different tier (High tier)
            let high_tier = H266ProfileTierLevel::new(1, 1, 51).unwrap();
            let f = FormatParams {
                h266_profile_tier_level: Some(high_tier),
                ..Default::default()
            };
            assert_eq!(f.to_string(), "profile-id=1;tier-flag=1;level-id=51");

            // Test different level (Level 5.1 = 153)
            let level_5_1 = H266ProfileTierLevel::new(1, 0, 83).unwrap();
            let f = FormatParams {
                h266_profile_tier_level: Some(level_5_1),
                ..Default::default()
            };
            assert_eq!(f.to_string(), "profile-id=1;tier-flag=0;level-id=83");
        }

        /// Test parsing H.266 parameters from key-value pairs as they would appear in SDP.
        /// Verifies that the three separate params (profile-id, tier-flag, level-id) are
        /// correctly combined into a single H266ProfileTierLevel composite.
        #[test]
        fn h266_parse_from_pairs() {
            // Test parsing from key-value pairs (as would come from SDP)
            let pairs = vec![
                ("profile-id".to_string(), "1".to_string()),
                ("tier-flag".to_string(), "0".to_string()),
                ("level-id".to_string(), "51".to_string()),
            ];

            let params = FormatParam::parse_pairs(pairs);

            // Should have exactly one H266ProfileTierLevel param
            let h266_params: Vec<_> = params
                .iter()
                .filter(|p| matches!(p, FormatParam::H266ProfileTierLevel(_)))
                .collect();
            assert_eq!(h266_params.len(), 1);

            if let FormatParam::H266ProfileTierLevel(ptl) = h266_params[0] {
                assert_eq!(ptl.profile_id(), 1);
                assert_eq!(ptl.tier_flag(), 0);
                assert_eq!(ptl.level_id(), 51);
            } else {
                panic!("Expected H266ProfileTierLevel");
            }
        }

        /// Test that H.266 profile-tier-level params can coexist with other H.266-specific
        /// parameters like sprop-max-don-diff without interfering with each other.
        #[test]
        fn h266_mixed_with_other_params() {
            // Test H.266 params mixed with other format params
            let pairs = vec![
                ("profile-id".to_string(), "1".to_string()),
                ("tier-flag".to_string(), "0".to_string()),
                ("level-id".to_string(), "51".to_string()),
                ("sprop-max-don-diff".to_string(), "0".to_string()),
            ];

            let params = FormatParam::parse_pairs(pairs);

            // Should have H266ProfileTierLevel and SpropMaxDonDiff
            assert!(
                params
                    .iter()
                    .any(|p| matches!(p, FormatParam::H266ProfileTierLevel(_)))
            );
            assert!(
                params
                    .iter()
                    .any(|p| matches!(p, FormatParam::SpropMaxDonDiff(0)))
            );
        }

        /// Test sprop-max-don-diff parsing, display, and round-trip for various values.
        #[test]
        fn sprop_max_don_diff_parse_and_display() {
            // Parse standalone sprop-max-don-diff
            let param = FormatParam::parse("sprop-max-don-diff", "32");
            assert!(matches!(param, FormatParam::SpropMaxDonDiff(32)));

            // Display format
            assert_eq!(param.to_string(), "sprop-max-don-diff=32");

            // Parse value 0 (DONL disabled)
            let param0 = FormatParam::parse("sprop-max-don-diff", "0");
            assert!(matches!(param0, FormatParam::SpropMaxDonDiff(0)));

            // Parse max valid value (RFC 9328 §7.2: 0..32767)
            let param_max = FormatParam::parse("sprop-max-don-diff", "32767");
            assert!(matches!(param_max, FormatParam::SpropMaxDonDiff(32767)));

            // Out-of-range value (32768) rejected per RFC 9328 §7.2
            let param_over = FormatParam::parse("sprop-max-don-diff", "32768");
            assert!(matches!(param_over, FormatParam::Unknown));

            // Invalid value falls back to Unknown
            let param_bad = FormatParam::parse("sprop-max-don-diff", "notanumber");
            assert!(matches!(param_bad, FormatParam::Unknown));
        }

        /// Test that sprop-max-don-diff > 0 round-trips through H.266 fmtp with PTL params.
        #[test]
        fn h266_sprop_max_don_diff_nonzero_with_ptl() {
            let pairs = vec![
                ("profile-id".to_string(), "1".to_string()),
                ("tier-flag".to_string(), "0".to_string()),
                ("level-id".to_string(), "51".to_string()),
                ("sprop-max-don-diff".to_string(), "32".to_string()),
            ];

            let params = FormatParam::parse_pairs(pairs);

            // Should have both H266ProfileTierLevel and SpropMaxDonDiff(32)
            assert!(
                params
                    .iter()
                    .any(|p| matches!(p, FormatParam::H266ProfileTierLevel(_)))
            );
            assert!(
                params
                    .iter()
                    .any(|p| matches!(p, FormatParam::SpropMaxDonDiff(32)))
            );

            // Verify PTL values are correct
            if let Some(FormatParam::H266ProfileTierLevel(ptl)) = params
                .iter()
                .find(|p| matches!(p, FormatParam::H266ProfileTierLevel(_)))
            {
                assert_eq!(ptl.profile_id(), 1);
                assert_eq!(ptl.tier_flag(), 0);
                assert_eq!(ptl.level_id(), 51);
            }
        }
    }

    /// Tests ensuring H.266 parameters are not confused with other codecs.
    /// Verifies that H.266's three-parameter format (profile-id, tier-flag, level-id)
    /// is correctly distinguished from H.264's profile-level-id, VP9's profile-id,
    /// and AV1's profile/tier/level-idx parameters.
    mod no_confusion_h266 {
        use super::*;

        /// Test that H.264's profile-level-id parameter (single hex value) is not confused
        /// with H.266's three separate parameters. H.264 uses a different format entirely.
        #[test]
        fn h266_no_confusion_with_h264() {
            // H.264 uses profile-level-id (single param)
            let h264_pairs = vec![
                ("profile-level-id".to_string(), "42e01f".to_string()),
                ("packetization-mode".to_string(), "1".to_string()),
            ];

            let h264_params = FormatParam::parse_pairs(h264_pairs);

            // Should NOT have H266ProfileTierLevel
            assert!(
                !h264_params
                    .iter()
                    .any(|p| matches!(p, FormatParam::H266ProfileTierLevel(_)))
            );

            // Should have ProfileLevelId (H.264)
            assert!(
                h264_params
                    .iter()
                    .any(|p| matches!(p, FormatParam::ProfileLevelId(_)))
            );
            assert!(
                h264_params
                    .iter()
                    .any(|p| matches!(p, FormatParam::PacketizationMode(1)))
            );
        }

        /// Test that VP9's profile-id parameter (standalone) is not confused with H.266's
        /// profile-id which must be combined with tier-flag and level-id.
        #[test]
        fn h266_no_confusion_with_vp9() {
            // VP9 uses profile-id (single param, not combined)
            let vp9_pairs = vec![("profile-id".to_string(), "0".to_string())];

            let vp9_params = FormatParam::parse_pairs(vp9_pairs);

            // Should NOT have H266ProfileTierLevel (missing tier-flag and level-id)
            assert!(
                !vp9_params
                    .iter()
                    .any(|p| matches!(p, FormatParam::H266ProfileTierLevel(_)))
            );

            // Should have ProfileId (VP9)
            assert!(
                vp9_params
                    .iter()
                    .any(|p| matches!(p, FormatParam::ProfileId(0)))
            );
        }

        /// Test that AV1's parameters (profile, tier, level-idx) use different naming than
        /// H.266 (profile-id, tier-flag, level-id) and are not confused during parsing.
        #[test]
        fn h266_no_confusion_with_av1() {
            // AV1 uses profile, level-idx, tier (but should not be confused with H.266)
            let av1_pairs = vec![
                ("profile".to_string(), "0".to_string()),
                ("level-idx".to_string(), "5".to_string()),
                ("tier".to_string(), "0".to_string()),
            ];

            let av1_params = FormatParam::parse_pairs(av1_pairs);

            // Should NOT have H266ProfileTierLevel
            // (AV1 uses different param names: profile vs profile-id,
            // tier vs tier-flag, level-idx vs level-id)
            assert!(
                !av1_params
                    .iter()
                    .any(|p| matches!(p, FormatParam::H266ProfileTierLevel(_)))
            );

            // Should have AV1-specific params
            assert!(
                av1_params
                    .iter()
                    .any(|p| matches!(p, FormatParam::Profile(0)))
            );
            assert!(
                av1_params
                    .iter()
                    .any(|p| matches!(p, FormatParam::LevelIdx(5)))
            );
            assert!(av1_params.iter().any(|p| matches!(p, FormatParam::Tier(0))));
        }
    }

    /// Additional H.266 parsing and validation tests.
    /// Covers incomplete parameter handling and complete SDP integration scenarios.
    mod h266_additional {
        use super::*;

        /// Test that incomplete H.266 parameter sets do not create a composite ProfileTierLevel.
        /// All three parameters (profile-id, tier-flag, level-id) must be present.
        #[test]
        fn h266_partial_params_no_composite() {
            // If only some H.266 params are present, should not create composite
            let partial_pairs = vec![
                ("profile-id".to_string(), "1".to_string()),
                ("tier-flag".to_string(), "0".to_string()),
                // Missing level-id
            ];

            let params = FormatParam::parse_pairs(partial_pairs);

            // Should NOT have H266ProfileTierLevel (incomplete)
            assert!(
                !params
                    .iter()
                    .any(|p| matches!(p, FormatParam::H266ProfileTierLevel(_)))
            );
        }

        /// Test H.266 parameters in a complete SDP offer/answer scenario.
        /// Verifies that H.266 rtpmap and fmtp lines are correctly parsed along with RTX.
        #[test]
        fn h266_in_complete_sdp() {
            let input = "v=0\r\n\
            o=- 123456 2 IN IP4 127.0.0.1\r\n\
            s=-\r\n\
            t=0 0\r\n\
            a=group:BUNDLE 0\r\n\
            m=video 9 UDP/TLS/RTP/SAVPF 96 97\r\n\
            c=IN IP4 0.0.0.0\r\n\
            a=mid:0\r\n\
            a=sendrecv\r\n\
            a=rtpmap:96 H266/90000\r\n\
            a=fmtp:96 profile-id=1;tier-flag=0;level-id=51\r\n\
            a=rtpmap:97 rtx/90000\r\n\
            a=fmtp:97 apt=96\r\n\
            a=setup:actpass\r\n\
            a=ice-ufrag:test\r\n\
            a=ice-pwd:testpassword\r\n\
            ";

            let sdp = Sdp::parse(input).expect("should parse");

            // Get RTP params
            let params = sdp.media_lines[0].rtp_params();

            // Find H.266 payload
            let h266_payload = params
                .iter()
                .find(|p| p.spec.codec == Codec::H266)
                .expect("should have H.266");

            assert_eq!(h266_payload.pt, 96.into());
            assert_eq!(h266_payload.spec.clock_rate, Frequency::NINETY_KHZ);

            // Check H.266 params
            let ptl = h266_payload
                .spec
                .format
                .h266_profile_tier_level
                .expect("should have H.266 PTL");
            assert_eq!(ptl.profile_id(), 1);
            assert_eq!(ptl.tier_flag(), 0);
            assert_eq!(ptl.level_id(), 51);

            // Check RTX
            assert_eq!(h266_payload.resend, Some(97.into()));
        }
    }

    /// Multi-codec integration tests.
    /// Verifies that multiple video codecs (H.264, H.266, VP8, VP9) can coexist
    /// in the same SDP without parameter conflicts or interference.
    mod multi_codec_h266 {
        use super::*;

        /// Test that H.264, H.266, and other video codecs can coexist in the same SDP
        /// without their format parameters interfering with each other.
        #[test]
        fn h266_and_h264_coexist() {
            let input = "v=0\r\n\
            o=- 123456 2 IN IP4 127.0.0.1\r\n\
            s=-\r\n\
            t=0 0\r\n\
            a=group:BUNDLE 0\r\n\
            m=video 9 UDP/TLS/RTP/SAVPF 96 97 98\r\n\
            c=IN IP4 0.0.0.0\r\n\
            a=mid:0\r\n\
            a=sendrecv\r\n\
            a=rtpmap:96 H264/90000\r\n\
            a=fmtp:96 profile-level-id=42e01f;packetization-mode=1\r\n\
            a=rtpmap:97 H266/90000\r\n\
            a=fmtp:97 profile-id=1;tier-flag=0;level-id=51\r\n\
            a=rtpmap:98 VP8/90000\r\n\
            a=setup:actpass\r\n\
            a=ice-ufrag:test\r\n\
            a=ice-pwd:testpassword\r\n\
            ";

            let sdp = Sdp::parse(input).expect("should parse");
            let params = sdp.media_lines[0].rtp_params();

            // Check H.264
            let h264 = params
                .iter()
                .find(|p| p.spec.codec == Codec::H264)
                .expect("should have H.264");
            assert_eq!(h264.pt, 96.into());
            assert_eq!(h264.spec.format.profile_level_id, Some(0x42e01f));
            assert_eq!(h264.spec.format.packetization_mode, Some(1));
            assert!(h264.spec.format.h266_profile_tier_level.is_none());

            // Check H.266
            let h266 = params
                .iter()
                .find(|p| p.spec.codec == Codec::H266)
                .expect("should have H.266");
            assert_eq!(h266.pt, 97.into());
            let ptl = h266
                .spec
                .format
                .h266_profile_tier_level
                .expect("should have H.266 PTL");
            assert_eq!(ptl.profile_id(), 1);
            assert!(h266.spec.format.profile_level_id.is_none());

            // Check VP8
            let vp8 = params
                .iter()
                .find(|p| p.spec.codec == Codec::Vp8)
                .expect("should have VP8");
            assert_eq!(vp8.pt, 98.into());
            assert!(vp8.spec.format.h266_profile_tier_level.is_none());
            assert!(vp8.spec.format.profile_level_id.is_none());
        }
    }

    /// H.266 serialization and edge case tests.
    /// Tests media attribute generation, empty parameter handling, extreme values,
    /// parameter ordering independence, DONL parameters, and invalid value handling.
    mod h266_serialization {
        use super::*;

        /// Test that H.266 parameters are correctly serialized as media attributes,
        /// including rtpmap, fmtp with ProfileTierLevel, rtcp-fb, and RTX payload.
        #[test]
        fn h266_serialization_in_media_attrs() {
            let ptl = H266ProfileTierLevel::new(33, 1, 83).unwrap(); // Main 10, High tier, Level 5.1

            let mut payload = PayloadParams::new(
                100.into(),
                Some(101.into()),
                CodecSpec {
                    codec: Codec::H266,
                    clock_rate: Frequency::NINETY_KHZ,
                    channels: None,
                    format: FormatParams {
                        h266_profile_tier_level: Some(ptl),
                        ..Default::default()
                    },
                },
            );
            payload.fb_transport_cc = true;
            payload.fb_nack = true;
            payload.fb_pli = true;

            let mut attrs = vec![];
            payload.as_media_attrs(&mut attrs);

            // Should have rtpmap for H.266
            assert!(attrs.iter().any(|a| matches!(a,
                MediaAttribute::RtpMap { pt, value }
                if *pt == 100.into() && value.codec == Codec::H266
            )));

            // Should have fmtp with H.266 params
            let fmtp = attrs.iter().find(|a| {
                matches!(a,
                    MediaAttribute::Fmtp { pt, .. } if *pt == 100.into()
                )
            });
            assert!(fmtp.is_some());

            if let Some(MediaAttribute::Fmtp { values, .. }) = fmtp {
                assert!(
                    values
                        .iter()
                        .any(|v| matches!(v, FormatParam::H266ProfileTierLevel(p)
                            if p.profile_id() == 33 && p.tier_flag() == 1 && p.level_id() == 83
                        ))
                );
            }

            // Should have rtx mapping
            assert!(attrs.iter().any(|a| matches!(a,
                MediaAttribute::RtpMap { pt, value }
                if *pt == 101.into() && value.codec == Codec::Rtx
            )));
        }

        /// Test that empty format parameters do not generate an a=fmtp line in SDP,
        /// avoiding invalid SDP syntax with empty fmtp attributes.
        #[test]
        fn h266_empty_fmtp_not_serialized() {
            // Empty fmtp should not be written
            let payload = PayloadParams::new(
                100.into(),
                None,
                CodecSpec {
                    codec: Codec::H266,
                    clock_rate: Frequency::NINETY_KHZ,
                    channels: None,
                    format: FormatParams::default(),
                },
            );

            let mut attrs = vec![];
            payload.as_media_attrs(&mut attrs);

            // Should NOT have fmtp line (it would be empty)
            assert!(
                !attrs
                    .iter()
                    .any(|a| matches!(a, MediaAttribute::Fmtp { .. }))
            );
        }

        /// Test H.266 with extreme profile/tier/level values at boundaries.
        /// Verifies parsing and serialization of edge case values.
        #[test]
        fn h266_extreme_values() {
            // Test minimum level (Level 1.0 = 30)
            let min_level = H266ProfileTierLevel::new(1, 0, 16).unwrap();
            let f = FormatParams {
                h266_profile_tier_level: Some(min_level),
                ..Default::default()
            };
            assert_eq!(f.to_string(), "profile-id=1;tier-flag=0;level-id=16");

            // Test maximum level (Level 6.2 = 186)
            let max_level = H266ProfileTierLevel::new(1, 0, 102).unwrap();
            let f = FormatParams {
                h266_profile_tier_level: Some(max_level),
                ..Default::default()
            };
            assert_eq!(f.to_string(), "profile-id=1;tier-flag=0;level-id=102");

            // Test Main10 with High tier
            let main10_high = H266ProfileTierLevel::new(33, 1, 51).unwrap();
            let f = FormatParams {
                h266_profile_tier_level: Some(main10_high),
                ..Default::default()
            };
            assert_eq!(f.to_string(), "profile-id=33;tier-flag=1;level-id=51");
        }

        /// Test H.266 parameter ordering doesn't affect parsing.
        /// SDP parameters can appear in any order.
        #[test]
        fn h266_parameter_order_independence() {
            // Normal order
            let normal_order = vec![
                ("profile-id".to_string(), "1".to_string()),
                ("tier-flag".to_string(), "0".to_string()),
                ("level-id".to_string(), "51".to_string()),
            ];

            // Reversed order
            let reversed_order = vec![
                ("level-id".to_string(), "51".to_string()),
                ("tier-flag".to_string(), "0".to_string()),
                ("profile-id".to_string(), "1".to_string()),
            ];

            // Mixed order
            let mixed_order = vec![
                ("tier-flag".to_string(), "0".to_string()),
                ("profile-id".to_string(), "1".to_string()),
                ("level-id".to_string(), "51".to_string()),
            ];

            let normal_params = FormatParam::parse_pairs(normal_order);
            let reversed_params = FormatParam::parse_pairs(reversed_order);
            let mixed_params = FormatParam::parse_pairs(mixed_order);

            // All should create H266ProfileTierLevel
            assert!(
                normal_params
                    .iter()
                    .any(|p| matches!(p, FormatParam::H266ProfileTierLevel(_)))
            );
            assert!(
                reversed_params
                    .iter()
                    .any(|p| matches!(p, FormatParam::H266ProfileTierLevel(_)))
            );
            assert!(
                mixed_params
                    .iter()
                    .any(|p| matches!(p, FormatParam::H266ProfileTierLevel(_)))
            );

            // All should have the same values
            for params in [&normal_params, &reversed_params, &mixed_params] {
                if let Some(FormatParam::H266ProfileTierLevel(ptl)) = params
                    .iter()
                    .find(|p| matches!(p, FormatParam::H266ProfileTierLevel(_)))
                {
                    assert_eq!(ptl.profile_id(), 1);
                    assert_eq!(ptl.tier_flag(), 0);
                    assert_eq!(ptl.level_id(), 51);
                }
            }
        }

        #[test]
        fn h266_invalid_parameter_values() {
            // Invalid profile-id
            let invalid_profile = vec![
                ("profile-id".to_string(), "999".to_string()),
                ("tier-flag".to_string(), "0".to_string()),
                ("level-id".to_string(), "51".to_string()),
            ];
            let params = FormatParam::parse_pairs(invalid_profile);
            assert!(
                !params
                    .iter()
                    .any(|p| matches!(p, FormatParam::H266ProfileTierLevel(_)))
            );

            // Invalid tier-flag
            let invalid_tier = vec![
                ("profile-id".to_string(), "1".to_string()),
                ("tier-flag".to_string(), "5".to_string()),
                ("level-id".to_string(), "51".to_string()),
            ];
            let params = FormatParam::parse_pairs(invalid_tier);
            assert!(
                !params
                    .iter()
                    .any(|p| matches!(p, FormatParam::H266ProfileTierLevel(_)))
            );

            // Invalid level-id
            let invalid_level = vec![
                ("profile-id".to_string(), "1".to_string()),
                ("tier-flag".to_string(), "0".to_string()),
                ("level-id".to_string(), "999".to_string()),
            ];
            let params = FormatParam::parse_pairs(invalid_level);
            assert!(
                !params
                    .iter()
                    .any(|p| matches!(p, FormatParam::H266ProfileTierLevel(_)))
            );
        }
    }

    /// H.266 integration with RTCP feedback mechanisms.
    /// Tests H.266 behavior with multiple codecs and various RTCP feedback types
    /// including NACK, PLI, FIR, and transport-cc.
    mod h266_integration {
        use super::*;

        /// Test H.266 in SDP with multiple video codecs and feedback mechanisms.
        /// Verifies proper parsing of complex multi-codec scenarios.
        #[test]
        fn h266_multi_codec_with_feedback() {
            let input = "v=0\r\n\
            o=- 123456 2 IN IP4 127.0.0.1\r\n\
            s=-\r\n\
            t=0 0\r\n\
            a=group:BUNDLE 0\r\n\
            m=video 9 UDP/TLS/RTP/SAVPF 96 97 98 99\r\n\
            c=IN IP4 0.0.0.0\r\n\
            a=mid:0\r\n\
            a=sendrecv\r\n\
            a=rtpmap:96 H266/90000\r\n\
            a=fmtp:96 profile-id=1;tier-flag=0;level-id=51\r\n\
            a=rtcp-fb:96 nack\r\n\
            a=rtcp-fb:96 nack pli\r\n\
            a=rtcp-fb:96 ccm fir\r\n\
            a=rtcp-fb:96 transport-cc\r\n\
            a=rtpmap:97 rtx/90000\r\n\
            a=fmtp:97 apt=96\r\n\
            a=rtpmap:98 VP9/90000\r\n\
            a=fmtp:98 profile-id=0\r\n\
            a=rtpmap:99 rtx/90000\r\n\
            a=fmtp:99 apt=98\r\n\
            a=setup:actpass\r\n\
            a=ice-ufrag:test\r\n\
            a=ice-pwd:testpassword\r\n\
            ";

            let sdp = Sdp::parse(input).expect("should parse");
            let params = sdp.media_lines[0].rtp_params();

            // Check H.266 with all feedback mechanisms
            let h266 = params
                .iter()
                .find(|p| p.spec.codec == Codec::H266)
                .expect("should have H.266");
            assert_eq!(h266.pt, 96.into());
            assert!(h266.fb_nack);
            assert!(h266.fb_pli);
            assert!(h266.fb_fir);
            assert!(h266.fb_transport_cc);
            assert_eq!(h266.resend, Some(97.into()));

            // Verify H.266 PTL is correct
            let ptl = h266
                .spec
                .format
                .h266_profile_tier_level
                .expect("should have PTL");
            assert_eq!(ptl.profile_id(), 1);

            // Check VP9 is separate and correct
            let vp9 = params
                .iter()
                .find(|p| p.spec.codec == Codec::Vp9)
                .expect("should have VP9");
            assert_eq!(vp9.pt, 98.into());
            assert_eq!(vp9.spec.format.profile_id, Some(0));
            assert!(vp9.spec.format.h266_profile_tier_level.is_none());
        }
    }

    /// Advanced H.266 functionality tests.
    /// Covers SDP round-trip serialization, incomplete parameter permutations,
    /// browser compatibility (Chrome profile-only mode), and validation of all
    /// standard H.266 level values (1.0 through 6.2).
    mod h266_advanced {
        use super::*;

        /// Test H.266 SDP serialization maintains parameter integrity.
        /// Ensures that serialized SDP can be parsed back with identical values.
        #[test]
        fn h266_sdp_serialization_round_trip() {
            let ptl = H266ProfileTierLevel::new(33, 0, 83).unwrap();

            let payload = PayloadParams::new(
                100.into(),
                Some(101.into()),
                CodecSpec {
                    codec: Codec::H266,
                    clock_rate: Frequency::NINETY_KHZ,
                    channels: None,
                    format: FormatParams {
                        h266_profile_tier_level: Some(ptl),
                        ..Default::default()
                    },
                },
            );

            let mut attrs = vec![];
            payload.as_media_attrs(&mut attrs);

            // Find the fmtp line
            let fmtp = attrs
                .iter()
                .find_map(|a| match a {
                    MediaAttribute::Fmtp { pt, values } if *pt == 100.into() => Some(values),
                    _ => None,
                })
                .expect("should have fmtp");

            // Should contain H266ProfileTierLevel
            assert!(fmtp.iter().any(
                |v| matches!(v, FormatParam::H266ProfileTierLevel(p) if p.profile_id() == 33)
            ));
        }

        /// Test that only two of three H.266 params doesn't create PTL.
        /// Each permutation of missing parameter should fail.
        #[test]
        fn h266_incomplete_params_all_permutations() {
            // Missing level-id
            let missing_level = vec![
                ("profile-id".to_string(), "1".to_string()),
                ("tier-flag".to_string(), "0".to_string()),
            ];
            let params = FormatParam::parse_pairs(missing_level);
            assert!(
                !params
                    .iter()
                    .any(|p| matches!(p, FormatParam::H266ProfileTierLevel(_)))
            );

            // Missing tier-flag
            let missing_tier = vec![
                ("profile-id".to_string(), "1".to_string()),
                ("level-id".to_string(), "51".to_string()),
            ];
            let params = FormatParam::parse_pairs(missing_tier);
            assert!(
                !params
                    .iter()
                    .any(|p| matches!(p, FormatParam::H266ProfileTierLevel(_)))
            );

            // Missing profile-id
            let missing_profile = vec![
                ("tier-flag".to_string(), "0".to_string()),
                ("level-id".to_string(), "51".to_string()),
            ];
            let params = FormatParam::parse_pairs(missing_profile);
            assert!(
                !params
                    .iter()
                    .any(|p| matches!(p, FormatParam::H266ProfileTierLevel(_)))
            );

            // Only one parameter
            let only_profile = vec![("profile-id".to_string(), "1".to_string())];
            let params = FormatParam::parse_pairs(only_profile);
            assert!(
                !params
                    .iter()
                    .any(|p| matches!(p, FormatParam::H266ProfileTierLevel(_)))
            );
        }

        /// Test H.266 with Chrome-style profile-only in complete SDP.
        /// Verifies that browser compatibility mode works in real SDP.
        #[test]
        fn h266_chrome_profile_only_sdp() {
            let input = "v=0\r\n\
            o=- 123456 2 IN IP4 127.0.0.1\r\n\
            s=-\r\n\
            t=0 0\r\n\
            a=group:BUNDLE 0\r\n\
            m=video 9 UDP/TLS/RTP/SAVPF 96\r\n\
            c=IN IP4 0.0.0.0\r\n\
            a=mid:0\r\n\
            a=sendrecv\r\n\
            a=rtpmap:96 H266/90000\r\n\
            a=fmtp:96 profile-id=1\r\n\
            a=setup:actpass\r\n\
            a=ice-ufrag:test\r\n\
            a=ice-pwd:testpassword\r\n\
            ";

            let sdp = Sdp::parse(input).expect("should parse");
            let params = sdp.media_lines[0].rtp_params();

            let h266 = params
                .iter()
                .find(|p| p.spec.codec == Codec::H266)
                .expect("should have H.266");

            // Should NOT have full PTL
            assert!(h266.spec.format.h266_profile_tier_level.is_none());

            // Should have profile_id field instead
            assert_eq!(h266.spec.format.profile_id, Some(1));
        }

        /// Test H.266 different level values in valid range.
        /// Ensures all valid level IDs are properly handled.
        #[test]
        fn h266_all_valid_levels() {
            let valid_levels = vec![
                16,  // Level 1.0
                32,  // Level 2.0
                35,  // Level 2.1
                48,  // Level 3.0
                51,  // Level 3.1
                64,  // Level 4.0
                67,  // Level 4.1
                80,  // Level 5.0
                83,  // Level 5.1
                86,  // Level 5.2
                96,  // Level 6.0
                99,  // Level 6.1
                102, // Level 6.2
            ];

            for level_id in valid_levels {
                let ptl = H266ProfileTierLevel::new(1, 0, level_id);
                assert!(ptl.is_some(), "Level ID {} should be valid", level_id);

                let f = FormatParams {
                    h266_profile_tier_level: ptl,
                    ..Default::default()
                };

                let serialized = f.to_string();
                assert!(serialized.contains(&format!("level-id={}", level_id)));
            }
        }
    }
}
