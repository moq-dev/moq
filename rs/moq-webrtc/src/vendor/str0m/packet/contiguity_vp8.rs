use super::Vp8CodecExtra;
use super::contiguity::FrameContiguityState;

#[derive(Debug, Default)]
pub struct Vp8Contiguity {
    /// last picture id of layer 0 that we allowed emitting
    last_tl0_picture_id: Option<u64>,
    /// last picture id of any layer that we allowed emitting
    last_picture_id: Option<u64>,
}

impl Vp8Contiguity {
    pub fn new() -> Self {
        Self::default()
    }

    /// Called when depacketizing a new frame is ready to be emitted
    ///
    /// Returns whether we can emit such frame and whether it's decodable with contiguity
    pub fn check(&mut self, next: &Vp8CodecExtra, contiguous_seq: bool) -> (bool, bool) {
        let mut frame_state: FrameContiguityState = self.into();
        let res = frame_state.next_frame(
            next.picture_id,
            next.tl0_picture_id,
            Some(next.layer_index.into()),
            next.is_keyframe,
            contiguous_seq,
        );

        *self = frame_state.into();
        res
    }
}

impl From<&mut Vp8Contiguity> for FrameContiguityState {
    fn from(value: &mut Vp8Contiguity) -> Self {
        Self {
            last_picture_id: value.last_picture_id,
            last_tl0_picture_id: value.last_tl0_picture_id,
        }
    }
}

impl From<FrameContiguityState> for Vp8Contiguity {
    fn from(value: FrameContiguityState) -> Self {
        Self {
            last_picture_id: value.last_picture_id,
            last_tl0_picture_id: value.last_tl0_picture_id,
        }
    }
}

#[cfg(test)]
mod test {

    use super::Vp8Contiguity;
    const L1T3_LAYERS: &[u64] = &[0, 2, 1, 2];
    const L1T2_LAYERS: &[u64] = &[0, 1, 0, 1];

    #[test]
    fn contiguous_l1t3() {
        let mut contiguity = Vp8Contiguity::new();

        for i in 0..100 {
            let next = &crate::packet::Vp8CodecExtra {
                discardable: false,
                sync: true,
                layer_index: L1T3_LAYERS[i as usize % 4] as u8,
                picture_id: Some(i),
                tl0_picture_id: Some(i / 4),
                is_keyframe: false,
            };

            let res = contiguity.check(next, true);
            assert_eq!(res, (true, true), "Failure at picture {} {:?}", i, next);
        }
    }

    #[test]
    fn contiguous_l1t3_no_l2_contig_l0() {
        const L1T3_LAYERS: &[u64] = &[0, 2, 1, 2];

        let mut contiguity = Vp8Contiguity::new();

        for i in 0..100 {
            let layer_index = L1T3_LAYERS[i as usize % 4] as u8;
            if layer_index == 2 {
                continue;
            }

            let next = &crate::packet::Vp8CodecExtra {
                discardable: false,
                sync: i % 8 == 0,
                layer_index,
                picture_id: Some(i),
                tl0_picture_id: Some(i / 4),
                is_keyframe: false,
            };

            let (emit, contiguous) = contiguity.check(next, true);

            // all layer 0 are contiguous therefore no discontinuity
            assert_eq!(emit, next.layer_index == 0);
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

        let mut contiguity = Vp8Contiguity::new();

        for i in 0..100 {
            let layer_index = L1T3_LAYERS[i as usize % 4] as u8;
            if layer_index == 2 {
                continue;
            }

            let next = &crate::packet::Vp8CodecExtra {
                discardable: false,
                sync: i % 8 == 0,
                layer_index,
                picture_id: Some(i),
                tl0_picture_id: Some(i),
                is_keyframe: false,
            };

            let (emit, _) = contiguity.check(next, true);

            assert!(emit == (next.layer_index == 0));
        }
    }

    #[test]
    fn contiguous_l1t2() {
        let mut contiguity = Vp8Contiguity::new();

        for i in 0..100 {
            let next = &crate::packet::Vp8CodecExtra {
                discardable: false,
                sync: true,
                layer_index: L1T2_LAYERS[i as usize % 4] as u8,
                picture_id: Some(i),
                tl0_picture_id: Some(i / 2),
                is_keyframe: false,
            };

            let res = contiguity.check(next, true);
            assert_eq!(res, (true, true), "Failure at picture {} {:?}", i, next);
        }
    }

    #[test]
    fn contiguous_l1t1_no_l1_contig_l0() {
        let mut contiguity = Vp8Contiguity::new();

        for i in 0..100 {
            let layer_index = L1T2_LAYERS[i as usize % 4] as u8;
            if layer_index == 1 {
                continue;
            }

            let next = &crate::packet::Vp8CodecExtra {
                discardable: false,
                sync: i % 8 == 0,
                layer_index,
                picture_id: Some(i),
                tl0_picture_id: Some(i / 2),
                is_keyframe: false,
            };

            let (emit, _) = contiguity.check(next, true);

            // all layer 0 are contiguous therefore can be emitted
            assert_eq!(emit, next.layer_index == 0);
        }
    }
}
