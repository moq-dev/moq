pub mod typedef_h {
    pub const silk_int16_MIN: i32 = i16::MIN as i32;
    pub const silk_int16_MAX: i32 = i16::MAX as i32;
}

pub use self::typedef_h::{silk_int16_MAX, silk_int16_MIN};

pub unsafe fn silk_biquad_alt_stride1(
    in_0: *const i16,
    B_Q28: *const i32,
    A_Q28: *const i32,
    S: *mut i32,
    out: *mut i16,
    len: i32,
) {
    let mut k: i32 = 0;
    let mut inval: i32 = 0;
    let mut A0_U_Q28: i32 = 0;
    let mut A0_L_Q28: i32 = 0;
    let mut A1_U_Q28: i32 = 0;
    let mut A1_L_Q28: i32 = 0;
    let mut out32_Q14: i32 = 0;
    A0_L_Q28 = -*A_Q28.offset(0 as isize) & 0x3fff;
    A0_U_Q28 = -*A_Q28.offset(0 as isize) >> 14;
    A1_L_Q28 = -*A_Q28.offset(1 as isize) & 0x3fff;
    A1_U_Q28 = -*A_Q28.offset(1 as isize) >> 14;
    k = 0;
    while k < len {
        inval = *in_0.offset(k as isize) as i32;
        out32_Q14 = (((*S.offset(0 as isize) as i64
            + (*B_Q28.offset(0 as isize) as i64 * inval as i16 as i64 >> 16))
            as i32 as u32)
            << 2) as i32;
        *S.offset(0 as isize) = *S.offset(1 as isize)
            + (if 14 == 1 {
                ((out32_Q14 as i64 * A0_L_Q28 as i16 as i64 >> 16) as i32 >> 1)
                    + ((out32_Q14 as i64 * A0_L_Q28 as i16 as i64 >> 16) as i32 & 1)
            } else {
                ((out32_Q14 as i64 * A0_L_Q28 as i16 as i64 >> 16) as i32 >> 14 - 1) + 1 >> 1
            });
        *S.offset(0 as isize) = (*S.offset(0 as isize) as i64
            + (out32_Q14 as i64 * A0_U_Q28 as i16 as i64 >> 16))
            as i32;
        *S.offset(0 as isize) = (*S.offset(0 as isize) as i64
            + (*B_Q28.offset(1 as isize) as i64 * inval as i16 as i64 >> 16))
            as i32;
        *S.offset(1 as isize) = if 14 == 1 {
            ((out32_Q14 as i64 * A1_L_Q28 as i16 as i64 >> 16) as i32 >> 1)
                + ((out32_Q14 as i64 * A1_L_Q28 as i16 as i64 >> 16) as i32 & 1)
        } else {
            ((out32_Q14 as i64 * A1_L_Q28 as i16 as i64 >> 16) as i32 >> 14 - 1) + 1 >> 1
        };
        *S.offset(1 as isize) = (*S.offset(1 as isize) as i64
            + (out32_Q14 as i64 * A1_U_Q28 as i16 as i64 >> 16))
            as i32;
        *S.offset(1 as isize) = (*S.offset(1 as isize) as i64
            + (*B_Q28.offset(2 as isize) as i64 * inval as i16 as i64 >> 16))
            as i32;
        *out.offset(k as isize) = (if out32_Q14 + ((1) << 14) - 1 >> 14 > silk_int16_MAX {
            silk_int16_MAX
        } else if (out32_Q14 + ((1) << 14) - 1 >> 14) < silk_int16_MIN {
            silk_int16_MIN
        } else {
            out32_Q14 + ((1) << 14) - 1 >> 14
        }) as i16;
        k += 1;
    }
}
