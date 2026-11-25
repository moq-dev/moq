pub mod typedef_h {
    pub const silk_int32_MAX: i32 = i32::MAX;
}

pub use self::typedef_h::silk_int32_MAX;

pub unsafe fn silk_log2lin(inLog_Q7: i32) -> i32 {
    let mut out: i32 = 0;
    let mut frac_Q7: i32 = 0;
    if inLog_Q7 < 0 {
        return 0;
    } else {
        if inLog_Q7 >= 3967 {
            return silk_int32_MAX;
        }
    }
    out = ((1) << (inLog_Q7 >> 7)) as i32;
    frac_Q7 = inLog_Q7 & 0x7f;
    if inLog_Q7 < 2048 {
        out = out
            + (out
                * (frac_Q7 as i64
                    + ((frac_Q7 as i16 as i32 * (128 - frac_Q7) as i16 as i32) as i64
                        * -(174) as i16 as i64
                        >> 16)) as i32
                >> 7);
    } else {
        out = out
            + (out >> 7)
                * (frac_Q7 as i64
                    + ((frac_Q7 as i16 as i32 * (128 - frac_Q7) as i16 as i32) as i64
                        * -(174) as i16 as i64
                        >> 16)) as i32;
    }
    return out;
}
