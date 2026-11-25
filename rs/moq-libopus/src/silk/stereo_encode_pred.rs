use crate::celt::entenc::{ec_enc, ec_enc_icdf};
use crate::silk::tables_other::{
    silk_stereo_only_code_mid_iCDF, silk_stereo_pred_joint_iCDF, silk_uniform3_iCDF,
    silk_uniform5_iCDF,
};

pub unsafe fn silk_stereo_encode_pred(psRangeEnc: &mut ec_enc, ix: *mut [i8; 3]) {
    let mut n: i32 = 0;
    n = 5 * (*ix.offset(0 as isize))[2 as usize] as i32
        + (*ix.offset(1 as isize))[2 as usize] as i32;
    assert!(n < 25);
    ec_enc_icdf(psRangeEnc, n, &silk_stereo_pred_joint_iCDF, 8);
    n = 0;
    while n < 2 {
        assert!(((*ix.offset(n as isize))[0 as usize] as i32) < 3);
        assert!(((*ix.offset(n as isize))[1 as usize] as i32) < 5);
        ec_enc_icdf(
            psRangeEnc,
            (*ix.offset(n as isize))[0 as usize] as i32,
            &silk_uniform3_iCDF,
            8,
        );
        ec_enc_icdf(
            psRangeEnc,
            (*ix.offset(n as isize))[1 as usize] as i32,
            &silk_uniform5_iCDF,
            8,
        );
        n += 1;
    }
}
pub unsafe fn silk_stereo_encode_mid_only(psRangeEnc: &mut ec_enc, mid_only_flag: i8) {
    ec_enc_icdf(
        psRangeEnc,
        mid_only_flag as i32,
        &silk_stereo_only_code_mid_iCDF,
        8,
    );
}
