pub mod typedef_h {
    pub const silk_int16_MIN: i32 = i16::MIN as i32;
    pub const silk_int16_MAX: i32 = i16::MAX as i32;
}

pub use self::typedef_h::{silk_int16_MAX, silk_int16_MIN};
use arrayref::array_mut_ref;

use super::ar2::silk_resampler_private_AR2;
use super::rom::silk_Resampler_2_3_COEFS_LQ;
use crate::silk::resampler::RESAMPLER_MAX_BATCH_SIZE_IN;

const ORDER_FIR: usize = 4;

/// Downsample by a factor 2/3, low quality
pub fn silk_resampler_down2_3(state: &mut [i32; 6], mut out: &mut [i16], mut in_0: &[i16]) {
    let mut nSamplesIn: usize = 0;
    let mut res_Q6: i32 = 0;
    let mut buf: [i32; RESAMPLER_MAX_BATCH_SIZE_IN + ORDER_FIR] =
        [0; RESAMPLER_MAX_BATCH_SIZE_IN + ORDER_FIR];

    let s = state;

    buf[..ORDER_FIR].copy_from_slice(&s[..ORDER_FIR]);

    loop {
        nSamplesIn = in_0.len().min(RESAMPLER_MAX_BATCH_SIZE_IN);
        silk_resampler_private_AR2(
            array_mut_ref![s, ORDER_FIR, 2],
            &mut buf[ORDER_FIR..][..nSamplesIn],
            &in_0[..nSamplesIn],
            &silk_Resampler_2_3_COEFS_LQ,
        );
        let mut buf_ptr = buf.as_mut_slice();
        let mut counter = nSamplesIn;
        while counter > 2 {
            res_Q6 = (buf_ptr[0] as i64 * silk_Resampler_2_3_COEFS_LQ[2] as i64 >> 16) as i32;
            res_Q6 = (res_Q6 as i64
                + (buf_ptr[1] as i64 * silk_Resampler_2_3_COEFS_LQ[3] as i64 >> 16))
                as i32;
            res_Q6 = (res_Q6 as i64
                + (buf_ptr[2] as i64 * silk_Resampler_2_3_COEFS_LQ[5] as i64 >> 16))
                as i32;
            res_Q6 = (res_Q6 as i64
                + (buf_ptr[3] as i64 * silk_Resampler_2_3_COEFS_LQ[4] as i64 >> 16))
                as i32;

            out[0] = (if (if 6 == 1 {
                (res_Q6 >> 1) + (res_Q6 & 1)
            } else {
                (res_Q6 >> 6 - 1) + 1 >> 1
            }) > silk_int16_MAX
            {
                silk_int16_MAX
            } else if (if 6 == 1 {
                (res_Q6 >> 1) + (res_Q6 & 1)
            } else {
                (res_Q6 >> 6 - 1) + 1 >> 1
            }) < silk_int16_MIN
            {
                silk_int16_MIN
            } else if 6 == 1 {
                (res_Q6 >> 1) + (res_Q6 & 1)
            } else {
                (res_Q6 >> 6 - 1) + 1 >> 1
            }) as i16;
            out = &mut out[1..];

            res_Q6 = (buf_ptr[1] as i64 * silk_Resampler_2_3_COEFS_LQ[4] as i64 >> 16) as i32;
            res_Q6 = (res_Q6 as i64
                + (buf_ptr[2] as i64 * silk_Resampler_2_3_COEFS_LQ[5] as i64 >> 16))
                as i32;
            res_Q6 = (res_Q6 as i64
                + (buf_ptr[3] as i64 * silk_Resampler_2_3_COEFS_LQ[3] as i64 >> 16))
                as i32;
            res_Q6 = (res_Q6 as i64
                + (buf_ptr[4] as i64 * silk_Resampler_2_3_COEFS_LQ[2] as i64 >> 16))
                as i32;

            out[0] = (if (if 6 == 1 {
                (res_Q6 >> 1) + (res_Q6 & 1)
            } else {
                (res_Q6 >> 6 - 1) + 1 >> 1
            }) > silk_int16_MAX
            {
                silk_int16_MAX
            } else if (if 6 == 1 {
                (res_Q6 >> 1) + (res_Q6 & 1)
            } else {
                (res_Q6 >> 6 - 1) + 1 >> 1
            }) < silk_int16_MIN
            {
                silk_int16_MIN
            } else if 6 == 1 {
                (res_Q6 >> 1) + (res_Q6 & 1)
            } else {
                (res_Q6 >> 6 - 1) + 1 >> 1
            }) as i16;
            out = &mut out[1..];

            buf_ptr = &mut buf_ptr[3..];
            counter -= 3;
        }

        in_0 = &in_0[nSamplesIn..];
        if in_0.is_empty() {
            break;
        }

        buf.copy_within(nSamplesIn..nSamplesIn + ORDER_FIR, 0);
    }

    s[..ORDER_FIR].copy_from_slice(&buf[nSamplesIn..][..ORDER_FIR]);
}
