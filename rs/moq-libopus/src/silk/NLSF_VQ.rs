/// Compute quantization errors for an LPC_order element input vector for a VQ codebook
pub fn silk_NLSF_VQ(
    // Quantization errors [K]
    err_Q24: &mut [i32],
    // Input vectors to be quantized [LPC_order]
    in_Q15: &[i16],
    // Codebook vectors [K*LPC_order]
    pCB_Q8: &[u8],
    // Codebook weights [K*LPC_order]
    pWght_Q9: &[i16],
    // Number of codebook vectors
    K: usize,
    // Number of LPCs
    LPC_order: usize,
) {
    assert_eq!(err_Q24.len(), K);
    assert_eq!(in_Q15.len(), LPC_order);
    assert_eq!(pCB_Q8.len(), K * LPC_order);
    assert_eq!(pWght_Q9.len(), K * LPC_order);

    assert_eq!(LPC_order & 1, 0);

    let mut diff_Q15: i32 = 0;
    let mut diffw_Q24: i32 = 0;
    let mut sum_error_Q24: i32 = 0;
    let mut pred_Q24: i32 = 0;
    let mut cb_Q8_ptr = pCB_Q8;
    let mut w_Q9_ptr = pWght_Q9;

    for i in 0..K {
        sum_error_Q24 = 0;
        pred_Q24 = 0;

        for m in (0..=LPC_order - 2).rev().step_by(2) {
            diff_Q15 = in_Q15[m + 1] as i32 - ((cb_Q8_ptr[m + 1] as i32 as u32) << 7) as i32;
            diffw_Q24 = diff_Q15 as i16 as i32 * w_Q9_ptr[m + 1] as i32;
            sum_error_Q24 += (diffw_Q24 - (pred_Q24 >> 1)).abs();
            pred_Q24 = diffw_Q24;
            diff_Q15 = in_Q15[m] as i32 - ((cb_Q8_ptr[m] as i32 as u32) << 7) as i32;
            diffw_Q24 = diff_Q15 as i16 as i32 * w_Q9_ptr[m] as i32;
            sum_error_Q24 += (diffw_Q24 - (pred_Q24 >> 1)).abs();
            pred_Q24 = diffw_Q24;
        }
        err_Q24[i] = sum_error_Q24;
        cb_Q8_ptr = &cb_Q8_ptr[LPC_order..];
        w_Q9_ptr = &w_Q9_ptr[LPC_order..];
    }
}
