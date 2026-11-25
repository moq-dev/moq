pub mod typedef_h {
    pub const silk_int16_MIN: i32 = i16::MIN as i32;
    pub const silk_int16_MAX: i32 = i16::MAX as i32;
}

pub use self::typedef_h::{silk_int16_MAX, silk_int16_MIN};

static mut A_fb1_20: i16 = ((5394) << 1) as i16;
static mut A_fb1_21: i16 = -(24290) as i16;
pub unsafe fn silk_ana_filt_bank_1(
    in_0: *const i16,
    S: *mut i32,
    outL: *mut i16,
    outH: *mut i16,
    N: i32,
) {
    let mut k: i32 = 0;
    let N2: i32 = N >> 1;
    let mut in32: i32 = 0;
    let mut X: i32 = 0;
    let mut Y: i32 = 0;
    let mut out_1: i32 = 0;
    let mut out_2: i32 = 0;
    k = 0;
    while k < N2 {
        in32 = ((*in_0.offset((2 * k) as isize) as i32 as u32) << 10) as i32;
        Y = in32 - *S.offset(0 as isize);
        X = (Y as i64 + (Y as i64 * A_fb1_21 as i64 >> 16)) as i32;
        out_1 = *S.offset(0 as isize) + X;
        *S.offset(0 as isize) = in32 + X;
        in32 = ((*in_0.offset((2 * k + 1) as isize) as i32 as u32) << 10) as i32;
        Y = in32 - *S.offset(1 as isize);
        X = (Y as i64 * A_fb1_20 as i64 >> 16) as i32;
        out_2 = *S.offset(1 as isize) + X;
        *S.offset(1 as isize) = in32 + X;
        *outL.offset(k as isize) = (if (if 11 == 1 {
            (out_2 + out_1 >> 1) + (out_2 + out_1 & 1)
        } else {
            (out_2 + out_1 >> 11 - 1) + 1 >> 1
        }) > silk_int16_MAX
        {
            silk_int16_MAX
        } else if (if 11 == 1 {
            (out_2 + out_1 >> 1) + (out_2 + out_1 & 1)
        } else {
            (out_2 + out_1 >> 11 - 1) + 1 >> 1
        }) < silk_int16_MIN
        {
            silk_int16_MIN
        } else if 11 == 1 {
            (out_2 + out_1 >> 1) + (out_2 + out_1 & 1)
        } else {
            (out_2 + out_1 >> 11 - 1) + 1 >> 1
        }) as i16;
        *outH.offset(k as isize) = (if (if 11 == 1 {
            (out_2 - out_1 >> 1) + (out_2 - out_1 & 1)
        } else {
            (out_2 - out_1 >> 11 - 1) + 1 >> 1
        }) > silk_int16_MAX
        {
            silk_int16_MAX
        } else if (if 11 == 1 {
            (out_2 - out_1 >> 1) + (out_2 - out_1 & 1)
        } else {
            (out_2 - out_1 >> 11 - 1) + 1 >> 1
        }) < silk_int16_MIN
        {
            silk_int16_MIN
        } else if 11 == 1 {
            (out_2 - out_1 >> 1) + (out_2 - out_1 & 1)
        } else {
            (out_2 - out_1 >> 11 - 1) + 1 >> 1
        }) as i16;
        k += 1;
    }
}
