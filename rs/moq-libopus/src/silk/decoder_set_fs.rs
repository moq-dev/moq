use crate::externs::memset;
use crate::silk::define::{MAX_LPC_ORDER, MAX_NB_SUBFR, MIN_LPC_ORDER, TYPE_NO_VOICE_ACTIVITY};
use crate::silk::resampler::silk_resampler_init;
use crate::silk::structs::silk_decoder_state;
use crate::silk::tables_NLSF_CB_NB_MB::silk_NLSF_CB_NB_MB;
use crate::silk::tables_NLSF_CB_WB::silk_NLSF_CB_WB;
use crate::silk::tables_other::{silk_uniform4_iCDF, silk_uniform6_iCDF, silk_uniform8_iCDF};
use crate::silk::tables_pitch_lag::{
    silk_pitch_contour_10_ms_NB_iCDF, silk_pitch_contour_10_ms_iCDF, silk_pitch_contour_NB_iCDF,
    silk_pitch_contour_iCDF,
};

pub unsafe fn silk_decoder_set_fs(
    psDec: *mut silk_decoder_state,
    fs_kHz: i32,
    fs_API_Hz: i32,
) -> i32 {
    let mut frame_length: i32 = 0;
    assert!(fs_kHz == 8 || fs_kHz == 12 || fs_kHz == 16);
    assert!((*psDec).nb_subfr == 4 || (*psDec).nb_subfr == 4 / 2);
    (*psDec).subfr_length = 5 * fs_kHz as i16 as i32;
    frame_length = (*psDec).nb_subfr as i16 as i32 * (*psDec).subfr_length as i16 as i32;
    if (*psDec).fs_kHz != fs_kHz || (*psDec).fs_API_hz != fs_API_Hz {
        (*psDec).resampler_state = silk_resampler_init(fs_kHz as i16 as i32 * 1000, fs_API_Hz, 0);
        (*psDec).fs_API_hz = fs_API_Hz;
    }
    if (*psDec).fs_kHz != fs_kHz || frame_length != (*psDec).frame_length {
        if fs_kHz == 8 {
            if (*psDec).nb_subfr == MAX_NB_SUBFR {
                (*psDec).pitch_contour_iCDF = &silk_pitch_contour_NB_iCDF;
            } else {
                (*psDec).pitch_contour_iCDF = &silk_pitch_contour_10_ms_NB_iCDF;
            }
        } else if (*psDec).nb_subfr == MAX_NB_SUBFR {
            (*psDec).pitch_contour_iCDF = &silk_pitch_contour_iCDF;
        } else {
            (*psDec).pitch_contour_iCDF = &silk_pitch_contour_10_ms_iCDF;
        }
        if (*psDec).fs_kHz != fs_kHz {
            (*psDec).ltp_mem_length = 20 * fs_kHz as i16 as i32;
            if fs_kHz == 8 || fs_kHz == 12 {
                (*psDec).LPC_order = MIN_LPC_ORDER;
                (*psDec).psNLSF_CB = &silk_NLSF_CB_NB_MB;
            } else {
                (*psDec).LPC_order = MAX_LPC_ORDER;
                (*psDec).psNLSF_CB = &silk_NLSF_CB_WB;
            }
            if fs_kHz == 16 {
                (*psDec).pitch_lag_low_bits_iCDF = &silk_uniform8_iCDF;
            } else if fs_kHz == 12 {
                (*psDec).pitch_lag_low_bits_iCDF = &silk_uniform6_iCDF;
            } else if fs_kHz == 8 {
                (*psDec).pitch_lag_low_bits_iCDF = &silk_uniform4_iCDF;
            } else {
                panic!("libopus: assert(0) called");
            }
            (*psDec).first_frame_after_reset = 1;
            (*psDec).lagPrev = 100;
            (*psDec).LastGainIndex = 10;
            (*psDec).prevSignalType = TYPE_NO_VOICE_ACTIVITY;
            memset(
                ((*psDec).outBuf).as_mut_ptr() as *mut core::ffi::c_void,
                0,
                ::core::mem::size_of::<[i16; 480]>() as u64,
            );
            memset(
                ((*psDec).sLPC_Q14_buf).as_mut_ptr() as *mut core::ffi::c_void,
                0,
                ::core::mem::size_of::<[i32; 16]>() as u64,
            );
        }
        (*psDec).fs_kHz = fs_kHz;
        (*psDec).frame_length = frame_length;
    }
    assert!((*psDec).frame_length > 0 && (*psDec).frame_length <= 5 * 4 * 16);

    return 0;
}
