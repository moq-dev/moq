pub mod typedef_h {
    pub const silk_int16_MIN: i32 = i16::MIN as i32;
    pub const silk_int16_MAX: i32 = i16::MAX as i32;
    pub const silk_int32_MAX: i32 = i32::MAX;
    pub const silk_int32_MIN: i32 = i32::MIN;
}
pub use self::typedef_h::{silk_int16_MAX, silk_int16_MIN, silk_int32_MAX, silk_int32_MIN};
use crate::externs::{memcpy, memset};
use crate::silk::define::{
    LTP_ORDER, MAX_LPC_ORDER, MAX_NB_SUBFR, QUANT_LEVEL_ADJUST_Q10, TYPE_VOICED,
};
use crate::silk::structs::{silk_decoder_control, silk_decoder_state};
use crate::silk::tables_other::silk_Quantization_Offsets_Q10;
use crate::silk::Inlines::{silk_DIV32_varQ, silk_INVERSE32_varQ};
use crate::silk::LPC_analysis_filter::silk_LPC_analysis_filter;

pub unsafe fn silk_decode_core(
    psDec: *mut silk_decoder_state,
    psDecCtrl: *mut silk_decoder_control,
    xq: *mut i16,
    pulses: *const i16,
    arch: i32,
) {
    let mut i: i32 = 0;
    let mut k: i32 = 0;
    let mut lag: i32 = 0;
    let mut start_idx: i32 = 0;
    let mut sLTP_buf_idx: i32 = 0;
    let mut NLSF_interpolation_flag: i32 = 0;
    let mut signalType: i32 = 0;
    let mut A_Q12: *mut i16 = 0 as *mut i16;
    let mut B_Q14: *mut i16 = 0 as *mut i16;
    let mut pxq: *mut i16 = 0 as *mut i16;
    let mut A_Q12_tmp: [i16; 16] = [0; 16];
    let mut LTP_pred_Q13: i32 = 0;
    let mut LPC_pred_Q10: i32 = 0;
    let mut Gain_Q10: i32 = 0;
    let mut inv_gain_Q31: i32 = 0;
    let mut gain_adj_Q16: i32 = 0;
    let mut rand_seed: i32 = 0;
    let mut offset_Q10: i32 = 0;
    let mut pred_lag_ptr: *mut i32 = 0 as *mut i32;
    let mut pexc_Q14: *mut i32 = 0 as *mut i32;
    let mut pres_Q14: *mut i32 = 0 as *mut i32;
    let vla = (*psDec).ltp_mem_length as usize;
    let mut sLTP: Vec<i16> = ::std::vec::from_elem(0, vla);
    let vla_0 = ((*psDec).ltp_mem_length + (*psDec).frame_length) as usize;
    let mut sLTP_Q15: Vec<i32> = ::std::vec::from_elem(0, vla_0);
    let vla_1 = (*psDec).subfr_length as usize;
    let mut res_Q14: Vec<i32> = ::std::vec::from_elem(0, vla_1);
    let vla_2 = ((*psDec).subfr_length + 16) as usize;
    let mut sLPC_Q14: Vec<i32> = ::std::vec::from_elem(0, vla_2);
    offset_Q10 = silk_Quantization_Offsets_Q10[((*psDec).indices.signalType as i32 >> 1) as usize]
        [(*psDec).indices.quantOffsetType as usize] as i32;
    if ((*psDec).indices.NLSFInterpCoef_Q2 as i32) < (1) << 2 {
        NLSF_interpolation_flag = 1;
    } else {
        NLSF_interpolation_flag = 0;
    }
    rand_seed = (*psDec).indices.Seed as i32;
    i = 0;
    while i < (*psDec).frame_length {
        rand_seed = 907633515_u32.wrapping_add((rand_seed as u32).wrapping_mul(196314165)) as i32;
        (*psDec).exc_Q14[i as usize] = ((*pulses.offset(i as isize) as i32 as u32) << 14) as i32;
        if (*psDec).exc_Q14[i as usize] > 0 {
            (*psDec).exc_Q14[i as usize] -= QUANT_LEVEL_ADJUST_Q10 << 4;
        } else if (*psDec).exc_Q14[i as usize] < 0 {
            (*psDec).exc_Q14[i as usize] += QUANT_LEVEL_ADJUST_Q10 << 4;
        }
        (*psDec).exc_Q14[i as usize] += offset_Q10 << 4;
        if rand_seed < 0 {
            (*psDec).exc_Q14[i as usize] = -(*psDec).exc_Q14[i as usize];
        }
        rand_seed = (rand_seed as u32).wrapping_add(*pulses.offset(i as isize) as u32) as i32;
        i += 1;
    }
    memcpy(
        sLPC_Q14.as_mut_ptr() as *mut core::ffi::c_void,
        ((*psDec).sLPC_Q14_buf).as_mut_ptr() as *const core::ffi::c_void,
        16_u64.wrapping_mul(::core::mem::size_of::<i32>() as u64),
    );
    pexc_Q14 = ((*psDec).exc_Q14).as_mut_ptr();
    pxq = xq;
    sLTP_buf_idx = (*psDec).ltp_mem_length;
    k = 0;
    while k < (*psDec).nb_subfr {
        pres_Q14 = res_Q14.as_mut_ptr();
        A_Q12 = ((*psDecCtrl).PredCoef_Q12[(k >> 1) as usize]).as_mut_ptr();
        memcpy(
            A_Q12_tmp.as_mut_ptr() as *mut core::ffi::c_void,
            A_Q12 as *const core::ffi::c_void,
            ((*psDec).LPC_order as u64).wrapping_mul(::core::mem::size_of::<i16>() as u64),
        );
        B_Q14 = &mut *((*psDecCtrl).LTPCoef_Q14)
            .as_mut_ptr()
            .offset((k * LTP_ORDER) as isize) as *mut i16;
        signalType = (*psDec).indices.signalType as i32;
        Gain_Q10 = (*psDecCtrl).Gains_Q16[k as usize] >> 6;
        inv_gain_Q31 = silk_INVERSE32_varQ((*psDecCtrl).Gains_Q16[k as usize], 47);
        if (*psDecCtrl).Gains_Q16[k as usize] != (*psDec).prev_gain_Q16 {
            gain_adj_Q16 = silk_DIV32_varQ(
                (*psDec).prev_gain_Q16,
                (*psDecCtrl).Gains_Q16[k as usize],
                16,
            );
            i = 0;
            while i < MAX_LPC_ORDER {
                *sLPC_Q14.as_mut_ptr().offset(i as isize) = (gain_adj_Q16 as i64
                    * *sLPC_Q14.as_mut_ptr().offset(i as isize) as i64
                    >> 16) as i32;
                i += 1;
            }
        } else {
            gain_adj_Q16 = (1) << 16;
        }
        (*psDec).prev_gain_Q16 = (*psDecCtrl).Gains_Q16[k as usize];
        if (*psDec).lossCnt != 0
            && (*psDec).prevSignalType == TYPE_VOICED
            && (*psDec).indices.signalType as i32 != TYPE_VOICED
            && k < MAX_NB_SUBFR / 2
        {
            memset(
                B_Q14 as *mut core::ffi::c_void,
                0,
                5_u64.wrapping_mul(::core::mem::size_of::<i16>() as u64),
            );
            *B_Q14.offset((LTP_ORDER / 2) as isize) =
                (0.25f64 * ((1) << 14) as f64 + 0.5f64) as i32 as i16;
            signalType = TYPE_VOICED;
            (*psDecCtrl).pitchL[k as usize] = (*psDec).lagPrev;
        }
        if signalType == TYPE_VOICED {
            lag = (*psDecCtrl).pitchL[k as usize];
            if k == 0 || k == 2 && NLSF_interpolation_flag != 0 {
                start_idx = (*psDec).ltp_mem_length - lag - (*psDec).LPC_order - LTP_ORDER / 2;
                assert!(start_idx > 0);
                if k == 2 {
                    memcpy(
                        &mut *((*psDec).outBuf)
                            .as_mut_ptr()
                            .offset((*psDec).ltp_mem_length as isize)
                            as *mut i16 as *mut core::ffi::c_void,
                        xq as *const core::ffi::c_void,
                        ((2 * (*psDec).subfr_length) as u64)
                            .wrapping_mul(::core::mem::size_of::<i16>() as u64),
                    );
                }
                silk_LPC_analysis_filter(
                    &mut *sLTP.as_mut_ptr().offset(start_idx as isize),
                    &mut *((*psDec).outBuf)
                        .as_mut_ptr()
                        .offset((start_idx + k * (*psDec).subfr_length) as isize),
                    A_Q12,
                    (*psDec).ltp_mem_length - start_idx,
                    (*psDec).LPC_order,
                    arch,
                );
                if k == 0 {
                    inv_gain_Q31 = (((inv_gain_Q31 as i64
                        * (*psDecCtrl).LTP_scale_Q14 as i16 as i64
                        >> 16) as i32 as u32)
                        << 2) as i32;
                }
                i = 0;
                while i < lag + LTP_ORDER / 2 {
                    *sLTP_Q15
                        .as_mut_ptr()
                        .offset((sLTP_buf_idx - i - 1) as isize) = (inv_gain_Q31 as i64
                        * *sLTP
                            .as_mut_ptr()
                            .offset(((*psDec).ltp_mem_length - i - 1) as isize)
                            as i64
                        >> 16)
                        as i32;
                    i += 1;
                }
            } else if gain_adj_Q16 != (1) << 16 {
                i = 0;
                while i < lag + LTP_ORDER / 2 {
                    *sLTP_Q15
                        .as_mut_ptr()
                        .offset((sLTP_buf_idx - i - 1) as isize) = (gain_adj_Q16 as i64
                        * *sLTP_Q15
                            .as_mut_ptr()
                            .offset((sLTP_buf_idx - i - 1) as isize)
                            as i64
                        >> 16)
                        as i32;
                    i += 1;
                }
            }
        }
        if signalType == TYPE_VOICED {
            pred_lag_ptr = &mut *sLTP_Q15
                .as_mut_ptr()
                .offset((sLTP_buf_idx - lag + LTP_ORDER / 2) as isize)
                as *mut i32;
            i = 0;
            while i < (*psDec).subfr_length {
                LTP_pred_Q13 = 2;
                LTP_pred_Q13 = (LTP_pred_Q13 as i64
                    + (*pred_lag_ptr.offset(0 as isize) as i64 * *B_Q14.offset(0 as isize) as i64
                        >> 16)) as i32;
                LTP_pred_Q13 = (LTP_pred_Q13 as i64
                    + (*pred_lag_ptr.offset(-1 as isize) as i64 * *B_Q14.offset(1 as isize) as i64
                        >> 16)) as i32;
                LTP_pred_Q13 = (LTP_pred_Q13 as i64
                    + (*pred_lag_ptr.offset(-(2) as isize) as i64
                        * *B_Q14.offset(2 as isize) as i64
                        >> 16)) as i32;
                LTP_pred_Q13 = (LTP_pred_Q13 as i64
                    + (*pred_lag_ptr.offset(-(3) as isize) as i64
                        * *B_Q14.offset(3 as isize) as i64
                        >> 16)) as i32;
                LTP_pred_Q13 = (LTP_pred_Q13 as i64
                    + (*pred_lag_ptr.offset(-(4) as isize) as i64
                        * *B_Q14.offset(4 as isize) as i64
                        >> 16)) as i32;
                pred_lag_ptr = pred_lag_ptr.offset(1);
                *pres_Q14.offset(i as isize) =
                    *pexc_Q14.offset(i as isize) + ((LTP_pred_Q13 as u32) << 1) as i32;
                *sLTP_Q15.as_mut_ptr().offset(sLTP_buf_idx as isize) =
                    ((*pres_Q14.offset(i as isize) as u32) << 1) as i32;
                sLTP_buf_idx += 1;
                i += 1;
            }
        } else {
            pres_Q14 = pexc_Q14;
        }
        i = 0;
        while i < (*psDec).subfr_length {
            assert!((*psDec).LPC_order == 10 || (*psDec).LPC_order == 16);
            LPC_pred_Q10 = (*psDec).LPC_order >> 1;
            LPC_pred_Q10 = (LPC_pred_Q10 as i64
                + (*sLPC_Q14.as_mut_ptr().offset((16 + i - 1) as isize) as i64
                    * A_Q12_tmp[0 as usize] as i64
                    >> 16)) as i32;
            LPC_pred_Q10 = (LPC_pred_Q10 as i64
                + (*sLPC_Q14.as_mut_ptr().offset((16 + i - 2) as isize) as i64
                    * A_Q12_tmp[1 as usize] as i64
                    >> 16)) as i32;
            LPC_pred_Q10 = (LPC_pred_Q10 as i64
                + (*sLPC_Q14.as_mut_ptr().offset((16 + i - 3) as isize) as i64
                    * A_Q12_tmp[2 as usize] as i64
                    >> 16)) as i32;
            LPC_pred_Q10 = (LPC_pred_Q10 as i64
                + (*sLPC_Q14.as_mut_ptr().offset((16 + i - 4) as isize) as i64
                    * A_Q12_tmp[3 as usize] as i64
                    >> 16)) as i32;
            LPC_pred_Q10 = (LPC_pred_Q10 as i64
                + (*sLPC_Q14.as_mut_ptr().offset((16 + i - 5) as isize) as i64
                    * A_Q12_tmp[4 as usize] as i64
                    >> 16)) as i32;
            LPC_pred_Q10 = (LPC_pred_Q10 as i64
                + (*sLPC_Q14.as_mut_ptr().offset((16 + i - 6) as isize) as i64
                    * A_Q12_tmp[5 as usize] as i64
                    >> 16)) as i32;
            LPC_pred_Q10 = (LPC_pred_Q10 as i64
                + (*sLPC_Q14.as_mut_ptr().offset((16 + i - 7) as isize) as i64
                    * A_Q12_tmp[6 as usize] as i64
                    >> 16)) as i32;
            LPC_pred_Q10 = (LPC_pred_Q10 as i64
                + (*sLPC_Q14.as_mut_ptr().offset((16 + i - 8) as isize) as i64
                    * A_Q12_tmp[7 as usize] as i64
                    >> 16)) as i32;
            LPC_pred_Q10 = (LPC_pred_Q10 as i64
                + (*sLPC_Q14.as_mut_ptr().offset((16 + i - 9) as isize) as i64
                    * A_Q12_tmp[8 as usize] as i64
                    >> 16)) as i32;
            LPC_pred_Q10 = (LPC_pred_Q10 as i64
                + (*sLPC_Q14.as_mut_ptr().offset((16 + i - 10) as isize) as i64
                    * A_Q12_tmp[9 as usize] as i64
                    >> 16)) as i32;
            if (*psDec).LPC_order == 16 {
                LPC_pred_Q10 = (LPC_pred_Q10 as i64
                    + (*sLPC_Q14.as_mut_ptr().offset((16 + i - 11) as isize) as i64
                        * A_Q12_tmp[10 as usize] as i64
                        >> 16)) as i32;
                LPC_pred_Q10 = (LPC_pred_Q10 as i64
                    + (*sLPC_Q14.as_mut_ptr().offset((16 + i - 12) as isize) as i64
                        * A_Q12_tmp[11 as usize] as i64
                        >> 16)) as i32;
                LPC_pred_Q10 = (LPC_pred_Q10 as i64
                    + (*sLPC_Q14.as_mut_ptr().offset((16 + i - 13) as isize) as i64
                        * A_Q12_tmp[12 as usize] as i64
                        >> 16)) as i32;
                LPC_pred_Q10 = (LPC_pred_Q10 as i64
                    + (*sLPC_Q14.as_mut_ptr().offset((16 + i - 14) as isize) as i64
                        * A_Q12_tmp[13 as usize] as i64
                        >> 16)) as i32;
                LPC_pred_Q10 = (LPC_pred_Q10 as i64
                    + (*sLPC_Q14.as_mut_ptr().offset((16 + i - 15) as isize) as i64
                        * A_Q12_tmp[14 as usize] as i64
                        >> 16)) as i32;
                LPC_pred_Q10 = (LPC_pred_Q10 as i64
                    + (*sLPC_Q14.as_mut_ptr().offset((16 + i - 16) as isize) as i64
                        * A_Q12_tmp[15 as usize] as i64
                        >> 16)) as i32;
            }
            *sLPC_Q14.as_mut_ptr().offset((MAX_LPC_ORDER + i) as isize) =
                if (*pres_Q14.offset(i as isize) as u32).wrapping_add(
                    (((if 0x80000000 as u32 as i32 >> 4 > 0x7fffffff >> 4 {
                        if LPC_pred_Q10 > 0x80000000 as u32 as i32 >> 4 {
                            0x80000000 as u32 as i32 >> 4
                        } else {
                            if LPC_pred_Q10 < 0x7fffffff >> 4 {
                                0x7fffffff >> 4
                            } else {
                                LPC_pred_Q10
                            }
                        }
                    } else {
                        if LPC_pred_Q10 > 0x7fffffff >> 4 {
                            0x7fffffff >> 4
                        } else {
                            if LPC_pred_Q10 < 0x80000000 as u32 as i32 >> 4 {
                                0x80000000 as u32 as i32 >> 4
                            } else {
                                LPC_pred_Q10
                            }
                        }
                    }) as u32)
                        << 4) as i32 as u32,
                ) & 0x80000000 as u32
                    == 0
                {
                    if (*pres_Q14.offset(i as isize)
                        & (((if 0x80000000 as u32 as i32 >> 4 > 0x7fffffff >> 4 {
                            if LPC_pred_Q10 > 0x80000000 as u32 as i32 >> 4 {
                                0x80000000 as u32 as i32 >> 4
                            } else {
                                if LPC_pred_Q10 < 0x7fffffff >> 4 {
                                    0x7fffffff >> 4
                                } else {
                                    LPC_pred_Q10
                                }
                            }
                        } else {
                            if LPC_pred_Q10 > 0x7fffffff >> 4 {
                                0x7fffffff >> 4
                            } else {
                                if LPC_pred_Q10 < 0x80000000 as u32 as i32 >> 4 {
                                    0x80000000 as u32 as i32 >> 4
                                } else {
                                    LPC_pred_Q10
                                }
                            }
                        }) as u32)
                            << 4) as i32) as u32
                        & 0x80000000 as u32
                        != 0
                    {
                        silk_int32_MIN as i32
                    } else {
                        *pres_Q14.offset(i as isize)
                            + (((if 0x80000000 as u32 as i32 >> 4 > 0x7fffffff >> 4 {
                                if LPC_pred_Q10 > 0x80000000 as u32 as i32 >> 4 {
                                    0x80000000 as u32 as i32 >> 4
                                } else {
                                    if LPC_pred_Q10 < 0x7fffffff >> 4 {
                                        0x7fffffff >> 4
                                    } else {
                                        LPC_pred_Q10
                                    }
                                }
                            } else {
                                if LPC_pred_Q10 > 0x7fffffff >> 4 {
                                    0x7fffffff >> 4
                                } else {
                                    if LPC_pred_Q10 < 0x80000000 as u32 as i32 >> 4 {
                                        0x80000000 as u32 as i32 >> 4
                                    } else {
                                        LPC_pred_Q10
                                    }
                                }
                            }) as u32)
                                << 4) as i32
                    }
                } else if (*pres_Q14.offset(i as isize)
                    | (((if 0x80000000 as u32 as i32 >> 4 > 0x7fffffff >> 4 {
                        if LPC_pred_Q10 > 0x80000000 as u32 as i32 >> 4 {
                            0x80000000 as u32 as i32 >> 4
                        } else {
                            if LPC_pred_Q10 < 0x7fffffff >> 4 {
                                0x7fffffff >> 4
                            } else {
                                LPC_pred_Q10
                            }
                        }
                    } else {
                        if LPC_pred_Q10 > 0x7fffffff >> 4 {
                            0x7fffffff >> 4
                        } else {
                            if LPC_pred_Q10 < 0x80000000 as u32 as i32 >> 4 {
                                0x80000000 as u32 as i32 >> 4
                            } else {
                                LPC_pred_Q10
                            }
                        }
                    }) as u32)
                        << 4) as i32) as u32
                    & 0x80000000 as u32
                    == 0
                {
                    silk_int32_MAX
                } else {
                    *pres_Q14.offset(i as isize)
                        + (((if 0x80000000 as u32 as i32 >> 4 > 0x7fffffff >> 4 {
                            if LPC_pred_Q10 > 0x80000000 as u32 as i32 >> 4 {
                                0x80000000 as u32 as i32 >> 4
                            } else {
                                if LPC_pred_Q10 < 0x7fffffff >> 4 {
                                    0x7fffffff >> 4
                                } else {
                                    LPC_pred_Q10
                                }
                            }
                        } else {
                            if LPC_pred_Q10 > 0x7fffffff >> 4 {
                                0x7fffffff >> 4
                            } else {
                                if LPC_pred_Q10 < 0x80000000 as u32 as i32 >> 4 {
                                    0x80000000 as u32 as i32 >> 4
                                } else {
                                    LPC_pred_Q10
                                }
                            }
                        }) as u32)
                            << 4) as i32
                };
            *pxq.offset(i as isize) = (if (if 8 == 1 {
                ((*sLPC_Q14.as_mut_ptr().offset((16 + i) as isize) as i64 * Gain_Q10 as i64 >> 16)
                    as i32
                    >> 1)
                    + ((*sLPC_Q14.as_mut_ptr().offset((16 + i) as isize) as i64 * Gain_Q10 as i64
                        >> 16) as i32
                        & 1)
            } else {
                ((*sLPC_Q14.as_mut_ptr().offset((16 + i) as isize) as i64 * Gain_Q10 as i64 >> 16)
                    as i32
                    >> 8 - 1)
                    + 1
                    >> 1
            }) > silk_int16_MAX
            {
                silk_int16_MAX
            } else if (if 8 == 1 {
                ((*sLPC_Q14.as_mut_ptr().offset((16 + i) as isize) as i64 * Gain_Q10 as i64 >> 16)
                    as i32
                    >> 1)
                    + ((*sLPC_Q14.as_mut_ptr().offset((16 + i) as isize) as i64 * Gain_Q10 as i64
                        >> 16) as i32
                        & 1)
            } else {
                ((*sLPC_Q14.as_mut_ptr().offset((16 + i) as isize) as i64 * Gain_Q10 as i64 >> 16)
                    as i32
                    >> 8 - 1)
                    + 1
                    >> 1
            }) < silk_int16_MIN
            {
                silk_int16_MIN
            } else if 8 == 1 {
                ((*sLPC_Q14.as_mut_ptr().offset((16 + i) as isize) as i64 * Gain_Q10 as i64 >> 16)
                    as i32
                    >> 1)
                    + ((*sLPC_Q14.as_mut_ptr().offset((16 + i) as isize) as i64 * Gain_Q10 as i64
                        >> 16) as i32
                        & 1)
            } else {
                ((*sLPC_Q14.as_mut_ptr().offset((16 + i) as isize) as i64 * Gain_Q10 as i64 >> 16)
                    as i32
                    >> 8 - 1)
                    + 1
                    >> 1
            }) as i16;
            i += 1;
        }
        memcpy(
            sLPC_Q14.as_mut_ptr() as *mut core::ffi::c_void,
            &mut *sLPC_Q14.as_mut_ptr().offset((*psDec).subfr_length as isize) as *mut i32
                as *const core::ffi::c_void,
            16_u64.wrapping_mul(::core::mem::size_of::<i32>() as u64),
        );
        pexc_Q14 = pexc_Q14.offset((*psDec).subfr_length as isize);
        pxq = pxq.offset((*psDec).subfr_length as isize);
        k += 1;
    }
    memcpy(
        ((*psDec).sLPC_Q14_buf).as_mut_ptr() as *mut core::ffi::c_void,
        sLPC_Q14.as_mut_ptr() as *const core::ffi::c_void,
        16_u64.wrapping_mul(::core::mem::size_of::<i32>() as u64),
    );
}
