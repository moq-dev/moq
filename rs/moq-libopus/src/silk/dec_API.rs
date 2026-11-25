#[derive(Copy, Clone)]
#[repr(C)]
pub struct silk_DecControlStruct {
    pub nChannelsAPI: i32,
    pub nChannelsInternal: i32,
    pub API_sampleRate: i32,
    pub internalSampleRate: i32,
    pub payloadSize_ms: i32,
    pub prevPitchLag: i32,
}
pub const FLAG_DECODE_NORMAL: i32 = 0;
pub const FLAG_DECODE_LBRR: i32 = 2;
pub const FLAG_PACKET_LOST: i32 = 1;
pub mod errors_h {
    pub const SILK_DEC_INVALID_SAMPLING_FREQUENCY: i32 = -(200);
    pub const SILK_NO_ERROR: i32 = 0;
    #[allow(unused)]
    pub const SILK_DEC_INVALID_FRAME_SIZE: i32 = -(203);
}
use self::errors_h::{SILK_DEC_INVALID_SAMPLING_FREQUENCY, SILK_NO_ERROR};
use crate::celt::entdec::{ec_dec, ec_dec_bit_logp, ec_dec_icdf};
use crate::externs::{memcpy, memset};
use crate::silk::decode_frame::silk_decode_frame;
use crate::silk::decode_indices::silk_decode_indices;
use crate::silk::decode_pulses::silk_decode_pulses;
use crate::silk::decoder_set_fs::silk_decoder_set_fs;
use crate::silk::define::{
    CODE_CONDITIONALLY, CODE_INDEPENDENTLY, CODE_INDEPENDENTLY_NO_LTP_SCALING,
    DECODER_NUM_CHANNELS, MAX_API_FS_KHZ, TYPE_NO_VOICE_ACTIVITY, TYPE_VOICED,
};
use crate::silk::init_decoder::silk_init_decoder;
use crate::silk::resampler::silk_resampler;
use crate::silk::resampler::ResamplerState;
use crate::silk::stereo_MS_to_LR::silk_stereo_MS_to_LR;
use crate::silk::stereo_decode_pred::{silk_stereo_decode_mid_only, silk_stereo_decode_pred};
use crate::silk::structs::{silk_decoder_state, stereo_dec_state};
use crate::silk::tables_other::silk_LBRR_flags_iCDF_ptr;

#[derive(Copy, Clone)]
#[repr(C)]
pub struct silk_decoder {
    pub channel_state: [silk_decoder_state; 2],
    pub sStereo: stereo_dec_state,
    pub nChannelsAPI: i32,
    pub nChannelsInternal: i32,
    pub prev_decode_only_middle: i32,
}
pub unsafe fn silk_Get_Decoder_Size(decSizeBytes: *mut i32) -> i32 {
    let ret: i32 = SILK_NO_ERROR;
    *decSizeBytes = ::core::mem::size_of::<silk_decoder>() as u64 as i32;
    return ret;
}
pub unsafe fn silk_InitDecoder(decState: *mut core::ffi::c_void) -> i32 {
    let mut n: i32 = 0;
    let mut ret: i32 = SILK_NO_ERROR;
    let channel_state: *mut silk_decoder_state =
        ((*(decState as *mut silk_decoder)).channel_state).as_mut_ptr();
    n = 0;
    while n < DECODER_NUM_CHANNELS {
        ret = silk_init_decoder(&mut *channel_state.offset(n as isize));
        n += 1;
    }
    memset(
        &mut (*(decState as *mut silk_decoder)).sStereo as *mut stereo_dec_state
            as *mut core::ffi::c_void,
        0,
        ::core::mem::size_of::<stereo_dec_state>() as u64,
    );
    (*(decState as *mut silk_decoder)).prev_decode_only_middle = 0;
    return ret;
}
pub unsafe fn silk_Decode(
    decState: *mut core::ffi::c_void,
    decControl: *mut silk_DecControlStruct,
    lostFlag: i32,
    newPacketFlag: i32,
    psRangeDec: &mut ec_dec,
    samplesOut: *mut i16,
    nSamplesOut: *mut i32,
    arch: i32,
) -> i32 {
    let mut i: i32 = 0;
    let mut n: i32 = 0;
    let mut decode_only_middle: i32 = 0;
    let mut ret: i32 = SILK_NO_ERROR;
    let mut nSamplesOutDec: i32 = 0;
    let mut LBRR_symbol: i32 = 0;
    let mut samplesOut1_tmp: [*mut i16; 2] = [0 as *mut i16; 2];
    let mut MS_pred_Q13: [i32; 2] = [0, 0];
    let psDec: *mut silk_decoder = decState as *mut silk_decoder;
    let channel_state: *mut silk_decoder_state = ((*psDec).channel_state).as_mut_ptr();
    let mut has_side: i32 = 0;
    let mut stereo_to_mono: i32 = 0;
    let mut delay_stack_alloc: i32 = 0;
    assert!((*decControl).nChannelsInternal == 1 || (*decControl).nChannelsInternal == 2);
    if newPacketFlag != 0 {
        n = 0;
        while n < (*decControl).nChannelsInternal {
            (*channel_state.offset(n as isize)).nFramesDecoded = 0;
            n += 1;
        }
    }
    if (*decControl).nChannelsInternal > (*psDec).nChannelsInternal {
        ret += silk_init_decoder(&mut *channel_state.offset(1 as isize));
    }
    stereo_to_mono = ((*decControl).nChannelsInternal == 1
        && (*psDec).nChannelsInternal == 2
        && (*decControl).internalSampleRate == 1000 * (*channel_state.offset(0 as isize)).fs_kHz)
        as i32;
    if (*channel_state.offset(0 as isize)).nFramesDecoded == 0 {
        n = 0;
        while n < (*decControl).nChannelsInternal {
            let mut fs_kHz_dec: i32 = 0;
            if (*decControl).payloadSize_ms == 0 {
                (*channel_state.offset(n as isize)).nFramesPerPacket = 1;
                (*channel_state.offset(n as isize)).nb_subfr = 2;
            } else if (*decControl).payloadSize_ms == 10 {
                (*channel_state.offset(n as isize)).nFramesPerPacket = 1;
                (*channel_state.offset(n as isize)).nb_subfr = 2;
            } else if (*decControl).payloadSize_ms == 20 {
                (*channel_state.offset(n as isize)).nFramesPerPacket = 1;
                (*channel_state.offset(n as isize)).nb_subfr = 4;
            } else if (*decControl).payloadSize_ms == 40 {
                (*channel_state.offset(n as isize)).nFramesPerPacket = 2;
                (*channel_state.offset(n as isize)).nb_subfr = 4;
            } else if (*decControl).payloadSize_ms == 60 {
                (*channel_state.offset(n as isize)).nFramesPerPacket = 3;
                (*channel_state.offset(n as isize)).nb_subfr = 4;
            } else {
                // see comments in `[unsafe_libopus::silk::check_control_input]`
                panic!("libopus: assert(0) called");
                // return SILK_DEC_INVALID_FRAME_SIZE;
            }
            fs_kHz_dec = ((*decControl).internalSampleRate >> 10) + 1;
            if fs_kHz_dec != 8 && fs_kHz_dec != 12 && fs_kHz_dec != 16 {
                panic!("libopus: assert(0) called");
                // return SILK_DEC_INVALID_SAMPLING_FREQUENCY;
            }
            ret += silk_decoder_set_fs(
                &mut *channel_state.offset(n as isize),
                fs_kHz_dec,
                (*decControl).API_sampleRate,
            );
            n += 1;
        }
    }
    if (*decControl).nChannelsAPI == 2
        && (*decControl).nChannelsInternal == 2
        && ((*psDec).nChannelsAPI == 1 || (*psDec).nChannelsInternal == 1)
    {
        memset(
            ((*psDec).sStereo.pred_prev_Q13).as_mut_ptr() as *mut core::ffi::c_void,
            0,
            ::core::mem::size_of::<[i16; 2]>() as u64,
        );
        memset(
            ((*psDec).sStereo.sSide).as_mut_ptr() as *mut core::ffi::c_void,
            0,
            ::core::mem::size_of::<[i16; 2]>() as u64,
        );
        memcpy(
            &mut (*channel_state.offset(1 as isize)).resampler_state as *mut ResamplerState
                as *mut core::ffi::c_void,
            &mut (*channel_state.offset(0 as isize)).resampler_state as *mut ResamplerState
                as *const core::ffi::c_void,
            ::core::mem::size_of::<ResamplerState>() as u64,
        );
    }
    (*psDec).nChannelsAPI = (*decControl).nChannelsAPI;
    (*psDec).nChannelsInternal = (*decControl).nChannelsInternal;
    if (*decControl).API_sampleRate > MAX_API_FS_KHZ * 1000 || (*decControl).API_sampleRate < 8000 {
        ret = SILK_DEC_INVALID_SAMPLING_FREQUENCY;
        return ret;
    }
    if lostFlag != FLAG_PACKET_LOST && (*channel_state.offset(0 as isize)).nFramesDecoded == 0 {
        n = 0;
        while n < (*decControl).nChannelsInternal {
            i = 0;
            while i < (*channel_state.offset(n as isize)).nFramesPerPacket {
                (*channel_state.offset(n as isize)).VAD_flags[i as usize] =
                    ec_dec_bit_logp(psRangeDec, 1);
                i += 1;
            }
            (*channel_state.offset(n as isize)).LBRR_flag = ec_dec_bit_logp(psRangeDec, 1);
            n += 1;
        }
        n = 0;
        while n < (*decControl).nChannelsInternal {
            memset(
                ((*channel_state.offset(n as isize)).LBRR_flags).as_mut_ptr()
                    as *mut core::ffi::c_void,
                0,
                ::core::mem::size_of::<[i32; 3]>() as u64,
            );
            if (*channel_state.offset(n as isize)).LBRR_flag != 0 {
                if (*channel_state.offset(n as isize)).nFramesPerPacket == 1 {
                    (*channel_state.offset(n as isize)).LBRR_flags[0 as usize] = 1;
                } else {
                    LBRR_symbol = ec_dec_icdf(
                        psRangeDec,
                        silk_LBRR_flags_iCDF_ptr
                            [((*channel_state.offset(n as isize)).nFramesPerPacket - 2) as usize],
                        8,
                    ) + 1;
                    i = 0;
                    while i < (*channel_state.offset(n as isize)).nFramesPerPacket {
                        (*channel_state.offset(n as isize)).LBRR_flags[i as usize] =
                            LBRR_symbol >> i & 1;
                        i += 1;
                    }
                }
            }
            n += 1;
        }
        if lostFlag == FLAG_DECODE_NORMAL {
            i = 0;
            while i < (*channel_state.offset(0 as isize)).nFramesPerPacket {
                n = 0;
                while n < (*decControl).nChannelsInternal {
                    if (*channel_state.offset(n as isize)).LBRR_flags[i as usize] != 0 {
                        let mut pulses: [i16; 320] = [0; 320];
                        let mut condCoding: i32 = 0;
                        if (*decControl).nChannelsInternal == 2 && n == 0 {
                            silk_stereo_decode_pred(psRangeDec, MS_pred_Q13.as_mut_ptr());
                            if (*channel_state.offset(1 as isize)).LBRR_flags[i as usize] == 0 {
                                silk_stereo_decode_mid_only(psRangeDec, &mut decode_only_middle);
                            }
                        }
                        if i > 0
                            && (*channel_state.offset(n as isize)).LBRR_flags[(i - 1) as usize] != 0
                        {
                            condCoding = CODE_CONDITIONALLY;
                        } else {
                            condCoding = CODE_INDEPENDENTLY;
                        }
                        silk_decode_indices(
                            &mut *channel_state.offset(n as isize),
                            psRangeDec,
                            i,
                            1,
                            condCoding,
                        );
                        silk_decode_pulses(
                            psRangeDec,
                            pulses.as_mut_ptr(),
                            (*channel_state.offset(n as isize)).indices.signalType as i32,
                            (*channel_state.offset(n as isize)).indices.quantOffsetType as i32,
                            (*channel_state.offset(n as isize)).frame_length,
                        );
                    }
                    n += 1;
                }
                i += 1;
            }
        }
    }
    if (*decControl).nChannelsInternal == 2 {
        if lostFlag == FLAG_DECODE_NORMAL
            || lostFlag == FLAG_DECODE_LBRR
                && (*channel_state.offset(0 as isize)).LBRR_flags
                    [(*channel_state.offset(0 as isize)).nFramesDecoded as usize]
                    == 1
        {
            silk_stereo_decode_pred(psRangeDec, MS_pred_Q13.as_mut_ptr());
            if lostFlag == FLAG_DECODE_NORMAL
                && (*channel_state.offset(1 as isize)).VAD_flags
                    [(*channel_state.offset(0 as isize)).nFramesDecoded as usize]
                    == 0
                || lostFlag == FLAG_DECODE_LBRR
                    && (*channel_state.offset(1 as isize)).LBRR_flags
                        [(*channel_state.offset(0 as isize)).nFramesDecoded as usize]
                        == 0
            {
                silk_stereo_decode_mid_only(psRangeDec, &mut decode_only_middle);
            } else {
                decode_only_middle = 0;
            }
        } else {
            n = 0;
            while n < 2 {
                MS_pred_Q13[n as usize] = (*psDec).sStereo.pred_prev_Q13[n as usize] as i32;
                n += 1;
            }
        }
    }
    if (*decControl).nChannelsInternal == 2
        && decode_only_middle == 0
        && (*psDec).prev_decode_only_middle == 1
    {
        memset(
            ((*psDec).channel_state[1 as usize].outBuf).as_mut_ptr() as *mut core::ffi::c_void,
            0,
            ::core::mem::size_of::<[i16; 480]>() as u64,
        );
        memset(
            ((*psDec).channel_state[1 as usize].sLPC_Q14_buf).as_mut_ptr()
                as *mut core::ffi::c_void,
            0,
            ::core::mem::size_of::<[i32; 16]>() as u64,
        );
        (*psDec).channel_state[1 as usize].lagPrev = 100;
        (*psDec).channel_state[1 as usize].LastGainIndex = 10;
        (*psDec).channel_state[1 as usize].prevSignalType = TYPE_NO_VOICE_ACTIVITY;
        (*psDec).channel_state[1 as usize].first_frame_after_reset = 1;
    }
    delay_stack_alloc = ((*decControl).internalSampleRate * (*decControl).nChannelsInternal
        < (*decControl).API_sampleRate * (*decControl).nChannelsAPI) as i32;
    let vla = (if delay_stack_alloc != 0 {
        1
    } else {
        (*decControl).nChannelsInternal * ((*channel_state.offset(0 as isize)).frame_length + 2)
    }) as usize;
    let mut samplesOut1_tmp_storage1: Vec<i16> = ::std::vec::from_elem(0, vla);
    if delay_stack_alloc != 0 {
        samplesOut1_tmp[0 as usize] = samplesOut;
        samplesOut1_tmp[1 as usize] = samplesOut
            .offset((*channel_state.offset(0 as isize)).frame_length as isize)
            .offset(2 as isize);
    } else {
        samplesOut1_tmp[0 as usize] = samplesOut1_tmp_storage1.as_mut_ptr();
        samplesOut1_tmp[1 as usize] = samplesOut1_tmp_storage1
            .as_mut_ptr()
            .offset((*channel_state.offset(0 as isize)).frame_length as isize)
            .offset(2 as isize);
    }
    if lostFlag == FLAG_DECODE_NORMAL {
        has_side = (decode_only_middle == 0) as i32;
    } else {
        has_side = ((*psDec).prev_decode_only_middle == 0
            || (*decControl).nChannelsInternal == 2
                && lostFlag == FLAG_DECODE_LBRR
                && (*channel_state.offset(1 as isize)).LBRR_flags
                    [(*channel_state.offset(1 as isize)).nFramesDecoded as usize]
                    == 1) as i32;
    }
    n = 0;
    while n < (*decControl).nChannelsInternal {
        if n == 0 || has_side != 0 {
            let mut FrameIndex: i32 = 0;
            let mut condCoding_0: i32 = 0;
            FrameIndex = (*channel_state.offset(0 as isize)).nFramesDecoded - n;
            if FrameIndex <= 0 {
                condCoding_0 = CODE_INDEPENDENTLY;
            } else if lostFlag == FLAG_DECODE_LBRR {
                condCoding_0 = if (*channel_state.offset(n as isize)).LBRR_flags
                    [(FrameIndex - 1) as usize]
                    != 0
                {
                    CODE_CONDITIONALLY
                } else {
                    CODE_INDEPENDENTLY
                };
            } else if n > 0 && (*psDec).prev_decode_only_middle != 0 {
                condCoding_0 = CODE_INDEPENDENTLY_NO_LTP_SCALING;
            } else {
                condCoding_0 = CODE_CONDITIONALLY;
            }
            ret += silk_decode_frame(
                &mut *channel_state.offset(n as isize),
                psRangeDec,
                &mut *(*samplesOut1_tmp.as_mut_ptr().offset(n as isize)).offset(2 as isize),
                &mut nSamplesOutDec,
                lostFlag,
                condCoding_0,
                arch,
            );
        } else {
            memset(
                &mut *(*samplesOut1_tmp.as_mut_ptr().offset(n as isize)).offset(2 as isize)
                    as *mut i16 as *mut core::ffi::c_void,
                0,
                (nSamplesOutDec as u64).wrapping_mul(::core::mem::size_of::<i16>() as u64),
            );
        }
        let ref mut fresh0 = (*channel_state.offset(n as isize)).nFramesDecoded;
        *fresh0 += 1;
        n += 1;
    }
    if (*decControl).nChannelsAPI == 2 && (*decControl).nChannelsInternal == 2 {
        silk_stereo_MS_to_LR(
            &mut (*psDec).sStereo,
            samplesOut1_tmp[0 as usize],
            samplesOut1_tmp[1 as usize],
            MS_pred_Q13.as_mut_ptr() as *const i32,
            (*channel_state.offset(0 as isize)).fs_kHz,
            nSamplesOutDec,
        );
    } else {
        memcpy(
            samplesOut1_tmp[0 as usize] as *mut core::ffi::c_void,
            ((*psDec).sStereo.sMid).as_mut_ptr() as *const core::ffi::c_void,
            2_u64.wrapping_mul(::core::mem::size_of::<i16>() as u64),
        );
        memcpy(
            ((*psDec).sStereo.sMid).as_mut_ptr() as *mut core::ffi::c_void,
            &mut *(*samplesOut1_tmp.as_mut_ptr().offset(0 as isize)).offset(nSamplesOutDec as isize)
                as *mut i16 as *const core::ffi::c_void,
            2_u64.wrapping_mul(::core::mem::size_of::<i16>() as u64),
        );
    }
    *nSamplesOut = nSamplesOutDec * (*decControl).API_sampleRate
        / ((*channel_state.offset(0 as isize)).fs_kHz as i16 as i32 * 1000);

    let vla_0 = (if (*decControl).nChannelsAPI == 2 {
        *nSamplesOut
    } else {
        1
    }) as usize;
    let mut samplesOut2_tmp: Vec<i16> = ::std::vec::from_elem(0, vla_0);

    let resample_out_ptr = if (*decControl).nChannelsAPI == 2 {
        samplesOut2_tmp.as_mut_slice()
    } else {
        std::slice::from_raw_parts_mut(samplesOut, *nSamplesOut as usize)
    };

    let vla_1 = (if delay_stack_alloc != 0 {
        (*decControl).nChannelsInternal * ((*channel_state.offset(0 as isize)).frame_length + 2)
    } else {
        1
    }) as usize;
    let mut samplesOut1_tmp_storage2: Vec<i16> = ::std::vec::from_elem(0, vla_1);
    if delay_stack_alloc != 0 {
        memcpy(
            samplesOut1_tmp_storage2.as_mut_ptr() as *mut core::ffi::c_void,
            samplesOut as *const core::ffi::c_void,
            (((*decControl).nChannelsInternal
                * ((*channel_state.offset(0 as isize)).frame_length + 2)) as u64)
                .wrapping_mul(::core::mem::size_of::<i16>() as u64)
                .wrapping_add(
                    (0 * samplesOut1_tmp_storage2
                        .as_mut_ptr()
                        .offset_from(samplesOut) as i64) as u64,
                ),
        );
        samplesOut1_tmp[0 as usize] = samplesOut1_tmp_storage2.as_mut_ptr();
        samplesOut1_tmp[1 as usize] = samplesOut1_tmp_storage2
            .as_mut_ptr()
            .offset((*channel_state.offset(0 as isize)).frame_length as isize)
            .offset(2 as isize);
    }
    n = 0;
    while n
        < (if (*decControl).nChannelsAPI < (*decControl).nChannelsInternal {
            (*decControl).nChannelsAPI
        } else {
            (*decControl).nChannelsInternal
        })
    {
        /* Resample decoded signal to API_sampleRate */
        ret += silk_resampler(
            &mut (*channel_state.offset(n as isize)).resampler_state,
            resample_out_ptr,
            std::slice::from_raw_parts(
                &mut *samplesOut1_tmp[n as usize].offset(1 as isize) as *mut i16 as *const i16,
                nSamplesOutDec as usize,
            ),
        );
        if (*decControl).nChannelsAPI == 2 {
            i = 0;
            while i < *nSamplesOut {
                *samplesOut.offset((n + 2 * i) as isize) = resample_out_ptr[i as usize];
                i += 1;
            }
        }
        n += 1;
    }

    /* Create two channel output from mono stream */
    if (*decControl).nChannelsAPI == 2 && (*decControl).nChannelsInternal == 1 {
        if stereo_to_mono != 0 {
            /* Resample right channel for newly collapsed stereo just in case
            we weren't doing collapsing when switching to mono */
            ret += silk_resampler(
                &mut (*channel_state.offset(1 as isize)).resampler_state,
                resample_out_ptr,
                std::slice::from_raw_parts(
                    &mut *samplesOut1_tmp[0].offset(1 as isize) as *mut i16 as *const i16,
                    nSamplesOutDec as usize,
                ),
            );
            i = 0;
            while i < *nSamplesOut {
                *samplesOut.offset((1 + 2 * i) as isize) = resample_out_ptr[i as usize];
                i += 1;
            }
        } else {
            i = 0;
            while i < *nSamplesOut {
                *samplesOut.offset((1 + 2 * i) as isize) = *samplesOut.offset((0 + 2 * i) as isize);
                i += 1;
            }
        }
    }
    if (*channel_state.offset(0 as isize)).prevSignalType == TYPE_VOICED {
        let mult_tab: [i32; 3] = [6, 4, 3];
        (*decControl).prevPitchLag = (*channel_state.offset(0 as isize)).lagPrev
            * mult_tab[((*channel_state.offset(0 as isize)).fs_kHz - 8 >> 2) as usize];
    } else {
        (*decControl).prevPitchLag = 0;
    }
    if lostFlag == FLAG_PACKET_LOST {
        i = 0;
        while i < (*psDec).nChannelsInternal {
            (*psDec).channel_state[i as usize].LastGainIndex = 10;
            i += 1;
        }
    } else {
        (*psDec).prev_decode_only_middle = decode_only_middle;
    }
    return ret;
}
