pub mod typedef_h {
    pub const silk_int16_MIN: i32 = i16::MIN as i32;
    pub const silk_int16_MAX: i32 = i16::MAX as i32;
}
pub use self::typedef_h::{silk_int16_MAX, silk_int16_MIN};
use super::rom::{silk_resampler_down2_0, silk_resampler_down2_1};

pub fn silk_resampler_down2(S: &mut [i32; 2], out: &mut [i16], in_0: &[i16]) {
    assert_eq!(out.len() * 2, in_0.len());

    assert!(silk_resampler_down2_0 as i32 > 0);
    assert!((silk_resampler_down2_1 as i32) < 0);
    for k in 0..out.len() {
        let in32 = ((in_0[2 * k] as i32 as u32) << 10) as i32;
        let Y = in32 - S[0];
        let X = (Y as i64 + ((Y as i64 * silk_resampler_down2_1 as i64) >> 16)) as i32;
        let mut out32 = S[0] + X;
        S[0] = in32 + X;

        let in32 = ((in_0[2 * k + 1] as i32 as u32) << 10) as i32;
        let Y = in32 - S[1];
        let X = (Y as i64 * silk_resampler_down2_0 as i64 >> 16) as i32;
        out32 += S[1];
        out32 += X;
        S[1] = in32 + X;

        out[k] = (if (if 11 == 1 {
            (out32 >> 1) + (out32 & 1)
        } else {
            (out32 >> 11 - 1) + 1 >> 1
        }) > silk_int16_MAX
        {
            silk_int16_MAX
        } else if (if 11 == 1 {
            (out32 >> 1) + (out32 & 1)
        } else {
            (out32 >> 11 - 1) + 1 >> 1
        }) < silk_int16_MIN
        {
            silk_int16_MIN
        } else if 11 == 1 {
            (out32 >> 1) + (out32 & 1)
        } else {
            (out32 >> 11 - 1) + 1 >> 1
        }) as i16;
    }
}
