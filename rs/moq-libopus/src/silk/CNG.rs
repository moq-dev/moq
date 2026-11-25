pub mod typedef_h {
    pub const silk_int16_MIN: i32 = i16::MIN as i32;
    pub const silk_int16_MAX: i32 = i16::MAX as i32;
    pub const silk_int32_MAX: i32 = i32::MAX;
    pub const silk_int32_MIN: i32 = i32::MIN;
}
pub use self::typedef_h::{silk_int16_MAX, silk_int16_MIN, silk_int32_MAX, silk_int32_MIN};
use crate::externs::{memcpy, memmove, memset};
use crate::silk::define::{CNG_BUF_MASK_MAX, MAX_LPC_ORDER, TYPE_NO_VOICE_ACTIVITY};
use crate::silk::structs::{silk_CNG_struct, silk_decoder_control, silk_decoder_state};
use crate::silk::Inlines::silk_SQRT_APPROX;
use crate::silk::NLSF2A::silk_NLSF2A;
#[inline]
unsafe fn silk_CNG_exc(exc_Q14: *mut i32, exc_buf_Q14: *mut i32, length: i32, rand_seed: *mut i32) {
    let mut seed: i32 = 0;
    let mut i: i32 = 0;
    let mut idx: i32 = 0;
    let mut exc_mask: i32 = 0;
    exc_mask = CNG_BUF_MASK_MAX;
    while exc_mask > length {
        exc_mask = exc_mask >> 1;
    }
    seed = *rand_seed;
    i = 0;
    while i < length {
        seed = 907633515_u32.wrapping_add((seed as u32).wrapping_mul(196314165)) as i32;
        idx = seed >> 24 & exc_mask;
        *exc_Q14.offset(i as isize) = *exc_buf_Q14.offset(idx as isize);
        i += 1;
    }
    *rand_seed = seed;
}
pub unsafe fn silk_CNG_Reset(psDec: *mut silk_decoder_state) {
    let mut i: i32 = 0;
    let mut NLSF_step_Q15: i32 = 0;
    let mut NLSF_acc_Q15: i32 = 0;
    NLSF_step_Q15 = 0x7fff / ((*psDec).LPC_order + 1);
    NLSF_acc_Q15 = 0;
    i = 0;
    while i < (*psDec).LPC_order {
        NLSF_acc_Q15 += NLSF_step_Q15;
        (*psDec).sCNG.CNG_smth_NLSF_Q15[i as usize] = NLSF_acc_Q15 as i16;
        i += 1;
    }
    (*psDec).sCNG.CNG_smth_Gain_Q16 = 0;
    (*psDec).sCNG.rand_seed = 3176576;
}
pub unsafe fn silk_CNG(
    psDec: *mut silk_decoder_state,
    psDecCtrl: *mut silk_decoder_control,
    frame: *mut i16,
    length: i32,
) {
    let mut i: i32 = 0;
    let mut subfr: i32 = 0;
    let mut LPC_pred_Q10: i32 = 0;
    let mut max_Gain_Q16: i32 = 0;
    let mut gain_Q16: i32 = 0;
    let mut gain_Q10: i32 = 0;
    let mut A_Q12: [i16; 16] = [0; 16];
    let psCNG: *mut silk_CNG_struct = &mut (*psDec).sCNG;
    if (*psDec).fs_kHz != (*psCNG).fs_kHz {
        silk_CNG_Reset(psDec);
        (*psCNG).fs_kHz = (*psDec).fs_kHz;
    }
    if (*psDec).lossCnt == 0 && (*psDec).prevSignalType == TYPE_NO_VOICE_ACTIVITY {
        i = 0;
        while i < (*psDec).LPC_order {
            (*psCNG).CNG_smth_NLSF_Q15[i as usize] = ((*psCNG).CNG_smth_NLSF_Q15[i as usize] as i32
                + (((*psDec).prevNLSF_Q15[i as usize] as i32
                    - (*psCNG).CNG_smth_NLSF_Q15[i as usize] as i32) as i64
                    * 16348 as i64
                    >> 16) as i32) as i16;
            i += 1;
        }
        max_Gain_Q16 = 0;
        subfr = 0;
        i = 0;
        while i < (*psDec).nb_subfr {
            if (*psDecCtrl).Gains_Q16[i as usize] > max_Gain_Q16 {
                max_Gain_Q16 = (*psDecCtrl).Gains_Q16[i as usize];
                subfr = i;
            }
            i += 1;
        }
        memmove(
            &mut *((*psCNG).CNG_exc_buf_Q14)
                .as_mut_ptr()
                .offset((*psDec).subfr_length as isize) as *mut i32
                as *mut core::ffi::c_void,
            ((*psCNG).CNG_exc_buf_Q14).as_mut_ptr() as *const core::ffi::c_void,
            ((((*psDec).nb_subfr - 1) * (*psDec).subfr_length) as u64)
                .wrapping_mul(::core::mem::size_of::<i32>() as u64),
        );
        memcpy(
            ((*psCNG).CNG_exc_buf_Q14).as_mut_ptr() as *mut core::ffi::c_void,
            &mut *((*psDec).exc_Q14)
                .as_mut_ptr()
                .offset((subfr * (*psDec).subfr_length) as isize) as *mut i32
                as *const core::ffi::c_void,
            ((*psDec).subfr_length as u64).wrapping_mul(::core::mem::size_of::<i32>() as u64),
        );
        i = 0;
        while i < (*psDec).nb_subfr {
            (*psCNG).CNG_smth_Gain_Q16 += (((*psDecCtrl).Gains_Q16[i as usize]
                - (*psCNG).CNG_smth_Gain_Q16) as i64
                * 4634 as i64
                >> 16) as i32;
            i += 1;
        }
    }
    if (*psDec).lossCnt != 0 {
        let vla = (length + 16) as usize;
        let mut CNG_sig_Q14: Vec<i32> = ::std::vec::from_elem(0, vla);
        gain_Q16 = ((*psDec).sPLC.randScale_Q14 as i64
            * (*psDec).sPLC.prevGain_Q16[1 as usize] as i64
            >> 16) as i32;
        if gain_Q16 >= (1) << 21 || (*psCNG).CNG_smth_Gain_Q16 > (1) << 23 {
            gain_Q16 = (gain_Q16 >> 16) * (gain_Q16 >> 16);
            gain_Q16 = ((*psCNG).CNG_smth_Gain_Q16 >> 16) * ((*psCNG).CNG_smth_Gain_Q16 >> 16)
                - ((gain_Q16 as u32) << 5) as i32;
            gain_Q16 = ((silk_SQRT_APPROX(gain_Q16) as u32) << 16) as i32;
        } else {
            gain_Q16 = (gain_Q16 as i64 * gain_Q16 as i64 >> 16) as i32;
            gain_Q16 = ((*psCNG).CNG_smth_Gain_Q16 as i64 * (*psCNG).CNG_smth_Gain_Q16 as i64 >> 16)
                as i32
                - ((gain_Q16 as u32) << 5) as i32;
            gain_Q16 = ((silk_SQRT_APPROX(gain_Q16) as u32) << 8) as i32;
        }
        gain_Q10 = gain_Q16 >> 6;
        silk_CNG_exc(
            CNG_sig_Q14.as_mut_ptr().offset(MAX_LPC_ORDER as isize),
            ((*psCNG).CNG_exc_buf_Q14).as_mut_ptr(),
            length,
            &mut (*psCNG).rand_seed,
        );
        silk_NLSF2A(
            A_Q12.as_mut_ptr(),
            ((*psCNG).CNG_smth_NLSF_Q15).as_mut_ptr(),
            (*psDec).LPC_order,
            (*psDec).arch,
        );
        memcpy(
            CNG_sig_Q14.as_mut_ptr() as *mut core::ffi::c_void,
            ((*psCNG).CNG_synth_state).as_mut_ptr() as *const core::ffi::c_void,
            16_u64.wrapping_mul(::core::mem::size_of::<i32>() as u64),
        );
        assert!((*psDec).LPC_order == 10 || (*psDec).LPC_order == 16);
        i = 0;
        while i < length {
            LPC_pred_Q10 = (*psDec).LPC_order >> 1;
            LPC_pred_Q10 = (LPC_pred_Q10 as i64
                + (*CNG_sig_Q14.as_mut_ptr().offset((16 + i - 1) as isize) as i64
                    * A_Q12[0 as usize] as i64
                    >> 16)) as i32;
            LPC_pred_Q10 = (LPC_pred_Q10 as i64
                + (*CNG_sig_Q14.as_mut_ptr().offset((16 + i - 2) as isize) as i64
                    * A_Q12[1 as usize] as i64
                    >> 16)) as i32;
            LPC_pred_Q10 = (LPC_pred_Q10 as i64
                + (*CNG_sig_Q14.as_mut_ptr().offset((16 + i - 3) as isize) as i64
                    * A_Q12[2 as usize] as i64
                    >> 16)) as i32;
            LPC_pred_Q10 = (LPC_pred_Q10 as i64
                + (*CNG_sig_Q14.as_mut_ptr().offset((16 + i - 4) as isize) as i64
                    * A_Q12[3 as usize] as i64
                    >> 16)) as i32;
            LPC_pred_Q10 = (LPC_pred_Q10 as i64
                + (*CNG_sig_Q14.as_mut_ptr().offset((16 + i - 5) as isize) as i64
                    * A_Q12[4 as usize] as i64
                    >> 16)) as i32;
            LPC_pred_Q10 = (LPC_pred_Q10 as i64
                + (*CNG_sig_Q14.as_mut_ptr().offset((16 + i - 6) as isize) as i64
                    * A_Q12[5 as usize] as i64
                    >> 16)) as i32;
            LPC_pred_Q10 = (LPC_pred_Q10 as i64
                + (*CNG_sig_Q14.as_mut_ptr().offset((16 + i - 7) as isize) as i64
                    * A_Q12[6 as usize] as i64
                    >> 16)) as i32;
            LPC_pred_Q10 = (LPC_pred_Q10 as i64
                + (*CNG_sig_Q14.as_mut_ptr().offset((16 + i - 8) as isize) as i64
                    * A_Q12[7 as usize] as i64
                    >> 16)) as i32;
            LPC_pred_Q10 = (LPC_pred_Q10 as i64
                + (*CNG_sig_Q14.as_mut_ptr().offset((16 + i - 9) as isize) as i64
                    * A_Q12[8 as usize] as i64
                    >> 16)) as i32;
            LPC_pred_Q10 = (LPC_pred_Q10 as i64
                + (*CNG_sig_Q14.as_mut_ptr().offset((16 + i - 10) as isize) as i64
                    * A_Q12[9 as usize] as i64
                    >> 16)) as i32;
            if (*psDec).LPC_order == 16 {
                LPC_pred_Q10 = (LPC_pred_Q10 as i64
                    + (*CNG_sig_Q14.as_mut_ptr().offset((16 + i - 11) as isize) as i64
                        * A_Q12[10 as usize] as i64
                        >> 16)) as i32;
                LPC_pred_Q10 = (LPC_pred_Q10 as i64
                    + (*CNG_sig_Q14.as_mut_ptr().offset((16 + i - 12) as isize) as i64
                        * A_Q12[11 as usize] as i64
                        >> 16)) as i32;
                LPC_pred_Q10 = (LPC_pred_Q10 as i64
                    + (*CNG_sig_Q14.as_mut_ptr().offset((16 + i - 13) as isize) as i64
                        * A_Q12[12 as usize] as i64
                        >> 16)) as i32;
                LPC_pred_Q10 = (LPC_pred_Q10 as i64
                    + (*CNG_sig_Q14.as_mut_ptr().offset((16 + i - 14) as isize) as i64
                        * A_Q12[13 as usize] as i64
                        >> 16)) as i32;
                LPC_pred_Q10 = (LPC_pred_Q10 as i64
                    + (*CNG_sig_Q14.as_mut_ptr().offset((16 + i - 15) as isize) as i64
                        * A_Q12[14 as usize] as i64
                        >> 16)) as i32;
                LPC_pred_Q10 = (LPC_pred_Q10 as i64
                    + (*CNG_sig_Q14.as_mut_ptr().offset((16 + i - 16) as isize) as i64
                        * A_Q12[15 as usize] as i64
                        >> 16)) as i32;
            }
            *CNG_sig_Q14
                .as_mut_ptr()
                .offset((MAX_LPC_ORDER + i) as isize) =
                if (*CNG_sig_Q14.as_mut_ptr().offset((16 + i) as isize) as u32).wrapping_add(
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
                    if (*CNG_sig_Q14.as_mut_ptr().offset((16 + i) as isize)
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
                        *CNG_sig_Q14.as_mut_ptr().offset((16 + i) as isize)
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
                } else if (*CNG_sig_Q14.as_mut_ptr().offset((16 + i) as isize)
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
                    *CNG_sig_Q14.as_mut_ptr().offset((16 + i) as isize)
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
            *frame.offset(i as isize) = (if *frame.offset(i as isize) as i32
                + (if (if 8 == 1 {
                    ((*CNG_sig_Q14.as_mut_ptr().offset((16 + i) as isize) as i64 * gain_Q10 as i64
                        >> 16) as i32
                        >> 1)
                        + ((*CNG_sig_Q14.as_mut_ptr().offset((16 + i) as isize) as i64
                            * gain_Q10 as i64
                            >> 16) as i32
                            & 1)
                } else {
                    ((*CNG_sig_Q14.as_mut_ptr().offset((16 + i) as isize) as i64 * gain_Q10 as i64
                        >> 16) as i32
                        >> 8 - 1)
                        + 1
                        >> 1
                }) > 0x7fff
                {
                    0x7fff
                } else {
                    if (if 8 == 1 {
                        ((*CNG_sig_Q14.as_mut_ptr().offset((16 + i) as isize) as i64
                            * gain_Q10 as i64
                            >> 16) as i32
                            >> 1)
                            + ((*CNG_sig_Q14.as_mut_ptr().offset((16 + i) as isize) as i64
                                * gain_Q10 as i64
                                >> 16) as i32
                                & 1)
                    } else {
                        ((*CNG_sig_Q14.as_mut_ptr().offset((16 + i) as isize) as i64
                            * gain_Q10 as i64
                            >> 16) as i32
                            >> 8 - 1)
                            + 1
                            >> 1
                    }) < 0x8000
                    {
                        0x8000
                    } else {
                        if 8 == 1 {
                            ((*CNG_sig_Q14.as_mut_ptr().offset((16 + i) as isize) as i64
                                * gain_Q10 as i64
                                >> 16) as i32
                                >> 1)
                                + ((*CNG_sig_Q14.as_mut_ptr().offset((16 + i) as isize) as i64
                                    * gain_Q10 as i64
                                    >> 16) as i32
                                    & 1)
                        } else {
                            ((*CNG_sig_Q14.as_mut_ptr().offset((16 + i) as isize) as i64
                                * gain_Q10 as i64
                                >> 16) as i32
                                >> 8 - 1)
                                + 1
                                >> 1
                        }
                    }
                })
                > silk_int16_MAX
            {
                silk_int16_MAX
            } else if *frame.offset(i as isize) as i32
                + (if (if 8 == 1 {
                    ((*CNG_sig_Q14.as_mut_ptr().offset((16 + i) as isize) as i64 * gain_Q10 as i64
                        >> 16) as i32
                        >> 1)
                        + ((*CNG_sig_Q14.as_mut_ptr().offset((16 + i) as isize) as i64
                            * gain_Q10 as i64
                            >> 16) as i32
                            & 1)
                } else {
                    ((*CNG_sig_Q14.as_mut_ptr().offset((16 + i) as isize) as i64 * gain_Q10 as i64
                        >> 16) as i32
                        >> 8 - 1)
                        + 1
                        >> 1
                }) > 0x7fff
                {
                    0x7fff
                } else {
                    if (if 8 == 1 {
                        ((*CNG_sig_Q14.as_mut_ptr().offset((16 + i) as isize) as i64
                            * gain_Q10 as i64
                            >> 16) as i32
                            >> 1)
                            + ((*CNG_sig_Q14.as_mut_ptr().offset((16 + i) as isize) as i64
                                * gain_Q10 as i64
                                >> 16) as i32
                                & 1)
                    } else {
                        ((*CNG_sig_Q14.as_mut_ptr().offset((16 + i) as isize) as i64
                            * gain_Q10 as i64
                            >> 16) as i32
                            >> 8 - 1)
                            + 1
                            >> 1
                    }) < 0x8000
                    {
                        0x8000
                    } else {
                        if 8 == 1 {
                            ((*CNG_sig_Q14.as_mut_ptr().offset((16 + i) as isize) as i64
                                * gain_Q10 as i64
                                >> 16) as i32
                                >> 1)
                                + ((*CNG_sig_Q14.as_mut_ptr().offset((16 + i) as isize) as i64
                                    * gain_Q10 as i64
                                    >> 16) as i32
                                    & 1)
                        } else {
                            ((*CNG_sig_Q14.as_mut_ptr().offset((16 + i) as isize) as i64
                                * gain_Q10 as i64
                                >> 16) as i32
                                >> 8 - 1)
                                + 1
                                >> 1
                        }
                    }
                })
                < silk_int16_MIN
            {
                silk_int16_MIN
            } else {
                *frame.offset(i as isize) as i32
                    + (if (if 8 == 1 {
                        ((*CNG_sig_Q14.as_mut_ptr().offset((16 + i) as isize) as i64
                            * gain_Q10 as i64
                            >> 16) as i32
                            >> 1)
                            + ((*CNG_sig_Q14.as_mut_ptr().offset((16 + i) as isize) as i64
                                * gain_Q10 as i64
                                >> 16) as i32
                                & 1)
                    } else {
                        ((*CNG_sig_Q14.as_mut_ptr().offset((16 + i) as isize) as i64
                            * gain_Q10 as i64
                            >> 16) as i32
                            >> 8 - 1)
                            + 1
                            >> 1
                    }) > 0x7fff
                    {
                        0x7fff
                    } else {
                        if (if 8 == 1 {
                            ((*CNG_sig_Q14.as_mut_ptr().offset((16 + i) as isize) as i64
                                * gain_Q10 as i64
                                >> 16) as i32
                                >> 1)
                                + ((*CNG_sig_Q14.as_mut_ptr().offset((16 + i) as isize) as i64
                                    * gain_Q10 as i64
                                    >> 16) as i32
                                    & 1)
                        } else {
                            ((*CNG_sig_Q14.as_mut_ptr().offset((16 + i) as isize) as i64
                                * gain_Q10 as i64
                                >> 16) as i32
                                >> 8 - 1)
                                + 1
                                >> 1
                        }) < 0x8000
                        {
                            0x8000
                        } else {
                            if 8 == 1 {
                                ((*CNG_sig_Q14.as_mut_ptr().offset((16 + i) as isize) as i64
                                    * gain_Q10 as i64
                                    >> 16) as i32
                                    >> 1)
                                    + ((*CNG_sig_Q14.as_mut_ptr().offset((16 + i) as isize) as i64
                                        * gain_Q10 as i64
                                        >> 16) as i32
                                        & 1)
                            } else {
                                ((*CNG_sig_Q14.as_mut_ptr().offset((16 + i) as isize) as i64
                                    * gain_Q10 as i64
                                    >> 16) as i32
                                    >> 8 - 1)
                                    + 1
                                    >> 1
                            }
                        }
                    })
            }) as i16;
            i += 1;
        }
        memcpy(
            ((*psCNG).CNG_synth_state).as_mut_ptr() as *mut core::ffi::c_void,
            &mut *CNG_sig_Q14.as_mut_ptr().offset(length as isize) as *mut i32
                as *const core::ffi::c_void,
            16_u64.wrapping_mul(::core::mem::size_of::<i32>() as u64),
        );
    } else {
        memset(
            ((*psCNG).CNG_synth_state).as_mut_ptr() as *mut core::ffi::c_void,
            0,
            ((*psDec).LPC_order as u64).wrapping_mul(::core::mem::size_of::<i32>() as u64),
        );
    };
}
