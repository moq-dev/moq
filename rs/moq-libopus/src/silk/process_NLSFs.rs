use crate::externs::memcpy;
use crate::silk::interpolate::silk_interpolate;
use crate::silk::structs::silk_encoder_state;
use crate::silk::NLSF_VQ_weights_laroia::silk_NLSF_VQ_weights_laroia;
use crate::silk::NLSF_encode::silk_NLSF_encode;
use crate::silk::NLSF2A::silk_NLSF2A;

pub unsafe fn silk_process_NLSFs(
    psEncC: *mut silk_encoder_state,
    PredCoef_Q12: *mut [i16; 16],
    pNLSF_Q15: *mut i16,
    prev_NLSFq_Q15: *const i16,
) {
    let mut i: i32 = 0;
    let mut doInterpolate: i32 = 0;
    let mut NLSF_mu_Q20: i32 = 0;
    let mut i_sqr_Q15: i16 = 0;
    let mut pNLSF0_temp_Q15: [i16; 16] = [0; 16];
    let mut pNLSFW_QW: [i16; 16] = [0; 16];
    let mut pNLSFW0_temp_QW: [i16; 16] = [0; 16];
    assert!(
        (*psEncC).useInterpolatedNLSFs == 1
            || (*psEncC).indices.NLSFInterpCoef_Q2 as i32 == (1) << 2
    );
    NLSF_mu_Q20 = ((0.003f64 * ((1) << 20) as f64 + 0.5f64) as i32 as i64
        + ((-0.001f64 * ((1) << 28) as f64 + 0.5f64) as i32 as i64
            * (*psEncC).speech_activity_Q8 as i16 as i64
            >> 16)) as i32;
    if (*psEncC).nb_subfr == 2 {
        NLSF_mu_Q20 = NLSF_mu_Q20 + (NLSF_mu_Q20 >> 1);
    }
    assert!(NLSF_mu_Q20 > 0);
    silk_NLSF_VQ_weights_laroia(
        &mut pNLSFW_QW[..(*psEncC).predictLPCOrder as usize],
        std::slice::from_raw_parts(pNLSF_Q15, (*psEncC).predictLPCOrder as usize),
    );
    doInterpolate = ((*psEncC).useInterpolatedNLSFs == 1
        && ((*psEncC).indices.NLSFInterpCoef_Q2 as i32) < 4) as i32;
    if doInterpolate != 0 {
        silk_interpolate(
            &mut pNLSF0_temp_Q15[..(*psEncC).predictLPCOrder as usize],
            std::slice::from_raw_parts(prev_NLSFq_Q15, (*psEncC).predictLPCOrder as usize),
            std::slice::from_raw_parts(pNLSF_Q15, (*psEncC).predictLPCOrder as usize),
            (*psEncC).indices.NLSFInterpCoef_Q2 as i32,
        );
        silk_NLSF_VQ_weights_laroia(
            &mut pNLSFW0_temp_QW[..(*psEncC).predictLPCOrder as usize],
            &pNLSF0_temp_Q15[..(*psEncC).predictLPCOrder as usize],
        );
        i_sqr_Q15 = ((((*psEncC).indices.NLSFInterpCoef_Q2 as i16 as i32
            * (*psEncC).indices.NLSFInterpCoef_Q2 as i16 as i32) as u32)
            << 11) as i32 as i16;
        i = 0;
        while i < (*psEncC).predictLPCOrder {
            pNLSFW_QW[i as usize] = ((pNLSFW_QW[i as usize] as i32 >> 1)
                + (pNLSFW0_temp_QW[i as usize] as i32 * i_sqr_Q15 as i32 >> 16))
                as i16;
            i += 1;
        }
    }
    silk_NLSF_encode(
        ((*psEncC).indices.NLSFIndices).as_mut_ptr(),
        pNLSF_Q15,
        (*psEncC).psNLSF_CB,
        pNLSFW_QW.as_mut_ptr(),
        NLSF_mu_Q20,
        (*psEncC).NLSF_MSVQ_Survivors,
        (*psEncC).indices.signalType as i32,
    );
    silk_NLSF2A(
        (*PredCoef_Q12.offset(1 as isize)).as_mut_ptr(),
        pNLSF_Q15 as *const i16,
        (*psEncC).predictLPCOrder,
        (*psEncC).arch,
    );
    if doInterpolate != 0 {
        silk_interpolate(
            &mut pNLSF0_temp_Q15[..(*psEncC).predictLPCOrder as usize],
            std::slice::from_raw_parts(prev_NLSFq_Q15, (*psEncC).predictLPCOrder as usize),
            std::slice::from_raw_parts(pNLSF_Q15 as *const i16, (*psEncC).predictLPCOrder as usize),
            (*psEncC).indices.NLSFInterpCoef_Q2 as i32,
        );
        silk_NLSF2A(
            (*PredCoef_Q12.offset(0 as isize)).as_mut_ptr(),
            pNLSF0_temp_Q15.as_mut_ptr(),
            (*psEncC).predictLPCOrder,
            (*psEncC).arch,
        );
    } else {
        assert!((*psEncC).predictLPCOrder <= 16);
        memcpy(
            (*PredCoef_Q12.offset(0 as isize)).as_mut_ptr() as *mut core::ffi::c_void,
            (*PredCoef_Q12.offset(1 as isize)).as_mut_ptr() as *const core::ffi::c_void,
            ((*psEncC).predictLPCOrder as u64).wrapping_mul(::core::mem::size_of::<i16>() as u64),
        );
    };
}
