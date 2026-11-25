use crate::celt::entdec::{ec_dec, ec_dec_icdf};
use crate::silk::define::{
    CODE_CONDITIONALLY, CODE_INDEPENDENTLY, MAX_NB_SUBFR, NLSF_QUANT_MAX_AMPLITUDE, TYPE_VOICED,
};
use crate::silk::structs::silk_decoder_state;
use crate::silk::tables_LTP::{silk_LTP_gain_iCDF_ptrs, silk_LTP_per_index_iCDF};
use crate::silk::tables_gain::{silk_delta_gain_iCDF, silk_gain_iCDF};
use crate::silk::tables_other::{
    silk_LTPscale_iCDF, silk_NLSF_EXT_iCDF, silk_NLSF_interpolation_factor_iCDF,
    silk_type_offset_VAD_iCDF, silk_type_offset_no_VAD_iCDF, silk_uniform4_iCDF,
    silk_uniform8_iCDF,
};
use crate::silk::tables_pitch_lag::{silk_pitch_delta_iCDF, silk_pitch_lag_iCDF};
use crate::silk::NLSF_unpack::silk_NLSF_unpack;

pub unsafe fn silk_decode_indices(
    psDec: *mut silk_decoder_state,
    psRangeDec: &mut ec_dec,
    FrameIndex: i32,
    decode_LBRR: i32,
    condCoding: i32,
) {
    let mut i: i32 = 0;
    let mut k: i32 = 0;
    let mut Ix: i32 = 0;
    let mut decode_absolute_lagIndex: i32 = 0;
    let mut delta_lagIndex: i32 = 0;
    let mut ec_ix: [i16; 16] = [0; 16];
    let mut pred_Q8: [u8; 16] = [0; 16];
    if decode_LBRR != 0 || (*psDec).VAD_flags[FrameIndex as usize] != 0 {
        Ix = ec_dec_icdf(psRangeDec, &silk_type_offset_VAD_iCDF, 8) + 2;
    } else {
        Ix = ec_dec_icdf(psRangeDec, &silk_type_offset_no_VAD_iCDF, 8);
    }
    (*psDec).indices.signalType = (Ix >> 1) as i8;
    (*psDec).indices.quantOffsetType = (Ix & 1) as i8;
    if condCoding == CODE_CONDITIONALLY {
        (*psDec).indices.GainsIndices[0 as usize] =
            ec_dec_icdf(psRangeDec, &silk_delta_gain_iCDF, 8) as i8;
    } else {
        (*psDec).indices.GainsIndices[0 as usize] = ((ec_dec_icdf(
            psRangeDec,
            &silk_gain_iCDF[(*psDec).indices.signalType as usize],
            8,
        ) as u32)
            << 3) as i32 as i8;
        (*psDec).indices.GainsIndices[0 as usize] =
            ((*psDec).indices.GainsIndices[0 as usize] as i32
                + ec_dec_icdf(psRangeDec, &silk_uniform8_iCDF, 8) as i8 as i32) as i8;
    }
    i = 1;
    while i < (*psDec).nb_subfr {
        (*psDec).indices.GainsIndices[i as usize] =
            ec_dec_icdf(psRangeDec, &silk_delta_gain_iCDF, 8) as i8;
        i += 1;
    }
    (*psDec).indices.NLSFIndices[0 as usize] = ec_dec_icdf(
        psRangeDec,
        &(*(*psDec).psNLSF_CB).CB1_iCDF[(((*psDec).indices.signalType as i32 >> 1)
            * (*(*psDec).psNLSF_CB).nVectors as i32)
            as usize..],
        8,
    ) as i8;
    silk_NLSF_unpack(
        &mut ec_ix,
        &mut pred_Q8,
        (*psDec).psNLSF_CB,
        (*psDec).indices.NLSFIndices[0 as usize] as i32,
    );
    assert!((*(*psDec).psNLSF_CB).order as i32 == (*psDec).LPC_order);
    i = 0;
    while i < (*(*psDec).psNLSF_CB).order as i32 {
        Ix = ec_dec_icdf(
            psRangeDec,
            &(*(*psDec).psNLSF_CB).ec_iCDF[*ec_ix.as_mut_ptr().offset(i as isize) as usize..],
            8,
        );
        if Ix == 0 {
            Ix -= ec_dec_icdf(psRangeDec, &silk_NLSF_EXT_iCDF, 8);
        } else if Ix == 2 * NLSF_QUANT_MAX_AMPLITUDE {
            Ix += ec_dec_icdf(psRangeDec, &silk_NLSF_EXT_iCDF, 8);
        }
        (*psDec).indices.NLSFIndices[(i + 1) as usize] = (Ix - NLSF_QUANT_MAX_AMPLITUDE) as i8;
        i += 1;
    }
    if (*psDec).nb_subfr == MAX_NB_SUBFR {
        (*psDec).indices.NLSFInterpCoef_Q2 =
            ec_dec_icdf(psRangeDec, &silk_NLSF_interpolation_factor_iCDF, 8) as i8;
    } else {
        (*psDec).indices.NLSFInterpCoef_Q2 = 4;
    }
    if (*psDec).indices.signalType as i32 == TYPE_VOICED {
        decode_absolute_lagIndex = 1;
        if condCoding == CODE_CONDITIONALLY && (*psDec).ec_prevSignalType == TYPE_VOICED {
            delta_lagIndex = ec_dec_icdf(psRangeDec, &silk_pitch_delta_iCDF, 8) as i16 as i32;
            if delta_lagIndex > 0 {
                delta_lagIndex = delta_lagIndex - 9;
                (*psDec).indices.lagIndex =
                    ((*psDec).ec_prevLagIndex as i32 + delta_lagIndex) as i16;
                decode_absolute_lagIndex = 0;
            }
        }
        if decode_absolute_lagIndex != 0 {
            (*psDec).indices.lagIndex = (ec_dec_icdf(psRangeDec, &silk_pitch_lag_iCDF, 8) as i16
                as i32
                * ((*psDec).fs_kHz >> 1)) as i16;
            (*psDec).indices.lagIndex = ((*psDec).indices.lagIndex as i32
                + ec_dec_icdf(psRangeDec, (*psDec).pitch_lag_low_bits_iCDF, 8) as i16 as i32)
                as i16;
        }
        (*psDec).ec_prevLagIndex = (*psDec).indices.lagIndex;
        (*psDec).indices.contourIndex =
            ec_dec_icdf(psRangeDec, (*psDec).pitch_contour_iCDF, 8) as i8;
        (*psDec).indices.PERIndex = ec_dec_icdf(psRangeDec, &silk_LTP_per_index_iCDF, 8) as i8;
        k = 0;
        while k < (*psDec).nb_subfr {
            (*psDec).indices.LTPIndex[k as usize] = ec_dec_icdf(
                psRangeDec,
                silk_LTP_gain_iCDF_ptrs[(*psDec).indices.PERIndex as usize],
                8,
            ) as i8;
            k += 1;
        }
        if condCoding == CODE_INDEPENDENTLY {
            (*psDec).indices.LTP_scaleIndex = ec_dec_icdf(psRangeDec, &silk_LTPscale_iCDF, 8) as i8;
        } else {
            (*psDec).indices.LTP_scaleIndex = 0;
        }
    }
    (*psDec).ec_prevSignalType = (*psDec).indices.signalType as i32;
    (*psDec).indices.Seed = ec_dec_icdf(psRangeDec, &silk_uniform4_iCDF, 8) as i8;
}
