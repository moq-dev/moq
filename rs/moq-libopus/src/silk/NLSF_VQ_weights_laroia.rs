pub mod typedef_h {
    pub const silk_int16_MAX: i32 = i16::MAX as i32;
}
pub use self::typedef_h::silk_int16_MAX;
use crate::silk::SigProc_FIX::{silk_max_int, silk_min_int};

const NLSF_W_Q: i32 = 2;

/// Laroia low complexity NLSF weights
///
/// R. Laroia, N. Phamdo and N. Farvardin, "Robust and Efficient Quantization of Speech LSP
/// Parameters Using Structured Vector Quantization", Proc. IEEE Int. Conf. Acoust., Speech,
/// Signal Processing, pp. 641-644, 1991.
pub fn silk_NLSF_VQ_weights_laroia(pNLSFW_Q_OUT: &mut [i16], pNLSF_Q15: &[i16]) {
    let mut tmp1_int: i32 = 0;
    let mut tmp2_int: i32 = 0;

    assert_eq!(pNLSF_Q15.len(), pNLSFW_Q_OUT.len());
    let D = pNLSF_Q15.len();

    assert!(D > 0);
    assert_eq!(D & 1, 0);

    // First value
    tmp1_int = silk_max_int(pNLSF_Q15[0] as i32, 1);
    tmp1_int = (1 << (15 + NLSF_W_Q)) / tmp1_int;
    tmp2_int = silk_max_int(pNLSF_Q15[1] as i32 - pNLSF_Q15[0] as i32, 1);
    tmp2_int = (1 << (15 + NLSF_W_Q)) / tmp2_int;
    pNLSFW_Q_OUT[0] = silk_min_int(tmp1_int + tmp2_int, silk_int16_MAX) as i16;

    // Main loop
    let mut k = 1;
    while k < D - 1 {
        tmp1_int = silk_max_int(pNLSF_Q15[k + 1] as i32 - pNLSF_Q15[k] as i32, 1);
        tmp1_int = (1 << (15 + NLSF_W_Q)) / tmp1_int;
        pNLSFW_Q_OUT[k] = silk_min_int(tmp1_int + tmp2_int, silk_int16_MAX) as i16;
        tmp2_int = silk_max_int(pNLSF_Q15[k + 2] as i32 - pNLSF_Q15[k + 1] as i32, 1);
        tmp2_int = (1 << (15 + NLSF_W_Q)) / tmp2_int;
        pNLSFW_Q_OUT[k + 1] = silk_min_int(tmp1_int + tmp2_int, silk_int16_MAX) as i16;
        k += 2;
    }

    // Last value
    tmp1_int = silk_max_int((1 << 15) - pNLSF_Q15[D - 1] as i32, 1);
    tmp1_int = (1 << (15 + NLSF_W_Q)) / tmp1_int;
    pNLSFW_Q_OUT[D - 1] = silk_min_int(tmp1_int + tmp2_int, silk_int16_MAX) as i16;
}
