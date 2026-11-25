pub mod typedef_h {
    pub const silk_int32_MAX: i32 = i32::MAX;
}
pub use self::typedef_h::silk_int32_MAX;
use crate::celt::entenc::{ec_enc, ec_enc_icdf};
use crate::silk::code_signs::silk_encode_signs;
use crate::silk::define::{N_RATE_LEVELS, SHELL_CODEC_FRAME_LENGTH, SILK_MAX_PULSES};
use crate::silk::shell_coder::silk_shell_encoder;
use crate::silk::tables_other::silk_lsb_iCDF;
use crate::silk::tables_pulses_per_block::{
    silk_max_pulses_table, silk_pulses_per_block_BITS_Q5, silk_pulses_per_block_iCDF,
    silk_rate_levels_BITS_Q5, silk_rate_levels_iCDF,
};

#[inline]
fn combine_and_check(
    pulses_comb: &mut [i32],
    pulses_in: Option<&[i32]>,
    max_pulses: i32,
    len: i32,
) -> i32 {
    for k in 0..len as usize {
        let pulses_in = if let Some(pulses_in) = pulses_in {
            pulses_in
        } else {
            &*pulses_comb
        };

        let sum = pulses_in[2 * k] + pulses_in[2 * k + 1];
        if sum > max_pulses {
            return 1;
        }
        pulses_comb[k] = sum;
    }

    0
}

/// Encode quantization indices of excitation
pub fn silk_encode_pulses(
    psRangeEnc: &mut ec_enc,
    signalType: i32,
    quantOffsetType: i32,
    pulses_buffer: &mut [i8],
    frame_length: i32,
) {
    let mut i: i32 = 0;
    let mut k: i32 = 0;
    let mut j: i32 = 0;
    let mut iter: i32 = 0;
    let mut bit: i32 = 0;
    let mut nLS: i32 = 0;
    let mut scale_down: i32 = 0;
    let mut RateLevelIndex: i32 = 0;
    let mut abs_q: i32 = 0;
    let mut minSumBits_Q5: i32 = 0;
    let mut sumBits_Q5: i32 = 0;
    let mut pulses_comb: [i32; 8] = [0; 8];

    /****************************/
    /* Prepare for shell coding */
    /****************************/
    /* Calculate number of shell blocks */
    iter = frame_length >> 4;
    // special case for 10 ms @ 12 kHz: the frame length is not a multiple of SHELL_CODEC_FRAME_LENGTH
    // we expand the frame length to the next multiple of SHELL_CODEC_FRAME_LENGTH, filling the extra space with zeros
    if iter * SHELL_CODEC_FRAME_LENGTH < frame_length {
        assert_eq!(frame_length, 12 * 10); /* Make sure only happens for 10 ms @ 12 kHz */
        iter += 1;

        // zero out the unused part of pulses_buffer
        pulses_buffer[frame_length as usize..].fill(0);
    }
    let pulses_frame = &mut pulses_buffer[..(iter * SHELL_CODEC_FRAME_LENGTH) as usize];

    /* Take the absolute value of the pulses */
    let mut abs_pulses = pulses_frame
        .iter()
        .copied()
        .map(|v| v.abs() as i32)
        .collect::<Vec<_>>();

    /* Calc sum pulses per shell code frame */
    let vla_0 = iter as usize;
    let mut sum_pulses: Vec<i32> = ::std::vec::from_elem(0, vla_0);
    let vla_1 = iter as usize;
    let mut nRshifts: Vec<i32> = ::std::vec::from_elem(0, vla_1);
    let mut abs_pulses_ptr = abs_pulses.as_mut_slice();
    i = 0;
    while i < iter {
        nRshifts[i as usize] = 0;
        loop {
            scale_down = combine_and_check(
                &mut pulses_comb,
                Some(abs_pulses_ptr),
                silk_max_pulses_table[0] as i32,
                8,
            );
            scale_down +=
                combine_and_check(&mut pulses_comb, None, silk_max_pulses_table[1] as i32, 4);
            scale_down +=
                combine_and_check(&mut pulses_comb, None, silk_max_pulses_table[2] as i32, 2);
            scale_down += combine_and_check(
                &mut sum_pulses[i as usize..][..1],
                Some(&pulses_comb),
                silk_max_pulses_table[3] as i32,
                1,
            );
            if !(scale_down != 0) {
                break;
            }
            nRshifts[i as usize] += 1;
            k = 0;
            while k < SHELL_CODEC_FRAME_LENGTH {
                abs_pulses_ptr[k as usize] >>= 1;
                k += 1;
            }
        }
        abs_pulses_ptr = &mut abs_pulses_ptr[SHELL_CODEC_FRAME_LENGTH as usize..];

        i += 1;
    }

    /**************/
    /* Rate level */
    /**************/
    /* find rate level that leads to fewest bits for coding of pulses per block info */
    minSumBits_Q5 = silk_int32_MAX;
    k = 0;
    while k < N_RATE_LEVELS - 1 {
        let nBits_ptr = &silk_pulses_per_block_BITS_Q5[k as usize];
        sumBits_Q5 = silk_rate_levels_BITS_Q5[(signalType >> 1) as usize][k as usize] as i32;
        i = 0;
        while i < iter {
            if nRshifts[i as usize] > 0 {
                sumBits_Q5 += nBits_ptr[(SILK_MAX_PULSES + 1) as usize] as i32;
            } else {
                sumBits_Q5 += nBits_ptr[sum_pulses[i as usize] as usize] as i32;
            }
            i += 1;
        }
        if sumBits_Q5 < minSumBits_Q5 {
            minSumBits_Q5 = sumBits_Q5;
            RateLevelIndex = k;
        }
        k += 1;
    }
    ec_enc_icdf(
        psRangeEnc,
        RateLevelIndex,
        &silk_rate_levels_iCDF[(signalType >> 1) as usize],
        8,
    );

    /***************************************************/
    /* Sum-Weighted-Pulses Encoding                    */
    /***************************************************/
    let cdf_ptr = &silk_pulses_per_block_iCDF[RateLevelIndex as usize];
    for i in 0..iter as usize {
        if nRshifts[i] == 0 {
            ec_enc_icdf(psRangeEnc, sum_pulses[i], cdf_ptr, 8);
        } else {
            ec_enc_icdf(psRangeEnc, SILK_MAX_PULSES + 1, cdf_ptr, 8);
            k = 0;
            while k < nRshifts[i] - 1 {
                ec_enc_icdf(
                    psRangeEnc,
                    SILK_MAX_PULSES + 1,
                    &silk_pulses_per_block_iCDF[(N_RATE_LEVELS - 1) as usize],
                    8,
                );
                k += 1;
            }
            ec_enc_icdf(
                psRangeEnc,
                sum_pulses[i],
                &silk_pulses_per_block_iCDF[(N_RATE_LEVELS - 1) as usize],
                8,
            );
        }
    }

    /******************/
    /* Shell Encoding */
    /******************/
    for (i, chunk) in abs_pulses
        .chunks_exact(SHELL_CODEC_FRAME_LENGTH as usize)
        .enumerate()
    {
        if sum_pulses[i] > 0 {
            silk_shell_encoder(psRangeEnc, chunk);
        }
    }

    /****************/
    /* LSB Encoding */
    /****************/
    i = 0;
    while i < iter {
        if nRshifts[i as usize] > 0 {
            let pulses_ptr = &mut pulses_frame[(i * SHELL_CODEC_FRAME_LENGTH) as usize..]
                [..SHELL_CODEC_FRAME_LENGTH as usize];
            nLS = nRshifts[i as usize] - 1;
            k = 0;
            while k < SHELL_CODEC_FRAME_LENGTH {
                abs_q = pulses_ptr[k as usize].abs() as i32;
                j = nLS;
                while j > 0 {
                    bit = abs_q >> j & 1;
                    ec_enc_icdf(psRangeEnc, bit, &silk_lsb_iCDF, 8);
                    j -= 1;
                }
                bit = abs_q & 1;
                ec_enc_icdf(psRangeEnc, bit, &silk_lsb_iCDF, 8);
                k += 1;
            }
        }
        i += 1;
    }

    /****************/
    /* Encode signs */
    /****************/
    silk_encode_signs(
        psRangeEnc,
        pulses_frame,
        frame_length,
        signalType,
        quantOffsetType,
        &sum_pulses,
    );
}
