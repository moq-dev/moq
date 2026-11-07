use super::Vp9CodecExtra;
use super::contiguity::FrameContiguityState;

#[derive(Debug, Default)]
pub struct Vp9Contiguity {
    /// last picture id of layer 0 that we allowed emitting
    last_tl0_picture_id: Option<u64>,

    /// last picture id of any layer that we allowed emitting
    last_picture_id: Option<u64>,
}

impl Vp9Contiguity {
    pub fn new() -> Self {
        Self::default()
    }

    /// Called when depacketizing a new frame is ready to be emitted
    ///
    /// Returns whether we can emit such frame and whether it's decodable with contiguity
    pub fn check(&mut self, next: &Vp9CodecExtra, contiguous_seq: bool) -> (bool, bool) {
        let mut frame_state: FrameContiguityState = self.into();
        let res = frame_state.next_frame(
            next.picture_id,
            next.tl0_picture_id,
            next.tid.map(Into::into),
            next.is_keyframe,
            contiguous_seq,
        );

        *self = frame_state.into();
        res
    }
}

impl From<&mut Vp9Contiguity> for FrameContiguityState {
    fn from(value: &mut Vp9Contiguity) -> Self {
        Self {
            last_picture_id: value.last_picture_id,
            last_tl0_picture_id: value.last_tl0_picture_id,
        }
    }
}

impl From<FrameContiguityState> for Vp9Contiguity {
    fn from(value: FrameContiguityState) -> Self {
        Self {
            last_picture_id: value.last_picture_id,
            last_tl0_picture_id: value.last_tl0_picture_id,
        }
    }
}

#[cfg(test)]
mod test {
    use crate::packet::Vp9CodecExtra;

    use super::Vp9Contiguity;

    const L1T3_LAYERS: &[u64] = &[0, 2, 1, 2];
    const L1T2_LAYERS: &[u64] = &[0, 1, 0, 1];

    fn get_codec_extra(pid: u16, tid: u8, tl0_picture_id: u8) -> Vp9CodecExtra {
        Vp9CodecExtra {
            picture_id: Some(pid.into()),
            tid: Some(tid),
            tl0_picture_id: Some(tl0_picture_id.into()),
            ..Default::default()
        }
    }

    #[test]
    fn contiguous_l1t3() {
        let mut contiguity = Vp9Contiguity::new();

        for i in 0..100 {
            let next = get_codec_extra(i, L1T3_LAYERS[i as usize % 4] as u8, (i / 4) as u8);

            let res = contiguity.check(&next, true);
            assert_eq!(res, (true, true), "Failure at picture {} {:?}", i, next);
        }
    }

    #[test]
    fn contiguous_l1t3_no_l2_contig_l0() {
        const L1T3_LAYERS: &[u64] = &[0, 2, 1, 2];

        let mut contiguity = Vp9Contiguity::new();

        for i in 0..100 {
            let layer_index = L1T3_LAYERS[i as usize % 4] as u8;
            if layer_index == 2 {
                continue;
            }

            let next = get_codec_extra(i, layer_index, (i / 4) as u8);
            let (emit, contiguous) = contiguity.check(&next, true);

            // all layer 0 are contiguous therefore no discontinuity
            assert_eq!(emit, next.tid == Some(0));
            assert!(contiguous);

            if layer_index == 1 {
                // frames on layer 1 are not emitted (could be improved tracking more deps)
                // and we don't raise a discontinuity (stream still decodable)
                assert!(!emit);
                assert!(contiguous);
            }
        }
    }

    #[test]
    fn contiguous_l1t3_no_l2_discontig_l0() {
        const L1T3_LAYERS: &[u64] = &[0, 2, 1, 2];

        let mut contiguity = Vp9Contiguity::new();

        for i in 0..100 {
            let layer_index = L1T3_LAYERS[i as usize % 4] as u8;
            if layer_index == 2 {
                continue;
            }

            let next = get_codec_extra(i, layer_index, i as u8);
            let (emit, _) = contiguity.check(&next, true);

            assert!(emit == (next.tid == Some(0)));
        }
    }

    #[test]
    fn contiguous_l1t2() {
        let mut contiguity = Vp9Contiguity::new();

        for i in 0..100 {
            let next = get_codec_extra(i, L1T2_LAYERS[i as usize % 4] as u8, (i / 2) as u8);

            let res = contiguity.check(&next, true);
            assert_eq!(res, (true, true), "Failure at picture {} {:?}", i, next);
        }
    }

    #[test]
    fn contiguous_l1t1_no_l1_contig_l0() {
        let mut contiguity = Vp9Contiguity::new();

        for i in 0..100 {
            let layer_index = L1T2_LAYERS[i as usize % 4] as u8;
            if layer_index == 1 {
                continue;
            }

            let next = get_codec_extra(i, layer_index, (i / 2) as u8);
            let (emit, _) = contiguity.check(&next, true);

            // all layer 0 are contiguous therefore can be emitted
            assert_eq!(emit, next.tid == Some(0));
        }
    }

    #[test]
    fn keyframe_accepts_restarted_picture_ids() {
        let mut contiguity = Vp9Contiguity::new();
        let mut first = get_codec_extra(40, 0, 40);
        first.is_keyframe = true;
        assert_eq!(contiguity.check(&first, true), (true, true));

        let mut restarted = get_codec_extra(1, 0, 1);
        restarted.is_keyframe = true;
        assert_eq!(contiguity.check(&restarted, true), (true, false));
        assert_eq!(contiguity.check(&restarted, true), (true, false));
        assert_eq!(
            contiguity.check(&get_codec_extra(2, 0, 2), true),
            (true, true)
        );
    }
}
