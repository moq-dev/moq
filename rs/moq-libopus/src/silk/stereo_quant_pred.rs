pub mod typedef_h {
    pub const silk_int32_MAX: i32 = i32::MAX;
}
pub use self::typedef_h::silk_int32_MAX;
use crate::silk::define::{STEREO_QUANT_SUB_STEPS, STEREO_QUANT_TAB_SIZE};
use crate::silk::tables_other::silk_stereo_pred_quant_Q13;

pub unsafe fn silk_stereo_quant_pred(pred_Q13: *mut i32, ix: *mut [i8; 3]) {
    let mut i: i32 = 0;
    let mut j: i32 = 0;
    let mut n: i32 = 0;
    let mut low_Q13: i32 = 0;
    let mut step_Q13: i32 = 0;
    let mut lvl_Q13: i32 = 0;
    let mut err_min_Q13: i32 = 0;
    let mut err_Q13: i32 = 0;
    let mut quant_pred_Q13: i32 = 0;
    n = 0;
    while n < 2 {
        err_min_Q13 = silk_int32_MAX;
        i = 0;
        's_18: while i < STEREO_QUANT_TAB_SIZE - 1 {
            low_Q13 = silk_stereo_pred_quant_Q13[i as usize] as i32;
            step_Q13 = ((silk_stereo_pred_quant_Q13[(i + 1) as usize] as i32 - low_Q13) as i64
                * (0.5f64 / 5 as f64 * ((1) << 16) as f64 + 0.5f64) as i32 as i16 as i64
                >> 16) as i32;
            j = 0;
            while j < STEREO_QUANT_SUB_STEPS {
                lvl_Q13 = low_Q13 + step_Q13 as i16 as i32 * (2 * j + 1) as i16 as i32;
                err_Q13 = if *pred_Q13.offset(n as isize) - lvl_Q13 > 0 {
                    *pred_Q13.offset(n as isize) - lvl_Q13
                } else {
                    -(*pred_Q13.offset(n as isize) - lvl_Q13)
                };
                if !(err_Q13 < err_min_Q13) {
                    break 's_18;
                }
                err_min_Q13 = err_Q13;
                quant_pred_Q13 = lvl_Q13;
                (*ix.offset(n as isize))[0 as usize] = i as i8;
                (*ix.offset(n as isize))[1 as usize] = j as i8;
                j += 1;
            }
            i += 1;
        }
        (*ix.offset(n as isize))[2 as usize] =
            ((*ix.offset(n as isize))[0 as usize] as i32 / 3) as i8;
        let ref mut fresh0 = (*ix.offset(n as isize))[0 as usize];
        *fresh0 = (*fresh0 as i32 - (*ix.offset(n as isize))[2 as usize] as i32 * 3) as i8;
        *pred_Q13.offset(n as isize) = quant_pred_Q13;
        n += 1;
    }
    let ref mut fresh1 = *pred_Q13.offset(0 as isize);
    *fresh1 -= *pred_Q13.offset(1 as isize);
}
