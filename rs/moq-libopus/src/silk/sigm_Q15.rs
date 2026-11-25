static mut sigm_LUT_slope_Q10: [i32; 6] = [237, 153, 73, 30, 12, 7];
static mut sigm_LUT_pos_Q15: [i32; 6] = [16384, 23955, 28861, 31213, 32178, 32548];
static mut sigm_LUT_neg_Q15: [i32; 6] = [16384, 8812, 3906, 1554, 589, 219];
pub unsafe fn silk_sigm_Q15(mut in_Q5: i32) -> i32 {
    let mut ind: i32 = 0;
    if in_Q5 < 0 {
        in_Q5 = -in_Q5;
        if in_Q5 >= 6 * 32 {
            return 0;
        } else {
            ind = in_Q5 >> 5;
            return sigm_LUT_neg_Q15[ind as usize]
                - sigm_LUT_slope_Q10[ind as usize] as i16 as i32 * (in_Q5 & 0x1f) as i16 as i32;
        }
    } else if in_Q5 >= 6 * 32 {
        return 32767;
    } else {
        ind = in_Q5 >> 5;
        return sigm_LUT_pos_Q15[ind as usize]
            + sigm_LUT_slope_Q10[ind as usize] as i16 as i32 * (in_Q5 & 0x1f) as i16 as i32;
    };
}
