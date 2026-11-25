pub mod arch_h {
    pub type opus_val32 = f32;
}
pub mod xmmintrin_h {
    #[cfg(target_arch = "x86")]
    pub use core::arch::x86::{__m128, _mm_cvt_ss2si, _mm_cvtss_si32, _mm_set_ss};
    #[cfg(target_arch = "x86_64")]
    pub use core::arch::x86_64::{__m128, _mm_cvt_ss2si, _mm_cvtss_si32, _mm_set_ss};
}
pub mod SigProc_FLP_h {
    #[inline]
    pub unsafe fn silk_float2short_array(out: *mut i16, in_0: *const f32, length: i32) {
        let mut k: i32 = 0;
        k = length - 1;
        while k >= 0 {
            *out.offset(k as isize) = (if float2int(*in_0.offset(k as isize)) > silk_int16_MAX {
                silk_int16_MAX
            } else if float2int(*in_0.offset(k as isize)) < silk_int16_MIN {
                silk_int16_MIN
            } else {
                float2int(*in_0.offset(k as isize))
            }) as i16;
            k -= 1;
        }
    }
    #[inline]
    pub unsafe fn silk_short2float_array(out: *mut f32, in_0: *const i16, length: i32) {
        let mut k: i32 = 0;
        k = length - 1;
        while k >= 0 {
            *out.offset(k as isize) = *in_0.offset(k as isize) as f32;
            k -= 1;
        }
    }
    #[inline]
    pub unsafe fn silk_log2(x: f64) -> f32 {
        return (3.32192809488736f64 * x.log10()) as f32;
    }
    use super::typedef_h::{silk_int16_MAX, silk_int16_MIN};
    use crate::celt::float_cast::float2int;
}
pub mod typedef_h {
    pub const silk_int16_MAX: i32 = i16::MAX as i32;
    pub const silk_int16_MIN: i32 = i16::MIN as i32;
}

use self::arch_h::opus_val32;
pub use self::typedef_h::{silk_int16_MAX, silk_int16_MIN};
pub use self::SigProc_FLP_h::{silk_float2short_array, silk_log2, silk_short2float_array};
use crate::celt::pitch::celt_pitch_xcorr_c;
use crate::externs::memset;
use crate::silk::float::energy_FLP::silk_energy_FLP;
use crate::silk::float::inner_product_FLP::silk_inner_product_FLP;
use crate::silk::float::sort_FLP::silk_insertion_sort_decreasing_FLP;
use crate::silk::pitch_est_tables::{
    silk_CB_lags_stage2, silk_CB_lags_stage2_10_ms, silk_CB_lags_stage3, silk_CB_lags_stage3_10_ms,
    silk_Lag_range_stage3, silk_Lag_range_stage3_10_ms, silk_nb_cbk_searchs_stage3,
    PE_FLATCONTOUR_BIAS, PE_LTP_MEM_LENGTH_MS, PE_MAX_LAG_MS, PE_MAX_NB_SUBFR, PE_MIN_LAG_MS,
    PE_NB_CBKS_STAGE2, PE_NB_CBKS_STAGE2_10MS, PE_NB_CBKS_STAGE2_EXT, PE_NB_CBKS_STAGE3_10MS,
    PE_NB_CBKS_STAGE3_MAX, PE_NB_STAGE3_LAGS, PE_PREVLAG_BIAS, PE_SHORTLAG_BIAS,
    PE_SUBFR_LENGTH_MS, SILK_PE_MIN_COMPLEX,
};
use crate::silk::resampler::{silk_resampler_down2, silk_resampler_down2_3};
use crate::silk::SigProc_FIX::{silk_max_int, silk_min_int};
use arrayref::array_mut_ref;

pub unsafe fn silk_pitch_analysis_core_FLP(
    frame: *const f32,
    pitch_out: *mut i32,
    lagIndex: *mut i16,
    contourIndex: *mut i8,
    LTPCorr: *mut f32,
    mut prevLag: i32,
    search_thres1: f32,
    search_thres2: f32,
    Fs_kHz: i32,
    complexity: i32,
    nb_subfr: i32,
    arch: i32,
) -> i32 {
    let mut i: i32 = 0;
    let mut k: i32 = 0;
    let mut d: i32 = 0;
    let mut j: i32 = 0;
    let mut frame_8kHz: [f32; 320] = [0.; 320];
    let mut frame_4kHz: [f32; 160] = [0.; 160];
    let mut frame_8_FIX: [i16; 320] = [0; 320];
    let mut frame_4_FIX: [i16; 160] = [0; 160];
    let mut filt_state: [i32; 6] = [0; 6];
    let mut threshold: f32 = 0.;
    let mut contour_bias: f32 = 0.;
    let mut C: [[f32; 149]; 4] = [[0.; 149]; 4];
    let mut xcorr: [opus_val32; 65] = [0.; 65];
    let mut CC: [f32; 11] = [0.; 11];
    let mut target_ptr: *const f32 = 0 as *const f32;
    let mut basis_ptr: *const f32 = 0 as *const f32;
    let mut cross_corr: f64 = 0.;
    let mut normalizer: f64 = 0.;
    let mut energy: f64 = 0.;
    let mut energy_tmp: f64 = 0.;
    let mut d_srch: [i32; 24] = [0; 24];
    let mut d_comp: [i16; 149] = [0; 149];
    let mut length_d_srch: i32 = 0;
    let mut length_d_comp: i32 = 0;
    let mut Cmax: f32 = 0.;
    let mut CCmax: f32 = 0.;
    let mut CCmax_b: f32 = 0.;
    let mut CCmax_new_b: f32 = 0.;
    let mut CCmax_new: f32 = 0.;
    let mut CBimax: i32 = 0;
    let mut CBimax_new: i32 = 0;
    let mut lag: i32 = 0;
    let mut start_lag: i32 = 0;
    let mut end_lag: i32 = 0;
    let mut lag_new: i32 = 0;
    let mut cbk_size: i32 = 0;
    let mut lag_log2: f32 = 0.;
    let mut prevLag_log2: f32 = 0.;
    let mut delta_lag_log2_sqr: f32 = 0.;
    let mut energies_st3: [[[f32; 5]; 34]; 4] = [[[0.; 5]; 34]; 4];
    let mut cross_corr_st3: [[[f32; 5]; 34]; 4] = [[[0.; 5]; 34]; 4];
    let mut lag_counter: i32 = 0;
    let mut frame_length: i32 = 0;
    let mut frame_length_8kHz: i32 = 0;
    let mut frame_length_4kHz: i32 = 0;
    let mut sf_length: i32 = 0;
    let mut sf_length_8kHz: i32 = 0;
    let mut sf_length_4kHz: i32 = 0;
    let mut min_lag: i32 = 0;
    let mut min_lag_8kHz: i32 = 0;
    let mut min_lag_4kHz: i32 = 0;
    let mut max_lag: i32 = 0;
    let mut max_lag_8kHz: i32 = 0;
    let mut max_lag_4kHz: i32 = 0;
    let mut nb_cbk_search: i32 = 0;
    let mut Lag_CB_ptr: *const i8 = 0 as *const i8;
    assert!(Fs_kHz == 8 || Fs_kHz == 12 || Fs_kHz == 16);
    assert!(complexity >= 0);
    assert!(complexity <= 2);
    frame_length = (PE_LTP_MEM_LENGTH_MS + nb_subfr * PE_SUBFR_LENGTH_MS) * Fs_kHz;
    frame_length_4kHz = (PE_LTP_MEM_LENGTH_MS + nb_subfr * PE_SUBFR_LENGTH_MS) * 4;
    frame_length_8kHz = (PE_LTP_MEM_LENGTH_MS + nb_subfr * PE_SUBFR_LENGTH_MS) * 8;
    sf_length = PE_SUBFR_LENGTH_MS * Fs_kHz;
    sf_length_4kHz = PE_SUBFR_LENGTH_MS * 4;
    sf_length_8kHz = PE_SUBFR_LENGTH_MS * 8;
    min_lag = PE_MIN_LAG_MS * Fs_kHz;
    min_lag_4kHz = PE_MIN_LAG_MS * 4;
    min_lag_8kHz = PE_MIN_LAG_MS * 8;
    max_lag = PE_MAX_LAG_MS * Fs_kHz - 1;
    max_lag_4kHz = PE_MAX_LAG_MS * 4;
    max_lag_8kHz = PE_MAX_LAG_MS * 8 - 1;
    if Fs_kHz == 16 {
        let mut frame_16_FIX: [i16; 640] = [0; 640];
        silk_float2short_array(frame_16_FIX.as_mut_ptr(), frame, frame_length);
        let filt_state = array_mut_ref![filt_state, 0, 2];
        filt_state.fill(0);
        silk_resampler_down2(
            filt_state,
            &mut frame_8_FIX[..frame_length_8kHz as usize],
            &frame_16_FIX[..frame_length as usize],
        );
        silk_short2float_array(
            frame_8kHz.as_mut_ptr(),
            frame_8_FIX.as_mut_ptr(),
            frame_length_8kHz,
        );
    } else if Fs_kHz == 12 {
        let mut frame_12_FIX: [i16; 480] = [0; 480];
        silk_float2short_array(frame_12_FIX.as_mut_ptr(), frame, frame_length);
        filt_state.fill(0);
        silk_resampler_down2_3(
            &mut filt_state,
            &mut frame_8_FIX[..frame_length_8kHz as usize],
            &frame_12_FIX[..frame_length as usize],
        );
        silk_short2float_array(
            frame_8kHz.as_mut_ptr(),
            frame_8_FIX.as_mut_ptr(),
            frame_length_8kHz,
        );
    } else {
        assert!(Fs_kHz == 8);
        silk_float2short_array(frame_8_FIX.as_mut_ptr(), frame, frame_length_8kHz);
    }
    {
        let filt_state = array_mut_ref![filt_state, 0, 2];
        filt_state.fill(0);
        silk_resampler_down2(
            filt_state,
            &mut frame_4_FIX[..frame_length_4kHz as usize],
            &frame_8_FIX[..frame_length_8kHz as usize],
        );
    }
    silk_short2float_array(
        frame_4kHz.as_mut_ptr(),
        frame_4_FIX.as_mut_ptr(),
        frame_length_4kHz,
    );
    i = frame_length_4kHz - 1;
    while i > 0 {
        frame_4kHz[i as usize] = (if frame_4kHz[i as usize] as i32 as f32
            + frame_4kHz[(i - 1) as usize]
            > silk_int16_MAX as f32
        {
            silk_int16_MAX as f32
        } else if frame_4kHz[i as usize] as i32 as f32 + frame_4kHz[(i - 1) as usize]
            < silk_int16_MIN as f32
        {
            silk_int16_MIN as f32
        } else {
            frame_4kHz[i as usize] as i32 as f32 + frame_4kHz[(i - 1) as usize]
        }) as i16 as f32;
        i -= 1;
    }
    memset(
        C.as_mut_ptr() as *mut core::ffi::c_void,
        0,
        (::core::mem::size_of::<f32>() as u64)
            .wrapping_mul(nb_subfr as u64)
            .wrapping_mul(((18 * 16 >> 1) + 5) as u64),
    );
    target_ptr = &mut *frame_4kHz
        .as_mut_ptr()
        .offset(((sf_length_4kHz as u32) << 2) as i32 as isize) as *mut f32;
    k = 0;
    while k < nb_subfr >> 1 {
        assert!(target_ptr >= frame_4kHz.as_mut_ptr() as *const f32);
        assert!(
            target_ptr.offset(sf_length_8kHz as isize)
                <= frame_4kHz.as_mut_ptr().offset(frame_length_4kHz as isize) as *const f32
        );
        basis_ptr = target_ptr.offset(-(min_lag_4kHz as isize));
        assert!(basis_ptr >= frame_4kHz.as_mut_ptr() as *const f32);
        assert!(
            basis_ptr.offset(sf_length_8kHz as isize)
                <= frame_4kHz.as_mut_ptr().offset(frame_length_4kHz as isize) as *const f32
        );
        celt_pitch_xcorr_c(
            target_ptr,
            target_ptr.offset(-(max_lag_4kHz as isize)),
            xcorr.as_mut_ptr(),
            sf_length_8kHz,
            max_lag_4kHz - min_lag_4kHz + 1,
            arch,
        );
        cross_corr = xcorr[(max_lag_4kHz - min_lag_4kHz) as usize] as f64;
        normalizer = silk_energy_FLP(target_ptr, sf_length_8kHz)
            + silk_energy_FLP(basis_ptr, sf_length_8kHz)
            + (sf_length_8kHz as f32 * 4000.0f32) as f64;
        C[0 as usize][min_lag_4kHz as usize] += (2 as f64 * cross_corr / normalizer) as f32;
        d = min_lag_4kHz + 1;
        while d <= max_lag_4kHz {
            basis_ptr = basis_ptr.offset(-1);
            cross_corr = xcorr[(max_lag_4kHz - d) as usize] as f64;
            normalizer += *basis_ptr.offset(0 as isize) as f64
                * *basis_ptr.offset(0 as isize) as f64
                - *basis_ptr.offset(sf_length_8kHz as isize) as f64
                    * *basis_ptr.offset(sf_length_8kHz as isize) as f64;
            C[0 as usize][d as usize] += (2 as f64 * cross_corr / normalizer) as f32;
            d += 1;
        }
        target_ptr = target_ptr.offset(sf_length_8kHz as isize);
        k += 1;
    }
    i = max_lag_4kHz;
    while i >= min_lag_4kHz {
        C[0 as usize][i as usize] -= C[0 as usize][i as usize] * i as f32 / 4096.0f32;
        i -= 1;
    }
    length_d_srch = 4 + 2 * complexity;
    assert!(3 * length_d_srch <= 24);
    silk_insertion_sort_decreasing_FLP(
        &mut *(*C.as_mut_ptr().offset(0 as isize))
            .as_mut_ptr()
            .offset(min_lag_4kHz as isize),
        d_srch.as_mut_ptr(),
        max_lag_4kHz - min_lag_4kHz + 1,
        length_d_srch,
    );
    Cmax = C[0 as usize][min_lag_4kHz as usize];
    if Cmax < 0.2f32 {
        memset(
            pitch_out as *mut core::ffi::c_void,
            0,
            (nb_subfr as u64).wrapping_mul(::core::mem::size_of::<i32>() as u64),
        );
        *LTPCorr = 0.0f32;
        *lagIndex = 0;
        *contourIndex = 0;
        return 1;
    }
    threshold = search_thres1 * Cmax;
    i = 0;
    while i < length_d_srch {
        if C[0 as usize][(min_lag_4kHz + i) as usize] > threshold {
            d_srch[i as usize] = (((d_srch[i as usize] + min_lag_4kHz) as u32) << 1) as i32;
            i += 1;
        } else {
            length_d_srch = i;
            break;
        }
    }
    assert!(length_d_srch > 0);
    i = min_lag_8kHz - 5;
    while i < max_lag_8kHz + 5 {
        d_comp[i as usize] = 0;
        i += 1;
    }
    i = 0;
    while i < length_d_srch {
        d_comp[d_srch[i as usize] as usize] = 1;
        i += 1;
    }
    i = max_lag_8kHz + 3;
    while i >= min_lag_8kHz {
        d_comp[i as usize] = (d_comp[i as usize] as i32
            + (d_comp[(i - 1) as usize] as i32 + d_comp[(i - 2) as usize] as i32))
            as i16;
        i -= 1;
    }
    length_d_srch = 0;
    i = min_lag_8kHz;
    while i < max_lag_8kHz + 1 {
        if d_comp[(i + 1) as usize] as i32 > 0 {
            d_srch[length_d_srch as usize] = i;
            length_d_srch += 1;
        }
        i += 1;
    }
    i = max_lag_8kHz + 3;
    while i >= min_lag_8kHz {
        d_comp[i as usize] = (d_comp[i as usize] as i32
            + (d_comp[(i - 1) as usize] as i32
                + d_comp[(i - 2) as usize] as i32
                + d_comp[(i - 3) as usize] as i32)) as i16;
        i -= 1;
    }
    length_d_comp = 0;
    i = min_lag_8kHz;
    while i < max_lag_8kHz + 4 {
        if d_comp[i as usize] as i32 > 0 {
            d_comp[length_d_comp as usize] = (i - 2) as i16;
            length_d_comp += 1;
        }
        i += 1;
    }
    memset(
        C.as_mut_ptr() as *mut core::ffi::c_void,
        0,
        ((4 * ((18 * 16 >> 1) + 5)) as u64).wrapping_mul(::core::mem::size_of::<f32>() as u64),
    );
    if Fs_kHz == 8 {
        target_ptr = &*frame.offset((PE_LTP_MEM_LENGTH_MS * 8) as isize) as *const f32;
    } else {
        target_ptr = &mut *frame_8kHz
            .as_mut_ptr()
            .offset((PE_LTP_MEM_LENGTH_MS * 8) as isize) as *mut f32;
    }
    k = 0;
    while k < nb_subfr {
        energy_tmp = silk_energy_FLP(target_ptr, sf_length_8kHz) + 1.0f64;
        j = 0;
        while j < length_d_comp {
            d = d_comp[j as usize] as i32;
            basis_ptr = target_ptr.offset(-(d as isize));
            cross_corr = silk_inner_product_FLP(basis_ptr, target_ptr, sf_length_8kHz);
            if cross_corr > 0.0f32 as f64 {
                energy = silk_energy_FLP(basis_ptr, sf_length_8kHz);
                C[k as usize][d as usize] = (2 as f64 * cross_corr / (energy + energy_tmp)) as f32;
            } else {
                C[k as usize][d as usize] = 0.0f32;
            }
            j += 1;
        }
        target_ptr = target_ptr.offset(sf_length_8kHz as isize);
        k += 1;
    }
    CCmax = 0.0f32;
    CCmax_b = -1000.0f32;
    CBimax = 0;
    lag = -1;
    if prevLag > 0 {
        if Fs_kHz == 12 {
            prevLag = ((prevLag as u32) << 1) as i32 / 3;
        } else if Fs_kHz == 16 {
            prevLag = prevLag >> 1;
        }
        prevLag_log2 = silk_log2(prevLag as f32 as f64);
    } else {
        prevLag_log2 = 0 as f32;
    }
    if nb_subfr == PE_MAX_NB_SUBFR {
        cbk_size = PE_NB_CBKS_STAGE2_EXT;
        Lag_CB_ptr = &*(*silk_CB_lags_stage2.as_ptr().offset(0 as isize))
            .as_ptr()
            .offset(0 as isize) as *const i8;
        if Fs_kHz == 8 && complexity > SILK_PE_MIN_COMPLEX {
            nb_cbk_search = PE_NB_CBKS_STAGE2_EXT;
        } else {
            nb_cbk_search = PE_NB_CBKS_STAGE2;
        }
    } else {
        cbk_size = PE_NB_CBKS_STAGE2_10MS;
        Lag_CB_ptr = &*(*silk_CB_lags_stage2_10_ms.as_ptr().offset(0 as isize))
            .as_ptr()
            .offset(0 as isize) as *const i8;
        nb_cbk_search = PE_NB_CBKS_STAGE2_10MS;
    }
    k = 0;
    while k < length_d_srch {
        d = d_srch[k as usize];
        j = 0;
        while j < nb_cbk_search {
            CC[j as usize] = 0.0f32;
            i = 0;
            while i < nb_subfr {
                CC[j as usize] += C[i as usize]
                    [(d + *Lag_CB_ptr.offset((i * cbk_size + j) as isize) as i32) as usize];
                i += 1;
            }
            j += 1;
        }
        CCmax_new = -1000.0f32;
        CBimax_new = 0;
        i = 0;
        while i < nb_cbk_search {
            if CC[i as usize] > CCmax_new {
                CCmax_new = CC[i as usize];
                CBimax_new = i;
            }
            i += 1;
        }
        lag_log2 = silk_log2(d as f32 as f64);
        CCmax_new_b = CCmax_new - PE_SHORTLAG_BIAS * nb_subfr as f32 * lag_log2;
        if prevLag > 0 {
            delta_lag_log2_sqr = lag_log2 - prevLag_log2;
            delta_lag_log2_sqr *= delta_lag_log2_sqr;
            CCmax_new_b -= PE_PREVLAG_BIAS * nb_subfr as f32 * *LTPCorr * delta_lag_log2_sqr
                / (delta_lag_log2_sqr + 0.5f32);
        }
        if CCmax_new_b > CCmax_b && CCmax_new > nb_subfr as f32 * search_thres2 {
            CCmax_b = CCmax_new_b;
            CCmax = CCmax_new;
            lag = d;
            CBimax = CBimax_new;
        }
        k += 1;
    }
    if lag == -1 {
        memset(
            pitch_out as *mut core::ffi::c_void,
            0,
            4_u64.wrapping_mul(::core::mem::size_of::<i32>() as u64),
        );
        *LTPCorr = 0.0f32;
        *lagIndex = 0;
        *contourIndex = 0;
        return 1;
    }
    *LTPCorr = CCmax / nb_subfr as f32;
    if Fs_kHz > 8 {
        if Fs_kHz == 12 {
            lag = if 1 == 1 {
                (lag as i16 as i32 * 3 >> 1) + (lag as i16 as i32 * 3 & 1)
            } else {
                (lag as i16 as i32 * 3 >> 1 - 1) + 1 >> 1
            };
        } else {
            lag = ((lag as u32) << 1) as i32;
        }
        lag = if min_lag > max_lag {
            if lag > min_lag {
                min_lag
            } else if lag < max_lag {
                max_lag
            } else {
                lag
            }
        } else if lag > max_lag {
            max_lag
        } else if lag < min_lag {
            min_lag
        } else {
            lag
        };
        start_lag = silk_max_int(lag - 2, min_lag);
        end_lag = silk_min_int(lag + 2, max_lag);
        lag_new = lag;
        CBimax = 0;
        CCmax = -1000.0f32;
        silk_P_Ana_calc_corr_st3(
            cross_corr_st3.as_mut_ptr(),
            frame,
            start_lag,
            sf_length,
            nb_subfr,
            complexity,
            arch,
        );
        silk_P_Ana_calc_energy_st3(
            energies_st3.as_mut_ptr(),
            frame,
            start_lag,
            sf_length,
            nb_subfr,
            complexity,
        );
        lag_counter = 0;
        contour_bias = PE_FLATCONTOUR_BIAS / lag as f32;
        if nb_subfr == PE_MAX_NB_SUBFR {
            nb_cbk_search = silk_nb_cbk_searchs_stage3[complexity as usize] as i32;
            cbk_size = PE_NB_CBKS_STAGE3_MAX;
            Lag_CB_ptr = &*(*silk_CB_lags_stage3.as_ptr().offset(0 as isize))
                .as_ptr()
                .offset(0 as isize) as *const i8;
        } else {
            nb_cbk_search = PE_NB_CBKS_STAGE3_10MS;
            cbk_size = PE_NB_CBKS_STAGE3_10MS;
            Lag_CB_ptr = &*(*silk_CB_lags_stage3_10_ms.as_ptr().offset(0 as isize))
                .as_ptr()
                .offset(0 as isize) as *const i8;
        }
        target_ptr = &*frame.offset((PE_LTP_MEM_LENGTH_MS * Fs_kHz) as isize) as *const f32;
        energy_tmp = silk_energy_FLP(target_ptr, nb_subfr * sf_length) + 1.0f64;
        d = start_lag;
        while d <= end_lag {
            j = 0;
            while j < nb_cbk_search {
                cross_corr = 0.0f64;
                energy = energy_tmp;
                k = 0;
                while k < nb_subfr {
                    cross_corr +=
                        cross_corr_st3[k as usize][j as usize][lag_counter as usize] as f64;
                    energy += energies_st3[k as usize][j as usize][lag_counter as usize] as f64;
                    k += 1;
                }
                if cross_corr > 0.0f64 {
                    CCmax_new = (2 as f64 * cross_corr / energy) as f32;
                    CCmax_new *= 1.0f32 - contour_bias * j as f32;
                } else {
                    CCmax_new = 0.0f32;
                }
                if CCmax_new > CCmax
                    && d + silk_CB_lags_stage3[0 as usize][j as usize] as i32 <= max_lag
                {
                    CCmax = CCmax_new;
                    lag_new = d;
                    CBimax = j;
                }
                j += 1;
            }
            lag_counter += 1;
            d += 1;
        }
        k = 0;
        while k < nb_subfr {
            *pitch_out.offset(k as isize) =
                lag_new + *Lag_CB_ptr.offset((k * cbk_size + CBimax) as isize) as i32;
            *pitch_out.offset(k as isize) = if min_lag > 18 * Fs_kHz {
                if *pitch_out.offset(k as isize) > min_lag {
                    min_lag
                } else if *pitch_out.offset(k as isize) < 18 * Fs_kHz {
                    18 * Fs_kHz
                } else {
                    *pitch_out.offset(k as isize)
                }
            } else if *pitch_out.offset(k as isize) > 18 * Fs_kHz {
                18 * Fs_kHz
            } else if *pitch_out.offset(k as isize) < min_lag {
                min_lag
            } else {
                *pitch_out.offset(k as isize)
            };
            k += 1;
        }
        *lagIndex = (lag_new - min_lag) as i16;
        *contourIndex = CBimax as i8;
    } else {
        k = 0;
        while k < nb_subfr {
            *pitch_out.offset(k as isize) =
                lag + *Lag_CB_ptr.offset((k * cbk_size + CBimax) as isize) as i32;
            *pitch_out.offset(k as isize) = if min_lag_8kHz > 18 * 8 {
                if *pitch_out.offset(k as isize) > min_lag_8kHz {
                    min_lag_8kHz
                } else if *pitch_out.offset(k as isize) < 18 * 8 {
                    18 * 8
                } else {
                    *pitch_out.offset(k as isize)
                }
            } else if *pitch_out.offset(k as isize) > 18 * 8 {
                18 * 8
            } else if *pitch_out.offset(k as isize) < min_lag_8kHz {
                min_lag_8kHz
            } else {
                *pitch_out.offset(k as isize)
            };
            k += 1;
        }
        *lagIndex = (lag - min_lag_8kHz) as i16;
        *contourIndex = CBimax as i8;
    }
    assert!(*lagIndex as i32 >= 0);
    return 0;
}
unsafe fn silk_P_Ana_calc_corr_st3(
    cross_corr_st3: *mut [[f32; 5]; 34],
    frame: *const f32,
    start_lag: i32,
    sf_length: i32,
    nb_subfr: i32,
    complexity: i32,
    arch: i32,
) {
    let mut target_ptr: *const f32 = 0 as *const f32;
    let mut i: i32 = 0;
    let mut j: i32 = 0;
    let mut k: i32 = 0;
    let mut lag_counter: i32 = 0;
    let mut lag_low: i32 = 0;
    let mut lag_high: i32 = 0;
    let mut nb_cbk_search: i32 = 0;
    let mut delta: i32 = 0;
    let mut idx: i32 = 0;
    let mut cbk_size: i32 = 0;
    let mut scratch_mem: [f32; 22] = [0.; 22];
    let mut xcorr: [opus_val32; 22] = [0.; 22];
    let mut Lag_range_ptr: *const i8 = 0 as *const i8;
    let mut Lag_CB_ptr: *const i8 = 0 as *const i8;
    assert!(complexity >= 0);
    assert!(complexity <= 2);
    if nb_subfr == PE_MAX_NB_SUBFR {
        Lag_range_ptr = &*(*(*silk_Lag_range_stage3.as_ptr().offset(complexity as isize))
            .as_ptr()
            .offset(0 as isize))
        .as_ptr()
        .offset(0 as isize) as *const i8;
        Lag_CB_ptr = &*(*silk_CB_lags_stage3.as_ptr().offset(0 as isize))
            .as_ptr()
            .offset(0 as isize) as *const i8;
        nb_cbk_search = silk_nb_cbk_searchs_stage3[complexity as usize] as i32;
        cbk_size = PE_NB_CBKS_STAGE3_MAX;
    } else {
        assert!(nb_subfr == 4 >> 1);
        Lag_range_ptr = &*(*silk_Lag_range_stage3_10_ms.as_ptr().offset(0 as isize))
            .as_ptr()
            .offset(0 as isize) as *const i8;
        Lag_CB_ptr = &*(*silk_CB_lags_stage3_10_ms.as_ptr().offset(0 as isize))
            .as_ptr()
            .offset(0 as isize) as *const i8;
        nb_cbk_search = PE_NB_CBKS_STAGE3_10MS;
        cbk_size = PE_NB_CBKS_STAGE3_10MS;
    }
    target_ptr = &*frame.offset(((sf_length as u32) << 2) as i32 as isize) as *const f32;
    k = 0;
    while k < nb_subfr {
        lag_counter = 0;
        lag_low = *Lag_range_ptr.offset((k * 2 + 0) as isize) as i32;
        lag_high = *Lag_range_ptr.offset((k * 2 + 1) as isize) as i32;
        celt_pitch_xcorr_c(
            target_ptr,
            target_ptr
                .offset(-(start_lag as isize))
                .offset(-(lag_high as isize)),
            xcorr.as_mut_ptr(),
            sf_length,
            lag_high - lag_low + 1,
            arch,
        );
        j = lag_low;
        while j <= lag_high {
            scratch_mem[lag_counter as usize] = xcorr[(lag_high - j) as usize];
            lag_counter += 1;
            j += 1;
        }
        delta = *Lag_range_ptr.offset((k * 2 + 0) as isize) as i32;
        i = 0;
        while i < nb_cbk_search {
            idx = *Lag_CB_ptr.offset((k * cbk_size + i) as isize) as i32 - delta;
            j = 0;
            while j < PE_NB_STAGE3_LAGS {
                (*cross_corr_st3.offset(k as isize))[i as usize][j as usize] =
                    scratch_mem[(idx + j) as usize];
                j += 1;
            }
            i += 1;
        }
        target_ptr = target_ptr.offset(sf_length as isize);
        k += 1;
    }
}
unsafe fn silk_P_Ana_calc_energy_st3(
    energies_st3: *mut [[f32; 5]; 34],
    frame: *const f32,
    start_lag: i32,
    sf_length: i32,
    nb_subfr: i32,
    complexity: i32,
) {
    let mut target_ptr: *const f32 = 0 as *const f32;
    let mut basis_ptr: *const f32 = 0 as *const f32;
    let mut energy: f64 = 0.;
    let mut k: i32 = 0;
    let mut i: i32 = 0;
    let mut j: i32 = 0;
    let mut lag_counter: i32 = 0;
    let mut nb_cbk_search: i32 = 0;
    let mut delta: i32 = 0;
    let mut idx: i32 = 0;
    let mut cbk_size: i32 = 0;
    let mut lag_diff: i32 = 0;
    let mut scratch_mem: [f32; 22] = [0.; 22];
    let mut Lag_range_ptr: *const i8 = 0 as *const i8;
    let mut Lag_CB_ptr: *const i8 = 0 as *const i8;
    assert!(complexity >= 0);
    assert!(complexity <= 2);
    if nb_subfr == PE_MAX_NB_SUBFR {
        Lag_range_ptr = &*(*(*silk_Lag_range_stage3.as_ptr().offset(complexity as isize))
            .as_ptr()
            .offset(0 as isize))
        .as_ptr()
        .offset(0 as isize) as *const i8;
        Lag_CB_ptr = &*(*silk_CB_lags_stage3.as_ptr().offset(0 as isize))
            .as_ptr()
            .offset(0 as isize) as *const i8;
        nb_cbk_search = silk_nb_cbk_searchs_stage3[complexity as usize] as i32;
        cbk_size = PE_NB_CBKS_STAGE3_MAX;
    } else {
        assert!(nb_subfr == 4 >> 1);
        Lag_range_ptr = &*(*silk_Lag_range_stage3_10_ms.as_ptr().offset(0 as isize))
            .as_ptr()
            .offset(0 as isize) as *const i8;
        Lag_CB_ptr = &*(*silk_CB_lags_stage3_10_ms.as_ptr().offset(0 as isize))
            .as_ptr()
            .offset(0 as isize) as *const i8;
        nb_cbk_search = PE_NB_CBKS_STAGE3_10MS;
        cbk_size = PE_NB_CBKS_STAGE3_10MS;
    }
    target_ptr = &*frame.offset(((sf_length as u32) << 2) as i32 as isize) as *const f32;
    k = 0;
    while k < nb_subfr {
        lag_counter = 0;
        basis_ptr = target_ptr
            .offset(-((start_lag + *Lag_range_ptr.offset((k * 2 + 0) as isize) as i32) as isize));
        energy = silk_energy_FLP(basis_ptr, sf_length) + 1e-3f64;
        scratch_mem[lag_counter as usize] = energy as f32;
        lag_counter += 1;
        lag_diff = *Lag_range_ptr.offset((k * 2 + 1) as isize) as i32
            - *Lag_range_ptr.offset((k * 2 + 0) as isize) as i32
            + 1;
        i = 1;
        while i < lag_diff {
            energy -= *basis_ptr.offset((sf_length - i) as isize) as f64
                * *basis_ptr.offset((sf_length - i) as isize) as f64;
            energy += *basis_ptr.offset(-i as isize) as f64 * *basis_ptr.offset(-i as isize) as f64;
            scratch_mem[lag_counter as usize] = energy as f32;
            lag_counter += 1;
            i += 1;
        }
        delta = *Lag_range_ptr.offset((k * 2 + 0) as isize) as i32;
        i = 0;
        while i < nb_cbk_search {
            idx = *Lag_CB_ptr.offset((k * cbk_size + i) as isize) as i32 - delta;
            j = 0;
            while j < PE_NB_STAGE3_LAGS {
                (*energies_st3.offset(k as isize))[i as usize][j as usize] =
                    scratch_mem[(idx + j) as usize];
                j += 1;
            }
            i += 1;
        }
        target_ptr = target_ptr.offset(sf_length as isize);
        k += 1;
    }
}
