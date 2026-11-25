pub mod SigProc_FLP_h {
    #[inline]
    pub unsafe fn silk_sigmoid(x: f32) -> f32 {
        return (1.0f64 / (1.0f64 + (-x as f64).exp())) as f32;
    }
    #[inline]
    pub unsafe fn silk_log2(x: f64) -> f32 {
        return (3.32192809488736f64 * x.log10()) as f32;
    }
}
pub mod tuning_parameters_h {
    pub const FIND_PITCH_WHITE_NOISE_FRACTION: f32 = 1e-3f32;
    pub const BG_SNR_DECR_dB: f32 = 2.0f32;
    pub const HARM_SNR_INCR_dB: f32 = 2.0f32;
    pub const ENERGY_VARIATION_THRESHOLD_QNT_OFFSET: f32 = 0.6f32;
    pub const SHAPE_WHITE_NOISE_FRACTION: f32 = 3e-5f32;
    pub const BANDWIDTH_EXPANSION: f32 = 0.94f32;
    pub const HARMONIC_SHAPING: f32 = 0.3f32;
    pub const HIGH_RATE_OR_LOW_QUALITY_HARMONIC_SHAPING: f32 = 0.2f32;
    pub const HP_NOISE_COEF: f32 = 0.25f32;
    pub const HARM_HP_NOISE_COEF: f32 = 0.35f32;
    pub const LOW_FREQ_SHAPING: f32 = 4.0f32;
    pub const LOW_QUALITY_LOW_FREQ_SHAPING_DECR: f32 = 0.5f32;
    pub const SUBFR_SMTH_COEF: f32 = 0.4f32;
}

pub use self::tuning_parameters_h::{
    BG_SNR_DECR_dB, HARM_SNR_INCR_dB, BANDWIDTH_EXPANSION, ENERGY_VARIATION_THRESHOLD_QNT_OFFSET,
    FIND_PITCH_WHITE_NOISE_FRACTION, HARMONIC_SHAPING, HARM_HP_NOISE_COEF,
    HIGH_RATE_OR_LOW_QUALITY_HARMONIC_SHAPING, HP_NOISE_COEF, LOW_FREQ_SHAPING,
    LOW_QUALITY_LOW_FREQ_SHAPING_DECR, SHAPE_WHITE_NOISE_FRACTION, SUBFR_SMTH_COEF,
};
use crate::celt::mathops::celt_sqrt;
use crate::silk::define::{MAX_SHAPE_LPC_ORDER, MIN_QGAIN_DB, TYPE_VOICED, USE_HARM_SHAPING};
use crate::silk::float::structs_FLP::{
    silk_encoder_control_FLP, silk_encoder_state_FLP, silk_shape_state_FLP,
};

pub use self::SigProc_FLP_h::{silk_log2, silk_sigmoid};
use crate::externs::memcpy;
use crate::silk::float::apply_sine_window_FLP::silk_apply_sine_window_FLP;
use crate::silk::float::autocorrelation_FLP::silk_autocorrelation_FLP;
use crate::silk::float::bwexpander_FLP::silk_bwexpander_FLP;
use crate::silk::float::energy_FLP::silk_energy_FLP;
use crate::silk::float::k2a_FLP::silk_k2a_FLP;
use crate::silk::float::schur_FLP::silk_schur_FLP;
use crate::silk::float::warped_autocorrelation_FLP::silk_warped_autocorrelation_FLP;
use crate::silk::mathops::silk_exp2;

#[inline]
unsafe fn warped_gain(coefs: *const f32, mut lambda: f32, order: i32) -> f32 {
    let mut i: i32 = 0;
    let mut gain: f32 = 0.;
    lambda = -lambda;
    gain = *coefs.offset((order - 1) as isize);
    i = order - 2;
    while i >= 0 {
        gain = lambda * gain + *coefs.offset(i as isize);
        i -= 1;
    }
    return 1.0f32 / (1.0f32 - lambda * gain);
}
#[inline]
unsafe fn warped_true2monic_coefs(coefs: *mut f32, lambda: f32, limit: f32, order: i32) {
    let mut i: i32 = 0;
    let mut iter: i32 = 0;
    let mut ind: i32 = 0;
    let mut tmp: f32 = 0.;
    let mut maxabs: f32 = 0.;
    let mut chirp: f32 = 0.;
    let mut gain: f32 = 0.;
    i = order - 1;
    while i > 0 {
        *coefs.offset((i - 1) as isize) -= lambda * *coefs.offset(i as isize);
        i -= 1;
    }
    gain = (1.0f32 - lambda * lambda) / (1.0f32 + lambda * *coefs.offset(0 as isize));
    i = 0;
    while i < order {
        *coefs.offset(i as isize) *= gain;
        i += 1;
    }
    iter = 0;
    while iter < 10 {
        maxabs = -1.0f32;
        i = 0;
        while i < order {
            tmp = (*coefs.offset(i as isize)).abs();
            if tmp > maxabs {
                maxabs = tmp;
                ind = i;
            }
            i += 1;
        }
        if maxabs <= limit {
            return;
        }
        i = 1;
        while i < order {
            *coefs.offset((i - 1) as isize) += lambda * *coefs.offset(i as isize);
            i += 1;
        }
        gain = 1.0f32 / gain;
        i = 0;
        while i < order {
            *coefs.offset(i as isize) *= gain;
            i += 1;
        }
        chirp = 0.99f32
            - (0.8f32 + 0.1f32 * iter as f32) * (maxabs - limit) / (maxabs * (ind + 1) as f32);
        silk_bwexpander_FLP(coefs, order, chirp);
        i = order - 1;
        while i > 0 {
            *coefs.offset((i - 1) as isize) -= lambda * *coefs.offset(i as isize);
            i -= 1;
        }
        gain = (1.0f32 - lambda * lambda) / (1.0f32 + lambda * *coefs.offset(0 as isize));
        i = 0;
        while i < order {
            *coefs.offset(i as isize) *= gain;
            i += 1;
        }
        iter += 1;
    }
}
#[inline]
unsafe fn limit_coefs(coefs: *mut f32, limit: f32, order: i32) {
    let mut i: i32 = 0;
    let mut iter: i32 = 0;
    let mut ind: i32 = 0;
    let mut tmp: f32 = 0.;
    let mut maxabs: f32 = 0.;
    let mut chirp: f32 = 0.;
    iter = 0;
    while iter < 10 {
        maxabs = -1.0f32;
        i = 0;
        while i < order {
            tmp = (*coefs.offset(i as isize)).abs();
            if tmp > maxabs {
                maxabs = tmp;
                ind = i;
            }
            i += 1;
        }
        if maxabs <= limit {
            return;
        }
        chirp = 0.99f32
            - (0.8f32 + 0.1f32 * iter as f32) * (maxabs - limit) / (maxabs * (ind + 1) as f32);
        silk_bwexpander_FLP(coefs, order, chirp);
        iter += 1;
    }
}
pub unsafe fn silk_noise_shape_analysis_FLP(
    psEnc: *mut silk_encoder_state_FLP,
    psEncCtrl: *mut silk_encoder_control_FLP,
    pitch_res: *const f32,
    x: *const f32,
) {
    let psShapeSt: *mut silk_shape_state_FLP = &mut (*psEnc).sShape;
    let mut k: i32 = 0;
    let mut nSamples: i32 = 0;
    let mut nSegs: i32 = 0;
    let mut SNR_adj_dB: f32 = 0.;
    let mut HarmShapeGain: f32 = 0.;
    let mut Tilt: f32 = 0.;
    let mut nrg: f32 = 0.;
    let mut log_energy: f32 = 0.;
    let mut log_energy_prev: f32 = 0.;
    let mut energy_variation: f32 = 0.;
    let mut BWExp: f32 = 0.;
    let mut gain_mult: f32 = 0.;
    let mut gain_add: f32 = 0.;
    let mut strength: f32 = 0.;
    let mut b: f32 = 0.;
    let mut warping: f32 = 0.;
    let mut x_windowed: [f32; 240] = [0.; 240];
    let mut auto_corr: [f32; 25] = [0.; 25];
    let mut rc: [f32; 25] = [0.; 25];
    let mut x_ptr: *const f32 = 0 as *const f32;
    let mut pitch_res_ptr: *const f32 = 0 as *const f32;
    x_ptr = x.offset(-((*psEnc).sCmn.la_shape as isize));
    SNR_adj_dB = (*psEnc).sCmn.SNR_dB_Q7 as f32 * (1 as f32 / 128.0f32);
    (*psEncCtrl).input_quality = 0.5f32
        * ((*psEnc).sCmn.input_quality_bands_Q15[0 as usize]
            + (*psEnc).sCmn.input_quality_bands_Q15[1 as usize]) as f32
        * (1.0f32 / 32768.0f32);
    (*psEncCtrl).coding_quality = silk_sigmoid(0.25f32 * (SNR_adj_dB - 20.0f32));
    if (*psEnc).sCmn.useCBR == 0 {
        b = 1.0f32 - (*psEnc).sCmn.speech_activity_Q8 as f32 * (1.0f32 / 256.0f32);
        SNR_adj_dB -= BG_SNR_DECR_dB
            * (*psEncCtrl).coding_quality
            * (0.5f32 + 0.5f32 * (*psEncCtrl).input_quality)
            * b
            * b;
    }
    if (*psEnc).sCmn.indices.signalType as i32 == TYPE_VOICED {
        SNR_adj_dB += HARM_SNR_INCR_dB * (*psEnc).LTPCorr;
    } else {
        SNR_adj_dB += (-0.4f32 * (*psEnc).sCmn.SNR_dB_Q7 as f32 * (1 as f32 / 128.0f32) + 6.0f32)
            * (1.0f32 - (*psEncCtrl).input_quality);
    }
    if (*psEnc).sCmn.indices.signalType as i32 == TYPE_VOICED {
        (*psEnc).sCmn.indices.quantOffsetType = 0;
    } else {
        nSamples = 2 * (*psEnc).sCmn.fs_kHz;
        energy_variation = 0.0f32;
        log_energy_prev = 0.0f32;
        pitch_res_ptr = pitch_res;
        nSegs = 5 * (*psEnc).sCmn.nb_subfr as i16 as i32 / 2;
        k = 0;
        while k < nSegs {
            nrg = nSamples as f32 + silk_energy_FLP(pitch_res_ptr, nSamples) as f32;
            log_energy = silk_log2(nrg as f64);
            if k > 0 {
                energy_variation += (log_energy - log_energy_prev).abs();
            }
            log_energy_prev = log_energy;
            pitch_res_ptr = pitch_res_ptr.offset(nSamples as isize);
            k += 1;
        }
        if energy_variation > ENERGY_VARIATION_THRESHOLD_QNT_OFFSET * (nSegs - 1) as f32 {
            (*psEnc).sCmn.indices.quantOffsetType = 0;
        } else {
            (*psEnc).sCmn.indices.quantOffsetType = 1;
        }
    }
    strength = FIND_PITCH_WHITE_NOISE_FRACTION * (*psEncCtrl).predGain;
    BWExp = BANDWIDTH_EXPANSION / (1.0f32 + strength * strength);
    warping = (*psEnc).sCmn.warping_Q16 as f32 / 65536.0f32 + 0.01f32 * (*psEncCtrl).coding_quality;
    k = 0;
    while k < (*psEnc).sCmn.nb_subfr {
        let mut shift: i32 = 0;
        let mut slope_part: i32 = 0;
        let mut flat_part: i32 = 0;
        flat_part = (*psEnc).sCmn.fs_kHz * 3;
        slope_part = ((*psEnc).sCmn.shapeWinLength - flat_part) / 2;
        silk_apply_sine_window_FLP(x_windowed.as_mut_ptr(), x_ptr, 1, slope_part);
        shift = slope_part;
        memcpy(
            x_windowed.as_mut_ptr().offset(shift as isize) as *mut core::ffi::c_void,
            x_ptr.offset(shift as isize) as *const core::ffi::c_void,
            (flat_part as u64).wrapping_mul(::core::mem::size_of::<f32>() as u64),
        );
        shift += flat_part;
        silk_apply_sine_window_FLP(
            x_windowed.as_mut_ptr().offset(shift as isize),
            x_ptr.offset(shift as isize),
            2,
            slope_part,
        );
        x_ptr = x_ptr.offset((*psEnc).sCmn.subfr_length as isize);
        if (*psEnc).sCmn.warping_Q16 > 0 {
            silk_warped_autocorrelation_FLP(
                auto_corr.as_mut_ptr(),
                x_windowed.as_mut_ptr(),
                warping,
                (*psEnc).sCmn.shapeWinLength,
                (*psEnc).sCmn.shapingLPCOrder,
            );
        } else {
            silk_autocorrelation_FLP(
                auto_corr.as_mut_ptr(),
                x_windowed.as_mut_ptr(),
                (*psEnc).sCmn.shapeWinLength,
                (*psEnc).sCmn.shapingLPCOrder + 1,
            );
        }
        auto_corr[0 as usize] += auto_corr[0 as usize] * SHAPE_WHITE_NOISE_FRACTION + 1.0f32;
        nrg = silk_schur_FLP(
            rc.as_mut_ptr(),
            auto_corr.as_mut_ptr() as *const f32,
            (*psEnc).sCmn.shapingLPCOrder,
        );
        silk_k2a_FLP(
            &mut *((*psEncCtrl).AR)
                .as_mut_ptr()
                .offset((k * MAX_SHAPE_LPC_ORDER) as isize),
            rc.as_mut_ptr(),
            (*psEnc).sCmn.shapingLPCOrder,
        );
        (*psEncCtrl).Gains[k as usize] = celt_sqrt(nrg);
        if (*psEnc).sCmn.warping_Q16 > 0 {
            (*psEncCtrl).Gains[k as usize] *= warped_gain(
                &mut *((*psEncCtrl).AR)
                    .as_mut_ptr()
                    .offset((k * MAX_SHAPE_LPC_ORDER) as isize),
                warping,
                (*psEnc).sCmn.shapingLPCOrder,
            );
        }
        silk_bwexpander_FLP(
            &mut *((*psEncCtrl).AR)
                .as_mut_ptr()
                .offset((k * MAX_SHAPE_LPC_ORDER) as isize),
            (*psEnc).sCmn.shapingLPCOrder,
            BWExp,
        );
        if (*psEnc).sCmn.warping_Q16 > 0 {
            warped_true2monic_coefs(
                &mut *((*psEncCtrl).AR)
                    .as_mut_ptr()
                    .offset((k * MAX_SHAPE_LPC_ORDER) as isize),
                warping,
                3.999f32,
                (*psEnc).sCmn.shapingLPCOrder,
            );
        } else {
            limit_coefs(
                &mut *((*psEncCtrl).AR)
                    .as_mut_ptr()
                    .offset((k * MAX_SHAPE_LPC_ORDER) as isize),
                3.999f32,
                (*psEnc).sCmn.shapingLPCOrder,
            );
        }
        k += 1;
    }
    gain_mult = silk_exp2(-0.16f32 * SNR_adj_dB);
    gain_add = silk_exp2(0.16f32 * MIN_QGAIN_DB as f32);
    k = 0;
    while k < (*psEnc).sCmn.nb_subfr {
        (*psEncCtrl).Gains[k as usize] *= gain_mult;
        (*psEncCtrl).Gains[k as usize] += gain_add;
        k += 1;
    }
    strength = LOW_FREQ_SHAPING
        * (1.0f32
            + LOW_QUALITY_LOW_FREQ_SHAPING_DECR
                * ((*psEnc).sCmn.input_quality_bands_Q15[0 as usize] as f32
                    * (1.0f32 / 32768.0f32)
                    - 1.0f32));
    strength *= (*psEnc).sCmn.speech_activity_Q8 as f32 * (1.0f32 / 256.0f32);
    if (*psEnc).sCmn.indices.signalType as i32 == TYPE_VOICED {
        k = 0;
        while k < (*psEnc).sCmn.nb_subfr {
            b = 0.2f32 / (*psEnc).sCmn.fs_kHz as f32
                + 3.0f32 / (*psEncCtrl).pitchL[k as usize] as f32;
            (*psEncCtrl).LF_MA_shp[k as usize] = -1.0f32 + b;
            (*psEncCtrl).LF_AR_shp[k as usize] = 1.0f32 - b - b * strength;
            k += 1;
        }
        Tilt = -HP_NOISE_COEF
            - (1 as f32 - HP_NOISE_COEF)
                * HARM_HP_NOISE_COEF
                * (*psEnc).sCmn.speech_activity_Q8 as f32
                * (1.0f32 / 256.0f32);
    } else {
        b = 1.3f32 / (*psEnc).sCmn.fs_kHz as f32;
        (*psEncCtrl).LF_MA_shp[0 as usize] = -1.0f32 + b;
        (*psEncCtrl).LF_AR_shp[0 as usize] = 1.0f32 - b - b * strength * 0.6f32;
        k = 1;
        while k < (*psEnc).sCmn.nb_subfr {
            (*psEncCtrl).LF_MA_shp[k as usize] = (*psEncCtrl).LF_MA_shp[0 as usize];
            (*psEncCtrl).LF_AR_shp[k as usize] = (*psEncCtrl).LF_AR_shp[0 as usize];
            k += 1;
        }
        Tilt = -HP_NOISE_COEF;
    }
    if USE_HARM_SHAPING != 0 && (*psEnc).sCmn.indices.signalType as i32 == TYPE_VOICED {
        HarmShapeGain = HARMONIC_SHAPING;
        HarmShapeGain += HIGH_RATE_OR_LOW_QUALITY_HARMONIC_SHAPING
            * (1.0f32 - (1.0f32 - (*psEncCtrl).coding_quality) * (*psEncCtrl).input_quality);
        HarmShapeGain *= celt_sqrt((*psEnc).LTPCorr);
    } else {
        HarmShapeGain = 0.0f32;
    }
    k = 0;
    while k < (*psEnc).sCmn.nb_subfr {
        (*psShapeSt).HarmShapeGain_smth +=
            SUBFR_SMTH_COEF * (HarmShapeGain - (*psShapeSt).HarmShapeGain_smth);
        (*psEncCtrl).HarmShapeGain[k as usize] = (*psShapeSt).HarmShapeGain_smth;
        (*psShapeSt).Tilt_smth += SUBFR_SMTH_COEF * (Tilt - (*psShapeSt).Tilt_smth);
        (*psEncCtrl).Tilt[k as usize] = (*psShapeSt).Tilt_smth;
        k += 1;
    }
}
