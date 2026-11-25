use crate::silk::NLSF_stabilize::silk_NLSF_stabilize;
use crate::silk::NLSF_unpack::silk_NLSF_unpack;

use crate::silk::structs::silk_NLSF_CB_struct;

/// Predictive dequantizer for NLSF residuals
///
/// Returns RD value in Q30
#[inline]
fn silk_NLSF_residual_dequant(
    x_Q10: &mut [i16],
    indices: &[i8],
    pred_coef_Q8: &[u8],
    quant_step_size_Q16: i32,
) {
    assert_eq!(x_Q10.len(), indices.len());
    assert_eq!(x_Q10.len(), pred_coef_Q8.len());

    let mut out_Q10 = 0;

    for (x_Q10, (&index, &pref_coef_Q8)) in x_Q10
        .iter_mut()
        .zip(indices.iter().zip(pred_coef_Q8.iter()))
        .rev()
    {
        let pred_Q10 = out_Q10 as i16 as i32 * pref_coef_Q8 as i16 as i32 >> 8;
        out_Q10 = ((index as u32) << 10) as i32;
        if out_Q10 > 0 {
            out_Q10 -= (0.1f64 * ((1) << 10) as f64 + 0.5f64) as i32;
        } else if out_Q10 < 0 {
            out_Q10 += (0.1f64 * ((1) << 10) as f64 + 0.5f64) as i32;
        }
        out_Q10 =
            (pred_Q10 as i64 + (out_Q10 as i64 * quant_step_size_Q16 as i16 as i64 >> 16)) as i32;
        *x_Q10 = out_Q10 as i16;
    }
}

/// NLSF vector decoder
pub fn silk_NLSF_decode(
    pNLSF_Q15: &mut [i16],
    NLSFIndices: &[i8],
    psNLSF_CB: &silk_NLSF_CB_struct,
) {
    assert_eq!(pNLSF_Q15.len(), psNLSF_CB.order as usize);
    assert_eq!(NLSFIndices.len(), 1 + psNLSF_CB.order as usize);

    let mut pred_Q8: [u8; 16] = [0; 16];
    let mut ec_ix: [i16; 16] = [0; 16];
    let mut res_Q10: [i16; 16] = [0; 16];
    let mut NLSF_Q15_tmp: i32 = 0;

    // Unpack entropy table indices and predictor for current CB1 index
    silk_NLSF_unpack(&mut ec_ix, &mut pred_Q8, psNLSF_CB, NLSFIndices[0] as i32);

    // Predictive residual dequantizer
    silk_NLSF_residual_dequant(
        &mut res_Q10[..psNLSF_CB.order as usize],
        &NLSFIndices[1..],
        &pred_Q8[..psNLSF_CB.order as usize],
        psNLSF_CB.quantStepSize_Q16 as i32,
    );

    // Apply inverse square-rooted weights to first stage and add to output
    let pCB_element =
        &psNLSF_CB.CB1_NLSF_Q8[(NLSFIndices[0] as i32 * psNLSF_CB.order as i32) as usize..];
    let pCB_Wght_Q9 =
        &psNLSF_CB.CB1_Wght_Q9[(NLSFIndices[0] as i32 * psNLSF_CB.order as i32) as usize..];
    for (out, ((&res_Q10, &pCB_Wght_Q9), &pCB_element)) in pNLSF_Q15
        .iter_mut()
        .zip(res_Q10.iter().zip(pCB_Wght_Q9).zip(pCB_element))
    {
        NLSF_Q15_tmp = ((res_Q10 as i32 as u32) << 14) as i32 / pCB_Wght_Q9 as i32
            + ((pCB_element as i16 as u32) << 7) as i32;
        *out = (if 0 > 32767 {
            if NLSF_Q15_tmp > 0 {
                0
            } else if NLSF_Q15_tmp < 32767 {
                32767
            } else {
                NLSF_Q15_tmp
            }
        } else if NLSF_Q15_tmp > 32767 {
            32767
        } else if NLSF_Q15_tmp < 0 {
            0
        } else {
            NLSF_Q15_tmp
        }) as i16;
    }
    silk_NLSF_stabilize(pNLSF_Q15, psNLSF_CB.deltaMin_Q15);
}
