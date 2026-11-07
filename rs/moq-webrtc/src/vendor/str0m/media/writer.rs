use std::sync::Arc;
use std::time::Instant;

use crate::RtcError;
use crate::format::{CodecConfig, PayloadParams};
use crate::packet::PacketError;
use crate::poll::Readiness;
use crate::rtp_::AbsCaptureTime;
use crate::rtp_::MidRid;
use crate::rtp_::VideoOrientation;
use crate::session::Session;
use crate::streams::Streams;

use super::TelephoneEvent;
use super::tele::{MAX_DURATION, MIN_DURATION, TelephonePackets};
use super::{ExtensionValues, KeyframeRequestKind, Media, MediaTime, Mid, Pt, Rid, ToPayload};

/// Writer of frame level data.
///
/// Obtained via [`Rtc::writer`][crate::Rtc::writer].
///
/// This is the Frame Level API. For RTP level see
/// [`DirectApi::stream_tx`][crate::change::DirectApi::stream_tx].
pub struct Writer<'a> {
    session: &'a mut Session,
    readiness: &'a mut Readiness,
    mid: Mid,
    rid: Option<Rid>,
    start_of_talkspurt: Option<bool>,
    tele_event: Option<TelephoneEvent>,
    ext_vals: ExtensionValues,
}

impl<'a> Writer<'a> {
    /// Create a new writer object.
    ///
    /// The `mid` parameter is required to have a corresponding media in `self.session`.
    ///
    /// Mutations arm readiness within the method so read-only access leaves it
    /// intact and no persistent drop guard extends the caller's borrow.
    pub(crate) fn new(session: &'a mut Session, readiness: &'a mut Readiness, mid: Mid) -> Self {
        Writer {
            session,
            readiness,
            mid,
            rid: None,
            start_of_talkspurt: None,
            tele_event: None,
            ext_vals: ExtensionValues::default(),
        }
    }

    /// Get the configured payload parameters for the `mid` this writer is for.
    ///
    /// For the [`Writer::write()`] call, the `pt` must be set correctly.
    pub fn payload_params(&self) -> impl Iterator<Item = &PayloadParams> {
        // This unwrap is OK due to the invariant of self.mid being resolvable
        let media = self.session.media_by_mid(self.mid).unwrap();
        self.session
            .codec_config
            .params()
            .iter()
            .filter(|p| media.remote_pts().contains(&p.pt))
    }

    /// Match the given parameters to the configured parameters for this [`Media`].
    ///
    /// In a server scenario, a certain codec configuration might not have the same
    /// payload type (PT) for two different peers. We will have incoming data with one
    /// PT and need to match that against the PT of the outgoing [`Media`].
    ///
    /// This call performs matching and if a match is found, returns the _local_ PT
    /// that can be used for sending media.
    pub fn match_params(&self, params: PayloadParams) -> Option<Pt> {
        self.session
            .codec_config
            .match_params(params)
            .map(|p| p.pt())
    }

    /// Add on an Rtp Stream Id. This is typically used to separate simulcast layers.
    pub fn rid(mut self, rid: Rid) -> Self {
        self.rid = Some(rid);
        self
    }

    /// Add on audio level and voice activity. These values are communicated in the same
    /// RTP header extension, hence it makes sense setting both at the same time.
    ///
    /// Audio level is measured in negative decibel. 0 is max and a "normal" value might be -30.
    pub fn audio_level(mut self, audio_level: i8, voice_activity: bool) -> Self {
        self.ext_vals.audio_level = Some(audio_level);
        self.ext_vals.voice_activity = Some(voice_activity);
        self
    }

    /// First packet of a talkspurt, that is the first packet after a silence period during
    /// which packets have not been transmitted contiguously.
    ///
    /// For audio only when dtx or silence suppression is enabled.
    /// This will set the marker bit in the RTP header.
    pub fn start_of_talkspurt(mut self, start_of_talkspurt: bool) -> Self {
        self.start_of_talkspurt = Some(start_of_talkspurt);
        self
    }

    /// Add video orientation. This can be used by a player on the receiver end to decide
    /// whether the video requires to be rotated to show correctly.
    pub fn video_orientation(mut self, o: VideoOrientation) -> Self {
        self.ext_vals.video_orientation = Some(o);
        self
    }

    /// Set absolute capture time for this frame.
    pub fn abs_capture_time(mut self, capture_time: AbsCaptureTime) -> Self {
        self.ext_vals.abs_capture_time = Some(capture_time);
        self
    }

    /// Start a telephone event alongside the next audio write.
    ///
    /// `event` describes the complete tone: set `end` to true and `duration` to its total length.
    /// The `write` call supplies its start time and audio payload type. The negotiated
    /// telephone-event payload type with the same RTP clock rate is selected automatically.
    /// The complete series of telephone packets is queued by this write. Each packet waits until
    /// its send time, while later audio writes can pass packets still waiting in the queue.
    /// Passing an empty audio slice to `write` sends the telephone packets without an audio packet.
    ///
    /// Only one event can be active on this media. Start the next one on a later write, at least
    /// 50 ms after the previous event ends.
    ///
    /// Panics if called twice before `write`. Use a separate `write` for each event.
    pub fn telephone_event(mut self, event: TelephoneEvent) -> Self {
        assert!(
            self.tele_event.is_none(),
            "telephone_event was already set; call write before setting another event"
        );
        self.tele_event = Some(event);
        self
    }

    /// Set the minimum and maximum playout delay values. This can be used by a player
    /// on the receiver end to determine the size of the jitter buffer.
    pub fn playout_delay(mut self, min: MediaTime, max: MediaTime) -> Self {
        self.ext_vals.play_delay_min = Some(min);
        self.ext_vals.play_delay_max = Some(max);
        self
    }

    /// Set a user extension value.
    pub fn user_extension_value<T: Send + Sync + 'static>(mut self, val: T) -> Self {
        self.ext_vals.user_values.set(val);
        self
    }

    /// Write media.
    ///
    /// This operation fails if the PT doesn't match a negotiated codec, or the RID (`None` or a value)
    /// does not match anything negotiated.
    ///
    /// Regarding `wallclock` and `rtp_time`, the wallclock is the real world time that corresponds to
    /// the `MediaTime`. For an SFU, this can be hard to know, since RTP packets typically only
    /// contain the media time (RTP time). In the simplest SFU setup, the wallclock could simply
    /// be the arrival time of the incoming RTP data (see
    /// [`MediaData::network_time`][crate::media::MediaData]). For better synchronization the SFU
    /// probably needs to weigh in clock drifts and data provided via the statistics.
    ///
    /// If you write media before `IceConnectionState` is `Connected` it will be dropped.
    ///
    /// Panics if [`RtcConfig::set_rtp_mode()`][crate::RtcConfig::set_rtp_mode] is `true`.
    pub fn write(
        self,
        pt: Pt,
        wallclock: Instant,
        rtp_time: MediaTime,
        data: impl Into<Arc<[u8]>>,
    ) -> Result<(), RtcError> {
        let _wake = self.readiness.wake();

        if !self.session.codec_config.has_pt(pt) {
            return Err(RtcError::UnknownPt(pt));
        }
        let params = self
            .session
            .codec_config
            .params()
            .iter()
            .find(|p| p.pt() == pt)
            .expect("configured payload type exists");
        let is_audio = params.spec().codec.is_audio();
        if self.tele_event.is_some() && !is_audio {
            return Err(RtcError::UnknownPt(pt));
        }
        let clock_rate = params.spec().rtp_clock_rate();

        // This (indirect) unwrap is OK due to the invariant of self.mid being resolvable
        let media = media_by_mid_mut(&mut self.session.medias, self.mid);

        if let Some(rid) = self.rid {
            if !media.rids_tx().contains(rid) {
                return Err(RtcError::UnknownRid(rid));
            }
        }

        let data: Arc<[u8]> = data.into();

        trace!(
            "write {:?} {:?} {:?} time: {:?} len: {}",
            self.mid,
            self.rid,
            pt,
            rtp_time,
            data.len()
        );

        let has_data = !data.is_empty();
        let has_tele = self.tele_event.is_some();

        // If the user sends an empty slice, we honor it in case there is not
        // tele event. In case of tele event we assume the user just wants to send
        // the telephone events and nothing else.
        if has_data || !has_tele {
            media.set_to_payload(ToPayload {
                pt,
                rid: self.rid,
                wallclock,
                rtp_time,
                not_before: None,
                data,
                start_of_talk_spurt: self.start_of_talkspurt.unwrap_or(false),
                ext_vals: self.ext_vals.clone(),
            })?;
        }

        let Some(event) = self.tele_event else {
            return Ok(());
        };

        let event_pt = validate_tele_event(
            media,
            &self.session.codec_config,
            &mut self.session.streams,
            self.rid,
            params,
            wallclock,
            event,
        )?;

        // Iterator over the packets created by the event.
        let packets = TelephonePackets::new(
            event_pt,
            self.rid,
            event,
            wallclock,
            rtp_time.rebase(clock_rate),
            self.ext_vals,
        );

        media.queue_telephone_packets(packets, wallclock + event.duration)?;

        Ok(())
    }

    /// Test if the kind of keyframe request is possible.
    ///
    /// Sending a keyframe request requires the mechanic to be negotiated as a feedback mechanic
    /// in the SDP offer/answer dance first.
    ///
    /// Specifically these SDP lines would enable FIR and PLI respectively (for payload type 96).
    ///
    /// ```text
    /// a=rtcp-fb:96 ccm fir
    /// a=rtcp-fb:96 nack pli
    /// ```
    pub fn is_request_keyframe_possible(&self, kind: KeyframeRequestKind) -> bool {
        self.session.is_request_keyframe_possible(kind)
    }

    /// Request a keyframe from a remote peer sending media data.
    ///
    /// For SDP: This can fail if the kind of request (PLI or FIR), as specified by the
    /// [`KeyframeRequestKind`], is not negotiated in the SDP answer/offer for this m-line.
    ///
    /// To ensure the call will not fail, use [`Writer::is_request_keyframe_possible()`] to
    /// check whether the feedback mechanism is enabled.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use std::time::Instant;
    /// # use str0m::Rtc;
    /// # use str0m::media::{Mid, KeyframeRequestKind};
    /// let mut rtc = Rtc::new(Instant::now());
    ///
    /// // add candidates, do SDP negotiation
    /// let mid: Mid = todo!(); // obtain mid from Event::MediaAdded.
    ///
    /// let writer = rtc.writer(mid).unwrap();
    ///
    /// writer.request_keyframe(None, KeyframeRequestKind::Pli).unwrap();
    /// ```
    pub fn request_keyframe(
        &mut self,
        rid: Option<Rid>,
        kind: KeyframeRequestKind,
    ) -> Result<(), RtcError> {
        if !self.is_request_keyframe_possible(kind) {
            return Err(RtcError::NotReceivingDirection);
        }

        // Requests become RTCP feedback on the next timeout, so arm both signals.
        let _wake = self.readiness.wake();
        let midrid = MidRid(self.mid, rid);

        let stream = self
            .session
            .streams
            .stream_rx_by_midrid(midrid)
            .ok_or(RtcError::NoReceiverSource(rid))?;

        stream.request_keyframe(kind);

        Ok(())
    }
}

fn validate_tele_event(
    media: &Media,
    codec_config: &CodecConfig,
    streams: &mut Streams,
    rid: Option<Rid>,
    audio_params: &PayloadParams,
    wallclock: Instant,
    event: TelephoneEvent,
) -> Result<Pt, RtcError> {
    let mid = media.mid();
    let audio_pt = audio_params.pt();
    let clock_rate = audio_params.spec().rtp_clock_rate();
    let event_params = codec_config.params().iter().find(|p| {
        p.spec().codec.is_tele()
            && p.spec().rtp_clock_rate() == clock_rate
            && (media.remote_pts().is_empty() || media.remote_pts().contains(&p.pt()))
    });
    let Some(event_params) = event_params else {
        let reason = "no negotiated telephone event for audio clock rate";
        return Err(tele_invalid(mid, audio_pt, reason));
    };
    let event_pt = event_params.pt();
    let max = codec_config.tele_event_max(event_pt).unwrap();
    if event.event > max {
        return Err(tele_invalid(
            mid,
            event_pt,
            "event exceeds negotiated range",
        ));
    }
    if !event.end {
        return Err(tele_invalid(mid, event_pt, "event must have end set"));
    }
    if event.volume > 63 {
        return Err(tele_invalid(mid, event_pt, "volume exceeds 63"));
    }
    if !(MIN_DURATION..=MAX_DURATION).contains(&event.duration) {
        let reason = "duration must be between 40 ms and 6 s";
        return Err(tele_invalid(mid, event_pt, reason));
    }
    if !media.direction().is_sending() {
        return Err(RtcError::NotSendingDirection(media.direction()));
    }
    if let Some(rid) = rid {
        if !media.rids_tx().contains(rid) {
            return Err(RtcError::UnknownRid(rid));
        }
    }
    let midrid = MidRid(mid, rid);
    let sender_missing = streams.stream_tx_by_midrid(midrid).is_none();
    if sender_missing {
        return Err(RtcError::NoSenderSource);
    }
    let too_soon = media.telephone_event_too_soon(wallclock);
    if too_soon {
        let reason = "telephone events must be at least 50 ms apart";
        return Err(tele_invalid(mid, event_pt, reason));
    }
    Ok(event_pt)
}

fn tele_invalid(mid: Mid, pt: Pt, reason: &'static str) -> RtcError {
    RtcError::Packet(mid, pt, PacketError::InvalidTelephoneEvent(reason))
}

/// Get a &mut Media in a slice for a `mid`.
///
/// `mid` must be resolvable or panic will ensue.
fn media_by_mid_mut(medias: &mut [Media], mid: Mid) -> &mut Media {
    medias.iter_mut().find(|m| m.mid() == mid).unwrap()
}
