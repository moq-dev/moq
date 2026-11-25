use crate::celt::entdec::{ec_dec, ec_dec_icdf};
use crate::silk::tables_other::{
    silk_stereo_only_code_mid_iCDF, silk_stereo_pred_joint_iCDF, silk_stereo_pred_quant_Q13,
    silk_uniform3_iCDF, silk_uniform5_iCDF,
};

pub unsafe fn silk_stereo_decode_pred(psRangeDec: &mut ec_dec, pred_Q13: *mut i32) {
    let mut n: i32 = 0;
    let mut ix: [[i32; 3]; 2] = [[0; 3]; 2];
    let mut low_Q13: i32 = 0;
    let mut step_Q13: i32 = 0;
    n = ec_dec_icdf(psRangeDec, &silk_stereo_pred_joint_iCDF, 8);
    ix[0 as usize][2 as usize] = n / 5;
    ix[1 as usize][2 as usize] = n - 5 * ix[0 as usize][2 as usize];
    n = 0;
    while n < 2 {
        ix[n as usize][0 as usize] = ec_dec_icdf(psRangeDec, &silk_uniform3_iCDF, 8);
        ix[n as usize][1 as usize] = ec_dec_icdf(psRangeDec, &silk_uniform5_iCDF, 8);
        n += 1;
    }
    n = 0;
    while n < 2 {
        ix[n as usize][0 as usize] += 3 * ix[n as usize][2 as usize];
        low_Q13 = silk_stereo_pred_quant_Q13[ix[n as usize][0 as usize] as usize] as i32;
        step_Q13 = ((silk_stereo_pred_quant_Q13[(ix[n as usize][0 as usize] + 1) as usize] as i32
            - low_Q13) as i64
            * (0.5f64 / 5 as f64 * ((1) << 16) as f64 + 0.5f64) as i32 as i16 as i64
            >> 16) as i32;
        *pred_Q13.offset(n as isize) =
            low_Q13 + step_Q13 as i16 as i32 * (2 * ix[n as usize][1 as usize] + 1) as i16 as i32;
        n += 1;
    }
    let ref mut fresh0 = *pred_Q13.offset(0 as isize);
    *fresh0 -= *pred_Q13.offset(1 as isize);
}
pub unsafe fn silk_stereo_decode_mid_only(psRangeDec: &mut ec_dec, decode_only_mid: *mut i32) {
    *decode_only_mid = ec_dec_icdf(psRangeDec, &silk_stereo_only_code_mid_iCDF, 8);
}
