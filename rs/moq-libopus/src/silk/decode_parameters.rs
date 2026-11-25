use crate::externs::{memcpy, memset};
use crate::silk::bwexpander::silk_bwexpander;
use crate::silk::decode_pitch::silk_decode_pitch;
use crate::silk::define::{BWE_AFTER_LOSS_Q16, CODE_CONDITIONALLY, LTP_ORDER, TYPE_VOICED};
use crate::silk::gain_quant::silk_gains_dequant;
use crate::silk::structs::{silk_decoder_control, silk_decoder_state};
use crate::silk::tables_LTP::silk_LTP_vq_ptrs_Q7;
use crate::silk::tables_other::silk_LTPScales_table_Q14;
use crate::silk::NLSF_decode::silk_NLSF_decode;
use crate::silk::NLSF2A::silk_NLSF2A;

pub unsafe fn silk_decode_parameters(
    psDec: *mut silk_decoder_state,
    psDecCtrl: *mut silk_decoder_control,
    condCoding: i32,
) {
    let mut i: i32 = 0;
    let mut k: i32 = 0;
    let mut Ix: i32 = 0;
    let mut pNLSF_Q15: [i16; 16] = [0; 16];
    let mut pNLSF0_Q15: [i16; 16] = [0; 16];
    let mut cbk_ptr_Q7: *const i8 = 0 as *const i8;
    silk_gains_dequant(
        ((*psDecCtrl).Gains_Q16).as_mut_ptr(),
        ((*psDec).indices.GainsIndices).as_mut_ptr() as *const i8,
        &mut (*psDec).LastGainIndex,
        (condCoding == CODE_CONDITIONALLY) as i32,
        (*psDec).nb_subfr,
    );
    silk_NLSF_decode(
        &mut pNLSF_Q15[..(*psDec).psNLSF_CB.order as usize],
        &(*psDec).indices.NLSFIndices[..(*psDec).psNLSF_CB.order as usize + 1],
        (*psDec).psNLSF_CB,
    );
    silk_NLSF2A(
        ((*psDecCtrl).PredCoef_Q12[1 as usize]).as_mut_ptr(),
        pNLSF_Q15.as_mut_ptr(),
        (*psDec).LPC_order,
        (*psDec).arch,
    );
    if (*psDec).first_frame_after_reset == 1 {
        (*psDec).indices.NLSFInterpCoef_Q2 = 4;
    }
    if ((*psDec).indices.NLSFInterpCoef_Q2 as i32) < 4 {
        i = 0;
        while i < (*psDec).LPC_order {
            pNLSF0_Q15[i as usize] = ((*psDec).prevNLSF_Q15[i as usize] as i32
                + ((*psDec).indices.NLSFInterpCoef_Q2 as i32
                    * (pNLSF_Q15[i as usize] as i32 - (*psDec).prevNLSF_Q15[i as usize] as i32)
                    >> 2)) as i16;
            i += 1;
        }
        silk_NLSF2A(
            ((*psDecCtrl).PredCoef_Q12[0 as usize]).as_mut_ptr(),
            pNLSF0_Q15.as_mut_ptr(),
            (*psDec).LPC_order,
            (*psDec).arch,
        );
    } else {
        memcpy(
            ((*psDecCtrl).PredCoef_Q12[0 as usize]).as_mut_ptr() as *mut core::ffi::c_void,
            ((*psDecCtrl).PredCoef_Q12[1 as usize]).as_mut_ptr() as *const core::ffi::c_void,
            ((*psDec).LPC_order as u64).wrapping_mul(::core::mem::size_of::<i16>() as u64),
        );
    }
    memcpy(
        ((*psDec).prevNLSF_Q15).as_mut_ptr() as *mut core::ffi::c_void,
        pNLSF_Q15.as_mut_ptr() as *const core::ffi::c_void,
        ((*psDec).LPC_order as u64).wrapping_mul(::core::mem::size_of::<i16>() as u64),
    );
    if (*psDec).lossCnt != 0 {
        silk_bwexpander(
            ((*psDecCtrl).PredCoef_Q12[0 as usize]).as_mut_ptr(),
            (*psDec).LPC_order,
            BWE_AFTER_LOSS_Q16,
        );
        silk_bwexpander(
            ((*psDecCtrl).PredCoef_Q12[1 as usize]).as_mut_ptr(),
            (*psDec).LPC_order,
            BWE_AFTER_LOSS_Q16,
        );
    }
    if (*psDec).indices.signalType as i32 == TYPE_VOICED {
        silk_decode_pitch(
            (*psDec).indices.lagIndex,
            (*psDec).indices.contourIndex,
            ((*psDecCtrl).pitchL).as_mut_ptr(),
            (*psDec).fs_kHz,
            (*psDec).nb_subfr,
        );
        cbk_ptr_Q7 = silk_LTP_vq_ptrs_Q7[(*psDec).indices.PERIndex as usize];
        k = 0;
        while k < (*psDec).nb_subfr {
            Ix = (*psDec).indices.LTPIndex[k as usize] as i32;
            i = 0;
            while i < LTP_ORDER {
                (*psDecCtrl).LTPCoef_Q14[(k * LTP_ORDER + i) as usize] =
                    ((*cbk_ptr_Q7.offset((Ix * 5 + i) as isize) as u32) << 7) as i32 as i16;
                i += 1;
            }
            k += 1;
        }
        Ix = (*psDec).indices.LTP_scaleIndex as i32;
        (*psDecCtrl).LTP_scale_Q14 = silk_LTPScales_table_Q14[Ix as usize] as i32;
    } else {
        memset(
            ((*psDecCtrl).pitchL).as_mut_ptr() as *mut core::ffi::c_void,
            0,
            ((*psDec).nb_subfr as u64).wrapping_mul(::core::mem::size_of::<i32>() as u64),
        );
        memset(
            ((*psDecCtrl).LTPCoef_Q14).as_mut_ptr() as *mut core::ffi::c_void,
            0,
            ((5 * (*psDec).nb_subfr) as u64).wrapping_mul(::core::mem::size_of::<i16>() as u64),
        );
        (*psDec).indices.PERIndex = 0;
        (*psDecCtrl).LTP_scale_Q14 = 0;
    };
}
