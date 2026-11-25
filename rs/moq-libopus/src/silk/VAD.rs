pub mod typedef_h {
    pub const silk_uint8_MAX: i32 = 0xff;
    pub const silk_int32_MAX: i32 = i32::MAX;
}
pub use self::typedef_h::{silk_int32_MAX, silk_uint8_MAX};
use crate::externs::memset;
use crate::silk::ana_filt_bank_1::silk_ana_filt_bank_1;
use crate::silk::define::{
    VAD_INTERNAL_SUBFRAMES, VAD_NEGATIVE_OFFSET_Q5, VAD_NOISE_LEVEL_SMOOTH_COEF_Q16, VAD_N_BANDS,
};
use crate::silk::lin2log::silk_lin2log;
use crate::silk::sigm_Q15::silk_sigm_Q15;
use crate::silk::structs::{silk_VAD_state, silk_encoder_state};
use crate::silk::Inlines::silk_SQRT_APPROX;
use crate::silk::SigProc_FIX::{silk_max_32, silk_max_int, silk_min_int};

pub unsafe fn silk_VAD_Init(psSilk_VAD: *mut silk_VAD_state) -> i32 {
    let mut b: i32 = 0;
    let ret: i32 = 0;
    memset(
        psSilk_VAD as *mut core::ffi::c_void,
        0,
        ::core::mem::size_of::<silk_VAD_state>() as u64,
    );
    b = 0;
    while b < VAD_N_BANDS {
        (*psSilk_VAD).NoiseLevelBias[b as usize] = silk_max_32(50 / (b + 1), 1);
        b += 1;
    }
    b = 0;
    while b < VAD_N_BANDS {
        (*psSilk_VAD).NL[b as usize] = 100 * (*psSilk_VAD).NoiseLevelBias[b as usize];
        (*psSilk_VAD).inv_NL[b as usize] = 0x7fffffff / (*psSilk_VAD).NL[b as usize];
        b += 1;
    }
    (*psSilk_VAD).counter = 15;
    b = 0;
    while b < VAD_N_BANDS {
        (*psSilk_VAD).NrgRatioSmth_Q8[b as usize] = 100 * 256;
        b += 1;
    }
    return ret;
}
static mut tiltWeights: [i32; 4] = [30000, 6000, -(12000), -(12000)];
pub unsafe fn silk_VAD_GetSA_Q8_c(psEncC: *mut silk_encoder_state, pIn: *const i16) -> i32 {
    let mut SA_Q15: i32 = 0;
    let mut pSNR_dB_Q7: i32 = 0;
    let mut input_tilt: i32 = 0;
    let mut decimated_framelength1: i32 = 0;
    let mut decimated_framelength2: i32 = 0;
    let mut decimated_framelength: i32 = 0;
    let mut dec_subframe_length: i32 = 0;
    let mut dec_subframe_offset: i32 = 0;
    let mut SNR_Q7: i32 = 0;
    let mut i: i32 = 0;
    let mut b: i32 = 0;
    let mut s: i32 = 0;
    let mut sumSquared: i32 = 0;
    let mut smooth_coef_Q16: i32 = 0;
    let mut HPstateTmp: i16 = 0;
    let mut Xnrg: [i32; 4] = [0; 4];
    let mut NrgToNoiseRatio_Q8: [i32; 4] = [0; 4];
    let mut speech_nrg: i32 = 0;
    let mut x_tmp: i32 = 0;
    let mut X_offset: [i32; 4] = [0; 4];
    let ret: i32 = 0;
    let psSilk_VAD: *mut silk_VAD_state = &mut (*psEncC).sVAD;
    assert!(5 * 4 * 16 >= (*psEncC).frame_length);
    assert!((*psEncC).frame_length <= 512);
    assert!((*psEncC).frame_length == 8 * ((*psEncC).frame_length >> 3));
    decimated_framelength1 = (*psEncC).frame_length >> 1;
    decimated_framelength2 = (*psEncC).frame_length >> 2;
    decimated_framelength = (*psEncC).frame_length >> 3;
    X_offset[0 as usize] = 0;
    X_offset[1 as usize] = decimated_framelength + decimated_framelength2;
    X_offset[2 as usize] = X_offset[1 as usize] + decimated_framelength;
    X_offset[3 as usize] = X_offset[2 as usize] + decimated_framelength2;
    let vla = (X_offset[3 as usize] + decimated_framelength1) as usize;
    let mut X: Vec<i16> = ::std::vec::from_elem(0, vla);
    silk_ana_filt_bank_1(
        pIn,
        &mut *((*psSilk_VAD).AnaState).as_mut_ptr().offset(0 as isize),
        X.as_mut_ptr(),
        &mut *X
            .as_mut_ptr()
            .offset(*X_offset.as_mut_ptr().offset(3 as isize) as isize),
        (*psEncC).frame_length,
    );
    silk_ana_filt_bank_1(
        X.as_mut_ptr(),
        &mut *((*psSilk_VAD).AnaState1).as_mut_ptr().offset(0 as isize),
        X.as_mut_ptr(),
        &mut *X
            .as_mut_ptr()
            .offset(*X_offset.as_mut_ptr().offset(2 as isize) as isize),
        decimated_framelength1,
    );
    silk_ana_filt_bank_1(
        X.as_mut_ptr(),
        &mut *((*psSilk_VAD).AnaState2).as_mut_ptr().offset(0 as isize),
        X.as_mut_ptr(),
        &mut *X
            .as_mut_ptr()
            .offset(*X_offset.as_mut_ptr().offset(1 as isize) as isize),
        decimated_framelength2,
    );
    *X.as_mut_ptr().offset((decimated_framelength - 1) as isize) =
        (*X.as_mut_ptr().offset((decimated_framelength - 1) as isize) as i32 >> 1) as i16;
    HPstateTmp = *X.as_mut_ptr().offset((decimated_framelength - 1) as isize);
    i = decimated_framelength - 1;
    while i > 0 {
        *X.as_mut_ptr().offset((i - 1) as isize) =
            (*X.as_mut_ptr().offset((i - 1) as isize) as i32 >> 1) as i16;
        let ref mut fresh0 = *X.as_mut_ptr().offset(i as isize);
        *fresh0 = (*fresh0 as i32 - *X.as_mut_ptr().offset((i - 1) as isize) as i32) as i16;
        i -= 1;
    }
    let ref mut fresh1 = *X.as_mut_ptr().offset(0 as isize);
    *fresh1 = (*fresh1 as i32 - (*psSilk_VAD).HPstate as i32) as i16;
    (*psSilk_VAD).HPstate = HPstateTmp;
    b = 0;
    while b < VAD_N_BANDS {
        decimated_framelength = (*psEncC).frame_length >> silk_min_int(4 - b, 4 - 1);
        dec_subframe_length = decimated_framelength >> 2;
        dec_subframe_offset = 0;
        Xnrg[b as usize] = (*psSilk_VAD).XnrgSubfr[b as usize];
        s = 0;
        while s < VAD_INTERNAL_SUBFRAMES {
            sumSquared = 0;
            i = 0;
            while i < dec_subframe_length {
                x_tmp = *X
                    .as_mut_ptr()
                    .offset((X_offset[b as usize] + i + dec_subframe_offset) as isize)
                    as i32
                    >> 3;
                sumSquared = sumSquared + x_tmp as i16 as i32 * x_tmp as i16 as i32;
                i += 1;
            }
            if s < VAD_INTERNAL_SUBFRAMES - 1 {
                Xnrg[b as usize] = if (Xnrg[b as usize] as u32).wrapping_add(sumSquared as u32)
                    & 0x80000000 as u32
                    != 0
                {
                    silk_int32_MAX
                } else {
                    Xnrg[b as usize] + sumSquared
                };
            } else {
                Xnrg[b as usize] = if (Xnrg[b as usize] as u32)
                    .wrapping_add((sumSquared >> 1) as u32)
                    & 0x80000000 as u32
                    != 0
                {
                    silk_int32_MAX
                } else {
                    Xnrg[b as usize] + (sumSquared >> 1)
                };
            }
            dec_subframe_offset += dec_subframe_length;
            s += 1;
        }
        (*psSilk_VAD).XnrgSubfr[b as usize] = sumSquared;
        b += 1;
    }
    silk_VAD_GetNoiseLevels(
        &mut *Xnrg.as_mut_ptr().offset(0 as isize) as *mut i32 as *const i32,
        psSilk_VAD,
    );
    sumSquared = 0;
    input_tilt = 0;
    b = 0;
    while b < VAD_N_BANDS {
        speech_nrg = Xnrg[b as usize] - (*psSilk_VAD).NL[b as usize];
        if speech_nrg > 0 {
            if Xnrg[b as usize] as u32 & 0xff800000 as u32 == 0 {
                NrgToNoiseRatio_Q8[b as usize] =
                    ((Xnrg[b as usize] as u32) << 8) as i32 / ((*psSilk_VAD).NL[b as usize] + 1);
            } else {
                NrgToNoiseRatio_Q8[b as usize] =
                    Xnrg[b as usize] / (((*psSilk_VAD).NL[b as usize] >> 8) + 1);
            }
            SNR_Q7 = silk_lin2log(NrgToNoiseRatio_Q8[b as usize]) - 8 * 128;
            sumSquared = sumSquared + SNR_Q7 as i16 as i32 * SNR_Q7 as i16 as i32;
            if speech_nrg < (1) << 20 {
                SNR_Q7 = (((silk_SQRT_APPROX(speech_nrg) as u32) << 6) as i32 as i64
                    * SNR_Q7 as i16 as i64
                    >> 16) as i32;
            }
            input_tilt = (input_tilt as i64
                + (tiltWeights[b as usize] as i64 * SNR_Q7 as i16 as i64 >> 16))
                as i32;
        } else {
            NrgToNoiseRatio_Q8[b as usize] = 256;
        }
        b += 1;
    }
    sumSquared = sumSquared / 4;
    pSNR_dB_Q7 = (3 * silk_SQRT_APPROX(sumSquared)) as i16 as i32;
    SA_Q15 =
        silk_sigm_Q15((45000 * pSNR_dB_Q7 as i16 as i64 >> 16) as i32 - VAD_NEGATIVE_OFFSET_Q5);
    (*psEncC).input_tilt_Q15 = (((silk_sigm_Q15(input_tilt) - 16384) as u32) << 1) as i32;
    speech_nrg = 0;
    b = 0;
    while b < VAD_N_BANDS {
        speech_nrg += (b + 1) * (Xnrg[b as usize] - (*psSilk_VAD).NL[b as usize] >> 4);
        b += 1;
    }
    if (*psEncC).frame_length == 20 * (*psEncC).fs_kHz {
        speech_nrg = speech_nrg >> 1;
    }
    if speech_nrg <= 0 {
        SA_Q15 = SA_Q15 >> 1;
    } else if speech_nrg < 16384 {
        speech_nrg = ((speech_nrg as u32) << 16) as i32;
        speech_nrg = silk_SQRT_APPROX(speech_nrg);
        SA_Q15 = ((32768 + speech_nrg) as i64 * SA_Q15 as i16 as i64 >> 16) as i32;
    }
    (*psEncC).speech_activity_Q8 = silk_min_int(SA_Q15 >> 7, silk_uint8_MAX);
    smooth_coef_Q16 =
        (4096 * (SA_Q15 as i64 * SA_Q15 as i16 as i64 >> 16) as i32 as i16 as i64 >> 16) as i32;
    if (*psEncC).frame_length == 10 * (*psEncC).fs_kHz {
        smooth_coef_Q16 >>= 1;
    }
    b = 0;
    while b < VAD_N_BANDS {
        (*psSilk_VAD).NrgRatioSmth_Q8[b as usize] = ((*psSilk_VAD).NrgRatioSmth_Q8[b as usize]
            as i64
            + ((NrgToNoiseRatio_Q8[b as usize] - (*psSilk_VAD).NrgRatioSmth_Q8[b as usize]) as i64
                * smooth_coef_Q16 as i16 as i64
                >> 16)) as i32;
        SNR_Q7 = 3 * (silk_lin2log((*psSilk_VAD).NrgRatioSmth_Q8[b as usize]) - 8 * 128);
        (*psEncC).input_quality_bands_Q15[b as usize] = silk_sigm_Q15(SNR_Q7 - 16 * 128 >> 4);
        b += 1;
    }
    return ret;
}
#[inline]
unsafe fn silk_VAD_GetNoiseLevels(pX: *const i32, psSilk_VAD: *mut silk_VAD_state) {
    let mut k: i32 = 0;
    let mut nl: i32 = 0;
    let mut nrg: i32 = 0;
    let mut inv_nrg: i32 = 0;
    let mut coef: i32 = 0;
    let mut min_coef: i32 = 0;
    if (*psSilk_VAD).counter < 1000 {
        min_coef = 0x7fff / (((*psSilk_VAD).counter >> 4) + 1);
        (*psSilk_VAD).counter += 1;
    } else {
        min_coef = 0;
    }
    k = 0;
    while k < VAD_N_BANDS {
        nl = (*psSilk_VAD).NL[k as usize];
        nrg = if (*pX.offset(k as isize) as u32)
            .wrapping_add((*psSilk_VAD).NoiseLevelBias[k as usize] as u32)
            & 0x80000000 as u32
            != 0
        {
            silk_int32_MAX
        } else {
            *pX.offset(k as isize) + (*psSilk_VAD).NoiseLevelBias[k as usize]
        };
        inv_nrg = 0x7fffffff / nrg;
        if nrg > ((nl as u32) << 3) as i32 {
            coef = VAD_NOISE_LEVEL_SMOOTH_COEF_Q16 >> 3;
        } else if nrg < nl {
            coef = VAD_NOISE_LEVEL_SMOOTH_COEF_Q16;
        } else {
            coef = ((inv_nrg as i64 * nl as i64 >> 16) as i32 as i64 * ((1024) << 1) as i16 as i64
                >> 16) as i32;
        }
        coef = silk_max_int(coef, min_coef);
        (*psSilk_VAD).inv_NL[k as usize] = ((*psSilk_VAD).inv_NL[k as usize] as i64
            + ((inv_nrg - (*psSilk_VAD).inv_NL[k as usize]) as i64 * coef as i16 as i64 >> 16))
            as i32;
        nl = 0x7fffffff / (*psSilk_VAD).inv_NL[k as usize];
        nl = if nl < 0xffffff { nl } else { 0xffffff };
        (*psSilk_VAD).NL[k as usize] = nl;
        k += 1;
    }
}
