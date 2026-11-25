use crate::externs::{memcpy, memset};
use crate::silk::define::{
    MAX_PREDICTION_POWER_GAIN, MAX_PREDICTION_POWER_GAIN_AFTER_RESET, TYPE_VOICED,
};
use crate::silk::float::find_LPC_FLP::silk_find_LPC_FLP;
use crate::silk::float::find_LTP_FLP::silk_find_LTP_FLP;
use crate::silk::float::residual_energy_FLP::silk_residual_energy_FLP;
use crate::silk::float::scale_copy_vector_FLP::silk_scale_copy_vector_FLP;
use crate::silk::float::structs_FLP::{silk_encoder_control_FLP, silk_encoder_state_FLP};
use crate::silk::float::wrappers_FLP::{silk_process_NLSFs_FLP, silk_quant_LTP_gains_FLP};
use crate::silk::float::LTP_analysis_filter_FLP::silk_LTP_analysis_filter_FLP;
use crate::silk::float::LTP_scale_ctrl_FLP::silk_LTP_scale_ctrl_FLP;
use crate::silk::mathops::silk_exp2;

pub unsafe fn silk_find_pred_coefs_FLP(
    psEnc: *mut silk_encoder_state_FLP,
    psEncCtrl: *mut silk_encoder_control_FLP,
    res_pitch: *const f32,
    x: *const f32,
    condCoding: i32,
) {
    let mut i: i32 = 0;
    let mut XXLTP: [f32; 100] = [0.; 100];
    let mut xXLTP: [f32; 20] = [0.; 20];
    let mut invGains: [f32; 4] = [0.; 4];
    let mut NLSF_Q15: [i16; 16] = [0; 16];
    let mut x_ptr: *const f32 = 0 as *const f32;
    let mut x_pre_ptr: *mut f32 = 0 as *mut f32;
    let mut LPC_in_pre: [f32; 384] = [0.; 384];
    let mut minInvGain: f32 = 0.;
    i = 0;
    while i < (*psEnc).sCmn.nb_subfr {
        invGains[i as usize] = 1.0f32 / (*psEncCtrl).Gains[i as usize];
        i += 1;
    }
    if (*psEnc).sCmn.indices.signalType as i32 == TYPE_VOICED {
        assert!(
            (*psEnc).sCmn.ltp_mem_length - (*psEnc).sCmn.predictLPCOrder
                >= (*psEncCtrl).pitchL[0 as usize] + 5 / 2
        );
        silk_find_LTP_FLP(
            XXLTP.as_mut_ptr(),
            xXLTP.as_mut_ptr(),
            res_pitch,
            ((*psEncCtrl).pitchL).as_mut_ptr() as *const i32,
            (*psEnc).sCmn.subfr_length,
            (*psEnc).sCmn.nb_subfr,
        );
        silk_quant_LTP_gains_FLP(
            ((*psEncCtrl).LTPCoef).as_mut_ptr(),
            ((*psEnc).sCmn.indices.LTPIndex).as_mut_ptr(),
            &mut (*psEnc).sCmn.indices.PERIndex,
            &mut (*psEnc).sCmn.sum_log_gain_Q7,
            &mut (*psEncCtrl).LTPredCodGain,
            XXLTP.as_mut_ptr() as *const f32,
            xXLTP.as_mut_ptr() as *const f32,
            (*psEnc).sCmn.subfr_length,
            (*psEnc).sCmn.nb_subfr,
            (*psEnc).sCmn.arch,
        );
        silk_LTP_scale_ctrl_FLP(psEnc, psEncCtrl, condCoding);
        silk_LTP_analysis_filter_FLP(
            LPC_in_pre.as_mut_ptr(),
            x.offset(-((*psEnc).sCmn.predictLPCOrder as isize)),
            ((*psEncCtrl).LTPCoef).as_mut_ptr() as *const f32,
            ((*psEncCtrl).pitchL).as_mut_ptr() as *const i32,
            invGains.as_mut_ptr() as *const f32,
            (*psEnc).sCmn.subfr_length,
            (*psEnc).sCmn.nb_subfr,
            (*psEnc).sCmn.predictLPCOrder,
        );
    } else {
        x_ptr = x.offset(-((*psEnc).sCmn.predictLPCOrder as isize));
        x_pre_ptr = LPC_in_pre.as_mut_ptr();
        i = 0;
        while i < (*psEnc).sCmn.nb_subfr {
            silk_scale_copy_vector_FLP(
                x_pre_ptr,
                x_ptr,
                invGains[i as usize],
                (*psEnc).sCmn.subfr_length + (*psEnc).sCmn.predictLPCOrder,
            );
            x_pre_ptr = x_pre_ptr
                .offset(((*psEnc).sCmn.subfr_length + (*psEnc).sCmn.predictLPCOrder) as isize);
            x_ptr = x_ptr.offset((*psEnc).sCmn.subfr_length as isize);
            i += 1;
        }
        memset(
            ((*psEncCtrl).LTPCoef).as_mut_ptr() as *mut core::ffi::c_void,
            0,
            (((*psEnc).sCmn.nb_subfr * 5) as u64)
                .wrapping_mul(::core::mem::size_of::<f32>() as u64),
        );
        (*psEncCtrl).LTPredCodGain = 0.0f32;
        (*psEnc).sCmn.sum_log_gain_Q7 = 0;
    }
    if (*psEnc).sCmn.first_frame_after_reset != 0 {
        minInvGain = 1.0f32 / MAX_PREDICTION_POWER_GAIN_AFTER_RESET;
    } else {
        minInvGain = silk_exp2((*psEncCtrl).LTPredCodGain / 3.0) / MAX_PREDICTION_POWER_GAIN;
        minInvGain /= 0.25f32 + 0.75f32 * (*psEncCtrl).coding_quality;
    }
    silk_find_LPC_FLP(
        &mut (*psEnc).sCmn,
        NLSF_Q15.as_mut_ptr(),
        LPC_in_pre.as_mut_ptr() as *const f32,
        minInvGain,
    );
    silk_process_NLSFs_FLP(
        &mut (*psEnc).sCmn,
        ((*psEncCtrl).PredCoef).as_mut_ptr(),
        NLSF_Q15.as_mut_ptr(),
        ((*psEnc).sCmn.prev_NLSFq_Q15).as_mut_ptr() as *const i16,
    );
    silk_residual_energy_FLP(
        ((*psEncCtrl).ResNrg).as_mut_ptr(),
        LPC_in_pre.as_mut_ptr() as *const f32,
        ((*psEncCtrl).PredCoef).as_mut_ptr(),
        ((*psEncCtrl).Gains).as_mut_ptr() as *const f32,
        (*psEnc).sCmn.subfr_length,
        (*psEnc).sCmn.nb_subfr,
        (*psEnc).sCmn.predictLPCOrder,
    );
    memcpy(
        ((*psEnc).sCmn.prev_NLSFq_Q15).as_mut_ptr() as *mut core::ffi::c_void,
        NLSF_Q15.as_mut_ptr() as *const core::ffi::c_void,
        ::core::mem::size_of::<[i16; 16]>() as u64,
    );
}
