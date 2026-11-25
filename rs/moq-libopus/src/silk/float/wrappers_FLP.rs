use crate::silk::A2NLSF::silk_A2NLSF;
use crate::silk::NLSF2A::silk_NLSF2A;

pub mod xmmintrin_h {
    #[cfg(target_arch = "x86")]
    pub use core::arch::x86::{__m128, _mm_cvt_ss2si, _mm_cvtss_si32, _mm_set_ss};
    #[cfg(target_arch = "x86_64")]
    pub use core::arch::x86_64::{__m128, _mm_cvt_ss2si, _mm_cvtss_si32, _mm_set_ss};
}
pub mod SigProc_FLP_h {
    #[inline]
    pub unsafe fn silk_float2int(x: f32) -> i32 {
        return float2int(x);
    }
    use crate::celt::float_cast::float2int;
}

pub use self::SigProc_FLP_h::silk_float2int;
use crate::silk::define::{LTP_ORDER, MAX_SHAPE_LPC_ORDER, TYPE_VOICED};
use crate::silk::float::structs_FLP::{silk_encoder_control_FLP, silk_encoder_state_FLP};
use crate::silk::process_NLSFs::silk_process_NLSFs;
use crate::silk::quant_LTP_gains::silk_quant_LTP_gains;
use crate::silk::structs::{silk_encoder_state, silk_nsq_state, SideInfoIndices};
use crate::silk::tables_other::silk_LTPScales_table_Q14;
use crate::silk::NSQ_del_dec::silk_NSQ_del_dec_c;
use crate::silk::NSQ::silk_NSQ_c;

pub unsafe fn silk_A2NLSF_FLP(NLSF_Q15: *mut i16, pAR: *const f32, LPC_order: i32) {
    let mut i: i32 = 0;
    let mut a_fix_Q16: [i32; 16] = [0; 16];
    i = 0;
    while i < LPC_order {
        a_fix_Q16[i as usize] = silk_float2int(*pAR.offset(i as isize) * 65536.0f32);
        i += 1;
    }
    silk_A2NLSF(NLSF_Q15, a_fix_Q16.as_mut_ptr(), LPC_order);
}
pub unsafe fn silk_NLSF2A_FLP(pAR: *mut f32, NLSF_Q15: *const i16, LPC_order: i32, arch: i32) {
    let mut i: i32 = 0;
    let mut a_fix_Q12: [i16; 16] = [0; 16];
    silk_NLSF2A(a_fix_Q12.as_mut_ptr(), NLSF_Q15, LPC_order, arch);
    i = 0;
    while i < LPC_order {
        *pAR.offset(i as isize) = a_fix_Q12[i as usize] as f32 * (1.0f32 / 4096.0f32);
        i += 1;
    }
}
pub unsafe fn silk_process_NLSFs_FLP(
    psEncC: *mut silk_encoder_state,
    PredCoef: *mut [f32; 16],
    NLSF_Q15: *mut i16,
    prev_NLSF_Q15: *const i16,
) {
    let mut i: i32 = 0;
    let mut j: i32 = 0;
    let mut PredCoef_Q12: [[i16; 16]; 2] = [[0; 16]; 2];
    silk_process_NLSFs(psEncC, PredCoef_Q12.as_mut_ptr(), NLSF_Q15, prev_NLSF_Q15);
    j = 0;
    while j < 2 {
        i = 0;
        while i < (*psEncC).predictLPCOrder {
            (*PredCoef.offset(j as isize))[i as usize] =
                PredCoef_Q12[j as usize][i as usize] as f32 * (1.0f32 / 4096.0f32);
            i += 1;
        }
        j += 1;
    }
}
pub unsafe fn silk_NSQ_wrapper_FLP(
    psEnc: *mut silk_encoder_state_FLP,
    psEncCtrl: *mut silk_encoder_control_FLP,
    psIndices: *mut SideInfoIndices,
    psNSQ: *mut silk_nsq_state,
    pulses: *mut i8,
    x: *const f32,
) {
    let mut i: i32 = 0;
    let mut j: i32 = 0;
    let mut x16: [i16; 320] = [0; 320];
    let mut Gains_Q16: [i32; 4] = [0; 4];
    let mut PredCoef_Q12: [[i16; 16]; 2] = [[0; 16]; 2];
    let mut LTPCoef_Q14: [i16; 20] = [0; 20];
    let mut LTP_scale_Q14: i32 = 0;
    let mut AR_Q13: [i16; 96] = [0; 96];
    let mut LF_shp_Q14: [i32; 4] = [0; 4];
    let mut Lambda_Q10: i32 = 0;
    let mut Tilt_Q14: [i32; 4] = [0; 4];
    let mut HarmShapeGain_Q14: [i32; 4] = [0; 4];
    i = 0;
    while i < (*psEnc).sCmn.nb_subfr {
        j = 0;
        while j < (*psEnc).sCmn.shapingLPCOrder {
            AR_Q13[(i * MAX_SHAPE_LPC_ORDER + j) as usize] =
                silk_float2int((*psEncCtrl).AR[(i * MAX_SHAPE_LPC_ORDER + j) as usize] * 8192.0f32)
                    as i16;
            j += 1;
        }
        i += 1;
    }
    i = 0;
    while i < (*psEnc).sCmn.nb_subfr {
        LF_shp_Q14[i as usize] =
            ((silk_float2int((*psEncCtrl).LF_AR_shp[i as usize] * 16384.0f32) as u32) << 16) as i32
                | silk_float2int((*psEncCtrl).LF_MA_shp[i as usize] * 16384.0f32) as u16 as i32;
        Tilt_Q14[i as usize] = silk_float2int((*psEncCtrl).Tilt[i as usize] * 16384.0f32);
        HarmShapeGain_Q14[i as usize] =
            silk_float2int((*psEncCtrl).HarmShapeGain[i as usize] * 16384.0f32);
        i += 1;
    }
    Lambda_Q10 = silk_float2int((*psEncCtrl).Lambda * 1024.0f32);
    i = 0;
    while i < (*psEnc).sCmn.nb_subfr * LTP_ORDER {
        LTPCoef_Q14[i as usize] =
            silk_float2int((*psEncCtrl).LTPCoef[i as usize] * 16384.0f32) as i16;
        i += 1;
    }
    j = 0;
    while j < 2 {
        i = 0;
        while i < (*psEnc).sCmn.predictLPCOrder {
            PredCoef_Q12[j as usize][i as usize] =
                silk_float2int((*psEncCtrl).PredCoef[j as usize][i as usize] * 4096.0f32) as i16;
            i += 1;
        }
        j += 1;
    }
    i = 0;
    while i < (*psEnc).sCmn.nb_subfr {
        Gains_Q16[i as usize] = silk_float2int((*psEncCtrl).Gains[i as usize] * 65536.0f32);
        i += 1;
    }
    if (*psIndices).signalType as i32 == TYPE_VOICED {
        LTP_scale_Q14 = silk_LTPScales_table_Q14[(*psIndices).LTP_scaleIndex as usize] as i32;
    } else {
        LTP_scale_Q14 = 0;
    }
    i = 0;
    while i < (*psEnc).sCmn.frame_length {
        x16[i as usize] = silk_float2int(*x.offset(i as isize)) as i16;
        i += 1;
    }
    if (*psEnc).sCmn.nStatesDelayedDecision > 1 || (*psEnc).sCmn.warping_Q16 > 0 {
        silk_NSQ_del_dec_c(
            &mut (*psEnc).sCmn,
            psNSQ,
            psIndices,
            x16.as_mut_ptr() as *const i16,
            pulses,
            (PredCoef_Q12[0 as usize]).as_mut_ptr() as *const i16,
            LTPCoef_Q14.as_mut_ptr() as *const i16,
            AR_Q13.as_mut_ptr() as *const i16,
            HarmShapeGain_Q14.as_mut_ptr() as *const i32,
            Tilt_Q14.as_mut_ptr() as *const i32,
            LF_shp_Q14.as_mut_ptr() as *const i32,
            Gains_Q16.as_mut_ptr() as *const i32,
            ((*psEncCtrl).pitchL).as_mut_ptr() as *const i32,
            Lambda_Q10,
            LTP_scale_Q14,
        );
    } else {
        silk_NSQ_c(
            &mut (*psEnc).sCmn,
            psNSQ,
            psIndices,
            x16.as_mut_ptr() as *const i16,
            pulses,
            (PredCoef_Q12[0 as usize]).as_mut_ptr() as *const i16,
            LTPCoef_Q14.as_mut_ptr() as *const i16,
            AR_Q13.as_mut_ptr() as *const i16,
            HarmShapeGain_Q14.as_mut_ptr() as *const i32,
            Tilt_Q14.as_mut_ptr() as *const i32,
            LF_shp_Q14.as_mut_ptr() as *const i32,
            Gains_Q16.as_mut_ptr() as *const i32,
            ((*psEncCtrl).pitchL).as_mut_ptr() as *const i32,
            Lambda_Q10,
            LTP_scale_Q14,
        );
    };
}
pub unsafe fn silk_quant_LTP_gains_FLP(
    B: *mut f32,
    cbk_index: *mut i8,
    periodicity_index: *mut i8,
    sum_log_gain_Q7: *mut i32,
    pred_gain_dB: *mut f32,
    XX: *const f32,
    xX: *const f32,
    subfr_len: i32,
    nb_subfr: i32,
    arch: i32,
) {
    let mut i: i32 = 0;
    let mut pred_gain_dB_Q7: i32 = 0;
    let mut B_Q14: [i16; 20] = [0; 20];
    let mut XX_Q17: [i32; 100] = [0; 100];
    let mut xX_Q17: [i32; 20] = [0; 20];
    i = 0;
    while i < nb_subfr * LTP_ORDER * LTP_ORDER {
        XX_Q17[i as usize] = silk_float2int(*XX.offset(i as isize) * 131072.0f32);
        i += 1;
    }
    i = 0;
    while i < nb_subfr * LTP_ORDER {
        xX_Q17[i as usize] = silk_float2int(*xX.offset(i as isize) * 131072.0f32);
        i += 1;
    }
    silk_quant_LTP_gains(
        B_Q14.as_mut_ptr(),
        cbk_index,
        periodicity_index,
        sum_log_gain_Q7,
        &mut pred_gain_dB_Q7,
        XX_Q17.as_mut_ptr() as *const i32,
        xX_Q17.as_mut_ptr() as *const i32,
        subfr_len,
        nb_subfr,
        arch,
    );
    i = 0;
    while i < nb_subfr * LTP_ORDER {
        *B.offset(i as isize) = B_Q14[i as usize] as f32 * (1.0f32 / 16384.0f32);
        i += 1;
    }
    *pred_gain_dB = pred_gain_dB_Q7 as f32 * (1.0f32 / 128.0f32);
}
