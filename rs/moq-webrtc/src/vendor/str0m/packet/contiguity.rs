use super::CodecExtra;
use super::contiguity_vp8::Vp8Contiguity;
use super::contiguity_vp9::Vp9Contiguity;

#[derive(Debug)]
pub enum Contiguity {
    Vp8(Vp8Contiguity),
    Vp9(Vp9Contiguity),
    None,
}

impl Contiguity {
    pub fn check(&mut self, next: &CodecExtra, contiguous_seq: bool) -> (bool, bool) {
        match (self, next) {
            (Self::Vp8(contiguity), CodecExtra::Vp8(next)) => {
                contiguity.check(next, contiguous_seq)
            }
            (Self::Vp9(contiguity), CodecExtra::Vp9(next)) => {
                contiguity.check(next, contiguous_seq)
            }
            (Self::Vp8(_) | Self::Vp9(_) | Self::None, _) => (true, contiguous_seq),
        }
    }
}

#[derive(Debug)]
pub(crate) struct FrameContiguityState {
    /// last picture id of layer 0 that we allowed emitting
    pub(crate) last_picture_id: Option<u64>,

    /// last picture id of any layer that we allowed emitting
    pub(crate) last_tl0_picture_id: Option<u64>,
}

impl FrameContiguityState {
    /// Returns whether we can emit such frame and whether it's decodable with contiguity
    pub(crate) fn next_frame(
        &mut self,
        picture_id: Option<u64>,
        tl0_picture_id: Option<u64>,
        layer_index: Option<u64>,
        is_keyframe: bool,
        contiguous_seq: bool,
    ) -> (bool, bool) {
        let Some(picture_id) = picture_id else {
            // picture id is not enabled or not progressing anyway
            return (true, contiguous_seq);
        };

        let Some(tl0_picture_id) = tl0_picture_id else {
            return (true, contiguous_seq);
        };

        let (Some(last_tl0_picture_id), Some(last_picture_id)) =
            (self.last_tl0_picture_id, self.last_picture_id)
        else {
            self.last_tl0_picture_id = Some(tl0_picture_id);
            self.last_picture_id = Some(picture_id);
            return (true, true);
        };

        // A new encoder can keep the SSRC while restarting picture IDs. A keyframe
        // is independently decodable, so it can establish a fresh baseline.
        if is_keyframe && picture_id <= last_picture_id {
            self.last_tl0_picture_id = Some(tl0_picture_id);
            self.last_picture_id = Some(picture_id);
            return (true, false);
        }

        // discard older pictures if any
        if picture_id <= last_picture_id {
            return (false, true);
        }

        if layer_index == Some(0) {
            if tl0_picture_id == last_tl0_picture_id {
                warn!(
                    "VP8 or VP9: 2 subsequent frames on layer zero must \
                    have different tl0 picture id: encoding problem?"
                );
            }

            // Frame on layer 0: always emit and report discontinuity if not subsequent
            let emit = true;
            // note: we use wrapping add because the encoder can restart if the
            // camera is closed / reopened and here we only care about
            // contiguity
            let contiguous = tl0_picture_id == last_tl0_picture_id.wrapping_add(1);

            self.last_tl0_picture_id = Some(tl0_picture_id);
            self.last_picture_id = Some(picture_id);

            return (emit, contiguous);
        }

        // Frame on layer 1 or 2: only emit if they refer to the current layer 0
        // and they are subsequent
        let emit =
            tl0_picture_id == last_tl0_picture_id && picture_id == last_picture_id.wrapping_add(1);

        if emit {
            self.last_picture_id = Some(picture_id);
            if !contiguous_seq {
                // this as happened in Safari + very lossy network (very rare)
                debug!(
                    "VP8 or VP9: contiguous pictures implies contiguous seq numbers: encoding issue ?"
                )
            }
        }

        (emit, true)
    }
}
