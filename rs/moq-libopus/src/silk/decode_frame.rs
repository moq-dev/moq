use crate::celt::entdec::ec_dec;
use crate::externs::{memcpy, memmove};
use crate::silk::dec_API::{FLAG_DECODE_LBRR, FLAG_DECODE_NORMAL};
use crate::silk::decode_core::silk_decode_core;
use crate::silk::decode_indices::silk_decode_indices;
use crate::silk::decode_parameters::silk_decode_parameters;
use crate::silk::decode_pulses::silk_decode_pulses;
use crate::silk::structs::{silk_decoder_control, silk_decoder_state};
use crate::silk::CNG::silk_CNG;
use crate::silk::PLC::{silk_PLC, silk_PLC_glue_frames};

pub unsafe fn silk_decode_frame(
    psDec: *mut silk_decoder_state,
    psRangeDec: &mut ec_dec,
    pOut: *mut i16,
    pN: *mut i32,
    lostFlag: i32,
    condCoding: i32,
    arch: i32,
) -> i32 {
    let mut L: i32 = 0;
    let mut mv_len: i32 = 0;
    let ret: i32 = 0;
    L = (*psDec).frame_length;
    let mut psDecCtrl: [silk_decoder_control; 1] = [silk_decoder_control {
        pitchL: [0; 4],
        Gains_Q16: [0; 4],
        PredCoef_Q12: [[0; 16]; 2],
        LTPCoef_Q14: [0; 20],
        LTP_scale_Q14: 0,
    }; 1];
    (*psDecCtrl.as_mut_ptr()).LTP_scale_Q14 = 0;
    assert!(L > 0 && L <= 5 * 4 * 16);
    if lostFlag == FLAG_DECODE_NORMAL
        || lostFlag == FLAG_DECODE_LBRR
            && (*psDec).LBRR_flags[(*psDec).nFramesDecoded as usize] == 1
    {
        let vla = (L + 16 - 1 & !(16 - 1)) as usize;
        let mut pulses: Vec<i16> = ::std::vec::from_elem(0, vla);
        silk_decode_indices(
            psDec,
            psRangeDec,
            (*psDec).nFramesDecoded,
            lostFlag,
            condCoding,
        );
        silk_decode_pulses(
            psRangeDec,
            pulses.as_mut_ptr(),
            (*psDec).indices.signalType as i32,
            (*psDec).indices.quantOffsetType as i32,
            (*psDec).frame_length,
        );
        silk_decode_parameters(psDec, psDecCtrl.as_mut_ptr(), condCoding);
        silk_decode_core(
            psDec,
            psDecCtrl.as_mut_ptr(),
            pOut,
            pulses.as_mut_ptr() as *const i16,
            arch,
        );
        silk_PLC(psDec, psDecCtrl.as_mut_ptr(), pOut, 0, arch);
        (*psDec).lossCnt = 0;
        (*psDec).prevSignalType = (*psDec).indices.signalType as i32;
        assert!((*psDec).prevSignalType >= 0 && (*psDec).prevSignalType <= 2);
        (*psDec).first_frame_after_reset = 0;
    } else {
        (*psDec).indices.signalType = (*psDec).prevSignalType as i8;
        silk_PLC(psDec, psDecCtrl.as_mut_ptr(), pOut, 1, arch);
    }
    assert!((*psDec).ltp_mem_length >= (*psDec).frame_length);
    mv_len = (*psDec).ltp_mem_length - (*psDec).frame_length;
    memmove(
        ((*psDec).outBuf).as_mut_ptr() as *mut core::ffi::c_void,
        &mut *((*psDec).outBuf)
            .as_mut_ptr()
            .offset((*psDec).frame_length as isize) as *mut i16 as *const core::ffi::c_void,
        (mv_len as u64).wrapping_mul(::core::mem::size_of::<i16>() as u64),
    );
    memcpy(
        &mut *((*psDec).outBuf).as_mut_ptr().offset(mv_len as isize) as *mut i16
            as *mut core::ffi::c_void,
        pOut as *const core::ffi::c_void,
        ((*psDec).frame_length as u64).wrapping_mul(::core::mem::size_of::<i16>() as u64),
    );
    silk_CNG(psDec, psDecCtrl.as_mut_ptr(), pOut, L);
    silk_PLC_glue_frames(psDec, pOut, L);
    (*psDec).lagPrev = (*psDecCtrl.as_mut_ptr()).pitchL[((*psDec).nb_subfr - 1) as usize];
    *pN = L;
    return ret;
}
