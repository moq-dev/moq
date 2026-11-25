use crate::externs::{free, malloc};

pub mod arch_h {
    pub type opus_val16 = f32;
    pub type opus_val32 = f32;
}
pub mod stddef_h {
    pub type size_t = u64;
    pub const NULL: i32 = 0;
}
pub mod xmmintrin_h {
    #[cfg(target_arch = "x86")]
    pub use core::arch::x86::{__m128, _mm_cvt_ss2si, _mm_cvtss_si32, _mm_set_ss};
    #[cfg(target_arch = "x86_64")]
    pub use core::arch::x86_64::{__m128, _mm_cvt_ss2si, _mm_cvtss_si32, _mm_set_ss};
}
pub mod cpu_support_h {
    #[inline]
    pub unsafe fn opus_select_arch() -> i32 {
        return 0;
    }
}
pub mod stack_alloc_h {
    pub const ALLOC_NONE: i32 = 1;
    #[inline]
    pub unsafe fn _opus_false() -> i32 {
        return 0;
    }
}
pub use self::arch_h::{opus_val16, opus_val32};
pub use self::cpu_support_h::opus_select_arch;
pub use self::stack_alloc_h::{_opus_false, ALLOC_NONE};
pub use self::stddef_h::{size_t, NULL};
use crate::celt::celt::CELT_SET_SIGNALLING_REQUEST;
use crate::celt::celt_decoder::{
    celt_decode_with_ec, celt_decoder_get_size, celt_decoder_init, OpusCustomDecoder,
};
use crate::celt::entcode::ec_tell;
use crate::celt::entdec::ec_dec;
use crate::celt::entdec::{ec_dec_bit_logp, ec_dec_init, ec_dec_uint};
use crate::celt::float_cast::FLOAT2INT16;
use crate::celt::mathops::celt_exp2;
use crate::celt::modes::OpusCustomMode;
use crate::externs::memset;
use crate::silk::dec_API::silk_DecControlStruct;
use crate::silk::dec_API::{silk_Decode, silk_Get_Decoder_Size, silk_InitDecoder};
use crate::src::opus::opus_packet_parse_impl;
use crate::src::opus_defines::{
    OPUS_ALLOC_FAIL, OPUS_BAD_ARG, OPUS_BANDWIDTH_FULLBAND, OPUS_BANDWIDTH_MEDIUMBAND,
    OPUS_BANDWIDTH_NARROWBAND, OPUS_BANDWIDTH_SUPERWIDEBAND, OPUS_BANDWIDTH_WIDEBAND,
    OPUS_BUFFER_TOO_SMALL, OPUS_GET_BANDWIDTH_REQUEST, OPUS_GET_FINAL_RANGE_REQUEST,
    OPUS_GET_GAIN_REQUEST, OPUS_GET_LAST_PACKET_DURATION_REQUEST,
    OPUS_GET_PHASE_INVERSION_DISABLED_REQUEST, OPUS_GET_PITCH_REQUEST,
    OPUS_GET_SAMPLE_RATE_REQUEST, OPUS_INTERNAL_ERROR, OPUS_INVALID_PACKET, OPUS_OK,
    OPUS_RESET_STATE, OPUS_SET_GAIN_REQUEST, OPUS_SET_PHASE_INVERSION_DISABLED_REQUEST,
    OPUS_UNIMPLEMENTED,
};
use crate::src::opus_private::{align, MODE_CELT_ONLY, MODE_HYBRID, MODE_SILK_ONLY};
use crate::varargs::VarArgs;
use crate::{opus_custom_decoder_ctl, opus_packet_get_samples_per_frame, opus_pcm_soft_clip};

#[derive(Copy, Clone)]
#[repr(C)]
pub struct OpusDecoder {
    pub(crate) celt_dec_offset: i32,
    pub(crate) silk_dec_offset: i32,
    pub(crate) channels: i32,
    pub(crate) Fs: i32,
    pub(crate) DecControl: silk_DecControlStruct,
    pub(crate) decode_gain: i32,
    pub(crate) arch: i32,
    pub(crate) stream_channels: i32,
    pub(crate) bandwidth: i32,
    pub(crate) mode: i32,
    pub(crate) prev_mode: i32,
    pub(crate) frame_size: i32,
    pub(crate) prev_redundancy: i32,
    pub(crate) last_packet_duration: i32,
    pub(crate) softclip_mem: [opus_val16; 2],
    pub(crate) rangeFinal: u32,
}
unsafe fn validate_opus_decoder(st: *mut OpusDecoder) {
    assert!((*st).channels == 1 || (*st).channels == 2);
    assert!(
        (*st).Fs == 48000
            || (*st).Fs == 24000
            || (*st).Fs == 16000
            || (*st).Fs == 12000
            || (*st).Fs == 8000
    );
    assert!((*st).DecControl.API_sampleRate == (*st).Fs);
    assert!(
        (*st).DecControl.internalSampleRate == 0
            || (*st).DecControl.internalSampleRate == 16000
            || (*st).DecControl.internalSampleRate == 12000
            || (*st).DecControl.internalSampleRate == 8000
    );
    assert!((*st).DecControl.nChannelsAPI == (*st).channels);
    assert!(
        (*st).DecControl.nChannelsInternal == 0
            || (*st).DecControl.nChannelsInternal == 1
            || (*st).DecControl.nChannelsInternal == 2
    );
    assert!(
        (*st).DecControl.payloadSize_ms == 0
            || (*st).DecControl.payloadSize_ms == 10
            || (*st).DecControl.payloadSize_ms == 20
            || (*st).DecControl.payloadSize_ms == 40
            || (*st).DecControl.payloadSize_ms == 60
    );
    assert!((*st).arch >= 0);
    assert!((*st).arch <= 0);
    assert!((*st).stream_channels == 1 || (*st).stream_channels == 2);
}
pub unsafe fn opus_decoder_get_size(channels: i32) -> i32 {
    let mut silkDecSizeBytes: i32 = 0;
    let mut celtDecSizeBytes: i32 = 0;
    let mut ret: i32 = 0;
    if channels < 1 || channels > 2 {
        return 0;
    }
    ret = silk_Get_Decoder_Size(&mut silkDecSizeBytes);
    if ret != 0 {
        return 0;
    }
    silkDecSizeBytes = align(silkDecSizeBytes);
    celtDecSizeBytes = celt_decoder_get_size(channels);
    return align(::core::mem::size_of::<OpusDecoder>() as u64 as i32)
        + silkDecSizeBytes
        + celtDecSizeBytes;
}
pub unsafe fn opus_decoder_init(st: *mut OpusDecoder, Fs: i32, channels: i32) -> i32 {
    let mut silk_dec: *mut core::ffi::c_void = 0 as *mut core::ffi::c_void;
    let mut celt_dec: *mut OpusCustomDecoder = 0 as *mut OpusCustomDecoder;
    let mut ret: i32 = 0;
    let mut silkDecSizeBytes: i32 = 0;
    if Fs != 48000 && Fs != 24000 && Fs != 16000 && Fs != 12000 && Fs != 8000
        || channels != 1 && channels != 2
    {
        return OPUS_BAD_ARG;
    }
    memset(
        st as *mut i8 as *mut core::ffi::c_void,
        0,
        (opus_decoder_get_size(channels) as u64).wrapping_mul(::core::mem::size_of::<i8>() as u64),
    );
    ret = silk_Get_Decoder_Size(&mut silkDecSizeBytes);
    if ret != 0 {
        return OPUS_INTERNAL_ERROR;
    }
    silkDecSizeBytes = align(silkDecSizeBytes);
    (*st).silk_dec_offset = align(::core::mem::size_of::<OpusDecoder>() as u64 as i32);
    (*st).celt_dec_offset = (*st).silk_dec_offset + silkDecSizeBytes;
    silk_dec = (st as *mut i8).offset((*st).silk_dec_offset as isize) as *mut core::ffi::c_void;
    celt_dec = (st as *mut i8).offset((*st).celt_dec_offset as isize) as *mut OpusCustomDecoder;
    (*st).channels = channels;
    (*st).stream_channels = (*st).channels;
    (*st).Fs = Fs;
    (*st).DecControl.API_sampleRate = (*st).Fs;
    (*st).DecControl.nChannelsAPI = (*st).channels;
    ret = silk_InitDecoder(silk_dec);
    if ret != 0 {
        return OPUS_INTERNAL_ERROR;
    }
    ret = celt_decoder_init(celt_dec, Fs, channels);
    if ret != OPUS_OK {
        return OPUS_INTERNAL_ERROR;
    }
    opus_custom_decoder_ctl!(celt_dec, CELT_SET_SIGNALLING_REQUEST, 0);
    (*st).prev_mode = 0;
    (*st).frame_size = Fs / 400;
    (*st).arch = opus_select_arch();
    return OPUS_OK;
}
pub unsafe fn opus_decoder_create(Fs: i32, channels: i32, error: *mut i32) -> *mut OpusDecoder {
    let mut ret: i32 = 0;
    let mut st: *mut OpusDecoder = 0 as *mut OpusDecoder;
    if Fs != 48000 && Fs != 24000 && Fs != 16000 && Fs != 12000 && Fs != 8000
        || channels != 1 && channels != 2
    {
        if !error.is_null() {
            *error = OPUS_BAD_ARG;
        }
        return NULL as *mut OpusDecoder;
    }
    st = malloc(opus_decoder_get_size(channels) as size_t) as *mut OpusDecoder;
    if st.is_null() {
        if !error.is_null() {
            *error = OPUS_ALLOC_FAIL;
        }
        return NULL as *mut OpusDecoder;
    }
    ret = opus_decoder_init(st, Fs, channels);
    if !error.is_null() {
        *error = ret;
    }
    if ret != OPUS_OK {
        free(st as *mut core::ffi::c_void);
        st = NULL as *mut OpusDecoder;
    }
    return st;
}
unsafe fn smooth_fade(
    in1: *const opus_val16,
    in2: *const opus_val16,
    out: *mut opus_val16,
    overlap: i32,
    channels: i32,
    window: *const opus_val16,
    Fs: i32,
) {
    let mut i: i32 = 0;
    let mut c: i32 = 0;
    let inc: i32 = 48000 / Fs;
    c = 0;
    while c < channels {
        i = 0;
        while i < overlap {
            let w: opus_val16 =
                *window.offset((i * inc) as isize) * *window.offset((i * inc) as isize);
            *out.offset((i * channels + c) as isize) = w * *in2.offset((i * channels + c) as isize)
                + (1.0f32 - w) * *in1.offset((i * channels + c) as isize);
            i += 1;
        }
        c += 1;
    }
}
unsafe fn opus_packet_get_mode(data: *const u8) -> i32 {
    let mut mode: i32 = 0;
    if *data.offset(0 as isize) as i32 & 0x80 != 0 {
        mode = MODE_CELT_ONLY;
    } else if *data.offset(0 as isize) as i32 & 0x60 == 0x60 {
        mode = MODE_HYBRID;
    } else {
        mode = MODE_SILK_ONLY;
    }
    return mode;
}
unsafe fn opus_decode_frame(
    st: *mut OpusDecoder,
    mut data: *const u8,
    mut len: i32,
    mut pcm: *mut opus_val16,
    mut frame_size: i32,
    decode_fec: i32,
) -> i32 {
    let mut silk_dec: *mut core::ffi::c_void = 0 as *mut core::ffi::c_void;
    let mut celt_dec: *mut OpusCustomDecoder = 0 as *mut OpusCustomDecoder;
    let mut i: i32 = 0;
    let mut silk_ret: i32 = 0;
    let mut celt_ret: i32 = 0;
    let mut dec: ec_dec = ec_dec {
        buf: &mut [],
        storage: 0,
        end_offs: 0,
        end_window: 0,
        nend_bits: 0,
        nbits_total: 0,
        offs: 0,
        rng: 0,
        val: 0,
        ext: 0,
        rem: 0,
        error: 0,
    };
    let mut silk_frame_size: i32 = 0;
    let mut pcm_silk_size: i32 = 0;
    let mut pcm_transition_silk_size: i32 = 0;
    let mut pcm_transition_celt_size: i32 = 0;
    let mut pcm_transition: *mut opus_val16 = NULL as *mut opus_val16;
    let mut redundant_audio_size: i32 = 0;
    let mut audiosize: i32 = 0;
    let mut mode: i32 = 0;
    let mut bandwidth: i32 = 0;
    let mut transition: i32 = 0;
    let mut start_band: i32 = 0;
    let mut redundancy: i32 = 0;
    let mut redundancy_bytes: i32 = 0;
    let mut celt_to_silk: i32 = 0;
    let mut c: i32 = 0;
    let mut F2_5: i32 = 0;
    let mut F5: i32 = 0;
    let mut F10: i32 = 0;
    let mut F20: i32 = 0;
    let mut window: *const opus_val16 = 0 as *const opus_val16;
    let mut redundant_rng: u32 = 0;
    let mut celt_accum: i32 = 0;
    silk_dec = (st as *mut i8).offset((*st).silk_dec_offset as isize) as *mut core::ffi::c_void;
    celt_dec = (st as *mut i8).offset((*st).celt_dec_offset as isize) as *mut OpusCustomDecoder;
    F20 = (*st).Fs / 50;
    F10 = F20 >> 1;
    F5 = F10 >> 1;
    F2_5 = F5 >> 1;
    if frame_size < F2_5 {
        return OPUS_BUFFER_TOO_SMALL;
    }
    frame_size = if frame_size < (*st).Fs / 25 * 3 {
        frame_size
    } else {
        (*st).Fs / 25 * 3
    };
    if len <= 1 {
        data = NULL as *const u8;
        frame_size = if frame_size < (*st).frame_size {
            frame_size
        } else {
            (*st).frame_size
        };
    }
    if !data.is_null() {
        audiosize = (*st).frame_size;
        mode = (*st).mode;
        bandwidth = (*st).bandwidth;
        dec = ec_dec_init(std::slice::from_raw_parts_mut(
            data as *mut u8,
            len as usize,
        ));
    } else {
        audiosize = frame_size;
        mode = (*st).prev_mode;
        bandwidth = 0;
        if mode == 0 {
            i = 0;
            while i < audiosize * (*st).channels {
                *pcm.offset(i as isize) = 0 as opus_val16;
                i += 1;
            }
            return audiosize;
        }
        if audiosize > F20 {
            loop {
                let ret: i32 = opus_decode_frame(
                    st,
                    NULL as *const u8,
                    0,
                    pcm,
                    if audiosize < F20 { audiosize } else { F20 },
                    0,
                );
                if ret < 0 {
                    return ret;
                }
                pcm = pcm.offset((ret * (*st).channels) as isize);
                audiosize -= ret;
                if !(audiosize > 0) {
                    break;
                }
            }
            return frame_size;
        } else {
            if audiosize < F20 {
                if audiosize > F10 {
                    audiosize = F10;
                } else if mode != MODE_SILK_ONLY && audiosize > F5 && audiosize < F10 {
                    audiosize = F5;
                }
            }
        }
    }
    celt_accum = 0;
    pcm_transition_silk_size = ALLOC_NONE;
    pcm_transition_celt_size = ALLOC_NONE;
    if !data.is_null()
        && (*st).prev_mode > 0
        && (mode == MODE_CELT_ONLY
            && (*st).prev_mode != MODE_CELT_ONLY
            && (*st).prev_redundancy == 0
            || mode != MODE_CELT_ONLY && (*st).prev_mode == MODE_CELT_ONLY)
    {
        transition = 1;
        if mode == MODE_CELT_ONLY {
            pcm_transition_celt_size = F5 * (*st).channels;
        } else {
            pcm_transition_silk_size = F5 * (*st).channels;
        }
    }
    let vla = pcm_transition_celt_size as usize;
    let mut pcm_transition_celt: Vec<opus_val16> = ::std::vec::from_elem(0., vla);
    if transition != 0 && mode == MODE_CELT_ONLY {
        pcm_transition = pcm_transition_celt.as_mut_ptr();
        opus_decode_frame(
            st,
            NULL as *const u8,
            0,
            pcm_transition,
            if F5 < audiosize { F5 } else { audiosize },
            0,
        );
    }
    if audiosize > frame_size {
        return OPUS_BAD_ARG;
    } else {
        frame_size = audiosize;
    }
    pcm_silk_size = if mode != MODE_CELT_ONLY && celt_accum == 0 {
        (if F10 > frame_size { F10 } else { frame_size }) * (*st).channels
    } else {
        ALLOC_NONE
    };
    let vla_0 = pcm_silk_size as usize;
    let mut pcm_silk: Vec<i16> = ::std::vec::from_elem(0, vla_0);
    if mode != MODE_CELT_ONLY {
        let mut lost_flag: i32 = 0;
        let mut decoded_samples: i32 = 0;
        let mut pcm_ptr: *mut i16 = 0 as *mut i16;
        pcm_ptr = pcm_silk.as_mut_ptr();
        if (*st).prev_mode == MODE_CELT_ONLY {
            silk_InitDecoder(silk_dec);
        }
        (*st).DecControl.payloadSize_ms = if 10 > 1000 * audiosize / (*st).Fs {
            10
        } else {
            1000 * audiosize / (*st).Fs
        };
        if !data.is_null() {
            (*st).DecControl.nChannelsInternal = (*st).stream_channels;
            if mode == MODE_SILK_ONLY {
                if bandwidth == OPUS_BANDWIDTH_NARROWBAND {
                    (*st).DecControl.internalSampleRate = 8000;
                } else if bandwidth == OPUS_BANDWIDTH_MEDIUMBAND {
                    (*st).DecControl.internalSampleRate = 12000;
                } else if bandwidth == OPUS_BANDWIDTH_WIDEBAND {
                    (*st).DecControl.internalSampleRate = 16000;
                } else {
                    (*st).DecControl.internalSampleRate = 16000;
                    panic!("libopus: assert(0) called");
                }
            } else {
                (*st).DecControl.internalSampleRate = 16000;
            }
        }
        lost_flag = if data.is_null() { 1 } else { 2 * decode_fec };
        decoded_samples = 0;
        loop {
            let first_frame: i32 = (decoded_samples == 0) as i32;
            silk_ret = silk_Decode(
                silk_dec,
                &mut (*st).DecControl,
                lost_flag,
                first_frame,
                &mut dec,
                pcm_ptr,
                &mut silk_frame_size,
                (*st).arch,
            );
            if silk_ret != 0 {
                if lost_flag != 0 {
                    silk_frame_size = frame_size;
                    i = 0;
                    while i < frame_size * (*st).channels {
                        *pcm_ptr.offset(i as isize) = 0;
                        i += 1;
                    }
                } else {
                    return OPUS_INTERNAL_ERROR;
                }
            }
            pcm_ptr = pcm_ptr.offset((silk_frame_size * (*st).channels) as isize);
            decoded_samples += silk_frame_size;
            if !(decoded_samples < frame_size) {
                break;
            }
        }
    }
    start_band = 0;
    if decode_fec == 0
        && mode != MODE_CELT_ONLY
        && !data.is_null()
        && ec_tell(&mut dec) + 17 + 20 * ((*st).mode == MODE_HYBRID) as i32 <= 8 * len
    {
        if mode == MODE_HYBRID {
            redundancy = ec_dec_bit_logp(&mut dec, 12);
        } else {
            redundancy = 1;
        }
        if redundancy != 0 {
            celt_to_silk = ec_dec_bit_logp(&mut dec, 1);
            redundancy_bytes = if mode == MODE_HYBRID {
                ec_dec_uint(&mut dec, 256) as i32 + 2
            } else {
                len - (ec_tell(&mut dec) + 7 >> 3)
            };
            len -= redundancy_bytes;
            if (len * 8) < ec_tell(&mut dec) {
                len = 0;
                redundancy_bytes = 0;
                redundancy = 0;
            }
            dec.storage = (dec.storage as u32).wrapping_sub(redundancy_bytes as u32) as u32 as u32;
        }
    }
    if mode != MODE_CELT_ONLY {
        start_band = 17;
    }
    if redundancy != 0 {
        transition = 0;
        pcm_transition_silk_size = ALLOC_NONE;
    }
    let vla_1 = pcm_transition_silk_size as usize;
    let mut pcm_transition_silk: Vec<opus_val16> = ::std::vec::from_elem(0., vla_1);
    if transition != 0 && mode != MODE_CELT_ONLY {
        pcm_transition = pcm_transition_silk.as_mut_ptr();
        opus_decode_frame(
            st,
            NULL as *const u8,
            0,
            pcm_transition,
            if F5 < audiosize { F5 } else { audiosize },
            0,
        );
    }
    if bandwidth != 0 {
        let mut endband: i32 = 21;
        match bandwidth {
            OPUS_BANDWIDTH_NARROWBAND => {
                endband = 13;
            }
            OPUS_BANDWIDTH_MEDIUMBAND | OPUS_BANDWIDTH_WIDEBAND => {
                endband = 17;
            }
            OPUS_BANDWIDTH_SUPERWIDEBAND => {
                endband = 19;
            }
            OPUS_BANDWIDTH_FULLBAND => {
                endband = 21;
            }
            _ => {
                panic!("libopus: assert(0) called");
            }
        }
        assert!(opus_custom_decoder_ctl!(celt_dec, 10012, endband) == 0);
    }
    assert!(opus_custom_decoder_ctl!(celt_dec, 10008, (*st).stream_channels) == 0);
    redundant_audio_size = if redundancy != 0 {
        F5 * (*st).channels
    } else {
        ALLOC_NONE
    };
    let vla_2 = redundant_audio_size as usize;
    let mut redundant_audio: Vec<opus_val16> = ::std::vec::from_elem(0., vla_2);
    if redundancy != 0 && celt_to_silk != 0 {
        assert!(opus_custom_decoder_ctl!(celt_dec, 10010, 0) == 0);
        celt_decode_with_ec(
            celt_dec,
            data.offset(len as isize),
            redundancy_bytes,
            redundant_audio.as_mut_ptr(),
            F5,
            None,
            0,
        );
        assert!(opus_custom_decoder_ctl!(celt_dec, 4031, &mut redundant_rng) == 0);
    }
    assert!(opus_custom_decoder_ctl!(celt_dec, 10010, start_band) == 0);
    if mode != MODE_SILK_ONLY {
        let celt_frame_size: i32 = if F20 < frame_size { F20 } else { frame_size };
        if mode != (*st).prev_mode && (*st).prev_mode > 0 && (*st).prev_redundancy == 0 {
            assert!(opus_custom_decoder_ctl!(celt_dec, 4028) == 0);
        }
        celt_ret = celt_decode_with_ec(
            celt_dec,
            if decode_fec != 0 {
                NULL as *const u8
            } else {
                data
            },
            len,
            pcm,
            celt_frame_size,
            Some(&mut dec),
            celt_accum,
        );
    } else {
        let mut silence: [u8; 2] = [0xff, 0xff];
        if celt_accum == 0 {
            i = 0;
            while i < frame_size * (*st).channels {
                *pcm.offset(i as isize) = 0 as opus_val16;
                i += 1;
            }
        }
        if (*st).prev_mode == MODE_HYBRID
            && !(redundancy != 0 && celt_to_silk != 0 && (*st).prev_redundancy != 0)
        {
            assert!(opus_custom_decoder_ctl!(celt_dec, 10010, 0) == 0);
            celt_decode_with_ec(
                celt_dec,
                silence.as_mut_ptr(),
                2,
                pcm,
                F2_5,
                None,
                celt_accum,
            );
        }
    }
    if mode != MODE_CELT_ONLY && celt_accum == 0 {
        i = 0;
        while i < frame_size * (*st).channels {
            *pcm.offset(i as isize) = *pcm.offset(i as isize)
                + 1.0f32 / 32768.0f32 * *pcm_silk.as_mut_ptr().offset(i as isize) as i32 as f32;
            i += 1;
        }
    }
    let mut celt_mode: *const OpusCustomMode = 0 as *const OpusCustomMode;
    assert!(opus_custom_decoder_ctl!(celt_dec, 10015, &mut celt_mode) == 0);
    window = (*celt_mode).window;
    if redundancy != 0 && celt_to_silk == 0 {
        assert!(opus_custom_decoder_ctl!(celt_dec, 4028) == 0);
        assert!(opus_custom_decoder_ctl!(celt_dec, 10010, 0) == 0);
        celt_decode_with_ec(
            celt_dec,
            data.offset(len as isize),
            redundancy_bytes,
            redundant_audio.as_mut_ptr(),
            F5,
            None,
            0,
        );
        assert!(opus_custom_decoder_ctl!(celt_dec, 4031, &mut redundant_rng) == 0);
        smooth_fade(
            pcm.offset(((*st).channels * (frame_size - F2_5)) as isize),
            redundant_audio
                .as_mut_ptr()
                .offset(((*st).channels * F2_5) as isize),
            pcm.offset(((*st).channels * (frame_size - F2_5)) as isize),
            F2_5,
            (*st).channels,
            window,
            (*st).Fs,
        );
    }
    if redundancy != 0 && celt_to_silk != 0 {
        c = 0;
        while c < (*st).channels {
            i = 0;
            while i < F2_5 {
                *pcm.offset(((*st).channels * i + c) as isize) = *redundant_audio
                    .as_mut_ptr()
                    .offset(((*st).channels * i + c) as isize);
                i += 1;
            }
            c += 1;
        }
        smooth_fade(
            redundant_audio
                .as_mut_ptr()
                .offset(((*st).channels * F2_5) as isize),
            pcm.offset(((*st).channels * F2_5) as isize),
            pcm.offset(((*st).channels * F2_5) as isize),
            F2_5,
            (*st).channels,
            window,
            (*st).Fs,
        );
    }
    if transition != 0 {
        if audiosize >= F5 {
            i = 0;
            while i < (*st).channels * F2_5 {
                *pcm.offset(i as isize) = *pcm_transition.offset(i as isize);
                i += 1;
            }
            smooth_fade(
                pcm_transition.offset(((*st).channels * F2_5) as isize),
                pcm.offset(((*st).channels * F2_5) as isize),
                pcm.offset(((*st).channels * F2_5) as isize),
                F2_5,
                (*st).channels,
                window,
                (*st).Fs,
            );
        } else {
            smooth_fade(
                pcm_transition,
                pcm,
                pcm,
                F2_5,
                (*st).channels,
                window,
                (*st).Fs,
            );
        }
    }
    if (*st).decode_gain != 0 {
        let mut gain: opus_val32 = 0.;
        gain = celt_exp2(6.48814081e-4f32 * (*st).decode_gain as f32);
        i = 0;
        while i < frame_size * (*st).channels {
            let mut x: opus_val32 = 0.;
            x = *pcm.offset(i as isize) * gain;
            *pcm.offset(i as isize) = x;
            i += 1;
        }
    }
    if len <= 1 {
        (*st).rangeFinal = 0;
    } else {
        (*st).rangeFinal = dec.rng ^ redundant_rng;
    }
    (*st).prev_mode = mode;
    (*st).prev_redundancy = (redundancy != 0 && celt_to_silk == 0) as i32;
    if celt_ret >= 0 {
        let _ = _opus_false() != 0;
    }
    return if celt_ret < 0 { celt_ret } else { audiosize };
}
pub unsafe fn opus_decode_native(
    st: *mut OpusDecoder,
    mut data: *const u8,
    len: i32,
    pcm: *mut opus_val16,
    frame_size: i32,
    decode_fec: i32,
    self_delimited: i32,
    packet_offset: *mut i32,
    soft_clip: i32,
) -> i32 {
    let mut i: i32 = 0;
    let mut nb_samples: i32 = 0;
    let mut count: i32 = 0;
    let mut offset: i32 = 0;
    let mut toc: u8 = 0;
    let mut packet_frame_size: i32 = 0;
    let mut packet_bandwidth: i32 = 0;
    let mut packet_mode: i32 = 0;
    let mut packet_stream_channels: i32 = 0;
    let mut size: [i16; 48] = [0; 48];
    validate_opus_decoder(st);
    if decode_fec < 0 || decode_fec > 1 {
        return OPUS_BAD_ARG;
    }
    if (decode_fec != 0 || len == 0 || data.is_null()) && frame_size % ((*st).Fs / 400) != 0 {
        return OPUS_BAD_ARG;
    }
    if len == 0 || data.is_null() {
        let mut pcm_count: i32 = 0;
        loop {
            let mut ret: i32 = 0;
            ret = opus_decode_frame(
                st,
                NULL as *const u8,
                0,
                pcm.offset((pcm_count * (*st).channels) as isize),
                frame_size - pcm_count,
                0,
            );
            if ret < 0 {
                return ret;
            }
            pcm_count += ret;
            if !(pcm_count < frame_size) {
                break;
            }
        }
        assert!(pcm_count == frame_size);
        let _ = _opus_false() != 0;
        (*st).last_packet_duration = pcm_count;
        return pcm_count;
    } else {
        if len < 0 {
            return OPUS_BAD_ARG;
        }
    }
    packet_mode = opus_packet_get_mode(data);
    packet_bandwidth = opus_packet_get_bandwidth(data);
    packet_frame_size = opus_packet_get_samples_per_frame(data, (*st).Fs);
    packet_stream_channels = opus_packet_get_nb_channels(data);
    count = opus_packet_parse_impl(
        data,
        len,
        self_delimited,
        &mut toc,
        NULL as *mut *const u8,
        size.as_mut_ptr(),
        &mut offset,
        packet_offset,
    );
    if count < 0 {
        return count;
    }
    data = data.offset(offset as isize);
    if decode_fec != 0 {
        let mut duration_copy: i32 = 0;
        let mut ret_0: i32 = 0;
        if frame_size < packet_frame_size
            || packet_mode == MODE_CELT_ONLY
            || (*st).mode == MODE_CELT_ONLY
        {
            return opus_decode_native(
                st,
                NULL as *const u8,
                0,
                pcm,
                frame_size,
                0,
                0,
                NULL as *mut i32,
                soft_clip,
            );
        }
        duration_copy = (*st).last_packet_duration;
        if frame_size - packet_frame_size != 0 {
            ret_0 = opus_decode_native(
                st,
                NULL as *const u8,
                0,
                pcm,
                frame_size - packet_frame_size,
                0,
                0,
                NULL as *mut i32,
                soft_clip,
            );
            if ret_0 < 0 {
                (*st).last_packet_duration = duration_copy;
                return ret_0;
            }
            assert!(ret_0 == frame_size - packet_frame_size);
        }
        (*st).mode = packet_mode;
        (*st).bandwidth = packet_bandwidth;
        (*st).frame_size = packet_frame_size;
        (*st).stream_channels = packet_stream_channels;
        ret_0 = opus_decode_frame(
            st,
            data,
            size[0 as usize] as i32,
            pcm.offset(((*st).channels * (frame_size - packet_frame_size)) as isize),
            packet_frame_size,
            1,
        );
        if ret_0 < 0 {
            return ret_0;
        } else {
            let _ = _opus_false() != 0;
            (*st).last_packet_duration = frame_size;
            return frame_size;
        }
    }
    if count * packet_frame_size > frame_size {
        return OPUS_BUFFER_TOO_SMALL;
    }
    (*st).mode = packet_mode;
    (*st).bandwidth = packet_bandwidth;
    (*st).frame_size = packet_frame_size;
    (*st).stream_channels = packet_stream_channels;
    nb_samples = 0;
    i = 0;
    while i < count {
        let mut ret_1: i32 = 0;
        ret_1 = opus_decode_frame(
            st,
            data,
            size[i as usize] as i32,
            pcm.offset((nb_samples * (*st).channels) as isize),
            frame_size - nb_samples,
            0,
        );
        if ret_1 < 0 {
            return ret_1;
        }
        assert!(ret_1 == packet_frame_size);
        data = data.offset(size[i as usize] as i32 as isize);
        nb_samples += ret_1;
        i += 1;
    }
    (*st).last_packet_duration = nb_samples;
    let _ = _opus_false() != 0;
    if soft_clip != 0 {
        opus_pcm_soft_clip(
            pcm,
            nb_samples,
            (*st).channels,
            ((*st).softclip_mem).as_mut_ptr(),
        );
    } else {
        (*st).softclip_mem[1 as usize] = 0 as opus_val16;
        (*st).softclip_mem[0 as usize] = (*st).softclip_mem[1 as usize];
    }
    return nb_samples;
}
pub unsafe fn opus_decode(
    st: *mut OpusDecoder,
    data: *const u8,
    len: i32,
    pcm: *mut i16,
    mut frame_size: i32,
    decode_fec: i32,
) -> i32 {
    let mut ret: i32 = 0;
    let mut i: i32 = 0;
    let mut nb_samples: i32 = 0;
    if frame_size <= 0 {
        return OPUS_BAD_ARG;
    }
    if !data.is_null() && len > 0 && decode_fec == 0 {
        nb_samples = opus_decoder_get_nb_samples(st, data, len);
        if nb_samples > 0 {
            frame_size = if frame_size < nb_samples {
                frame_size
            } else {
                nb_samples
            };
        } else {
            return OPUS_INVALID_PACKET;
        }
    }
    assert!((*st).channels == 1 || (*st).channels == 2);
    let vla = (frame_size * (*st).channels) as usize;
    let mut out: Vec<f32> = ::std::vec::from_elem(0., vla);
    ret = opus_decode_native(
        st,
        data,
        len,
        out.as_mut_ptr(),
        frame_size,
        decode_fec,
        0,
        NULL as *mut i32,
        1,
    );
    if ret > 0 {
        i = 0;
        while i < ret * (*st).channels {
            *pcm.offset(i as isize) = FLOAT2INT16(*out.as_mut_ptr().offset(i as isize));
            i += 1;
        }
    }
    return ret;
}
pub unsafe fn opus_decode_float(
    st: *mut OpusDecoder,
    data: *const u8,
    len: i32,
    pcm: *mut opus_val16,
    frame_size: i32,
    decode_fec: i32,
) -> i32 {
    if frame_size <= 0 {
        return OPUS_BAD_ARG;
    }
    return opus_decode_native(
        st,
        data,
        len,
        pcm,
        frame_size,
        decode_fec,
        0,
        NULL as *mut i32,
        0,
    );
}
pub unsafe fn opus_decoder_ctl_impl(st: *mut OpusDecoder, request: i32, args: VarArgs) -> i32 {
    let mut silk_dec: *mut core::ffi::c_void = 0 as *mut core::ffi::c_void;
    let mut celt_dec: *mut OpusCustomDecoder = 0 as *mut OpusCustomDecoder;
    silk_dec = (st as *mut i8).offset((*st).silk_dec_offset as isize) as *mut core::ffi::c_void;
    celt_dec = (st as *mut i8).offset((*st).celt_dec_offset as isize) as *mut OpusCustomDecoder;

    let mut ap = args;

    match request {
        OPUS_GET_BANDWIDTH_REQUEST => {
            let value = ap.arg::<&mut i32>();
            *value = (*st).bandwidth;
            OPUS_OK
        }
        OPUS_GET_FINAL_RANGE_REQUEST => {
            let value_0 = ap.arg::<&mut u32>();
            *value_0 = (*st).rangeFinal;
            OPUS_OK
        }
        OPUS_RESET_STATE => {
            memset(
                &mut (*st).stream_channels as *mut i32 as *mut i8 as *mut core::ffi::c_void,
                0,
                (::core::mem::size_of::<OpusDecoder>() as u64)
                    .wrapping_sub(
                        (&mut (*st).stream_channels as *mut i32 as *mut i8)
                            .offset_from(st as *mut i8) as i64 as u64,
                    )
                    .wrapping_mul(::core::mem::size_of::<i8>() as u64),
            );
            opus_custom_decoder_ctl!(celt_dec, OPUS_RESET_STATE);
            silk_InitDecoder(silk_dec);
            (*st).stream_channels = (*st).channels;
            (*st).frame_size = (*st).Fs / 400;
            OPUS_OK
        }
        OPUS_GET_SAMPLE_RATE_REQUEST => {
            let value_1 = ap.arg::<&mut i32>();
            *value_1 = (*st).Fs;
            OPUS_OK
        }
        OPUS_GET_PITCH_REQUEST => {
            let value_2 = ap.arg::<&mut i32>();
            if (*st).prev_mode == MODE_CELT_ONLY {
                opus_custom_decoder_ctl!(celt_dec, OPUS_GET_PITCH_REQUEST, value_2,)
            } else {
                *value_2 = (*st).DecControl.prevPitchLag;
                OPUS_OK
            }
        }
        OPUS_GET_GAIN_REQUEST => {
            let value_3 = ap.arg::<&mut i32>();
            *value_3 = (*st).decode_gain;
            OPUS_OK
        }
        OPUS_SET_GAIN_REQUEST => {
            let value_4: i32 = ap.arg::<i32>();
            if value_4 < -(32768) || value_4 > 32767 {
                OPUS_BAD_ARG
            } else {
                (*st).decode_gain = value_4;
                OPUS_OK
            }
        }
        OPUS_GET_LAST_PACKET_DURATION_REQUEST => {
            let value_5 = ap.arg::<&mut i32>();
            *value_5 = (*st).last_packet_duration;
            OPUS_OK
        }
        OPUS_SET_PHASE_INVERSION_DISABLED_REQUEST => {
            let value_6: i32 = ap.arg::<i32>();
            if value_6 < 0 || value_6 > 1 {
                OPUS_BAD_ARG
            } else {
                opus_custom_decoder_ctl!(
                    celt_dec,
                    OPUS_SET_PHASE_INVERSION_DISABLED_REQUEST,
                    value_6
                )
            }
        }
        OPUS_GET_PHASE_INVERSION_DISABLED_REQUEST => {
            let value_7 = ap.arg::<&mut i32>();
            opus_custom_decoder_ctl!(celt_dec, OPUS_GET_PHASE_INVERSION_DISABLED_REQUEST, value_7)
        }
        _ => OPUS_UNIMPLEMENTED,
    }
}
#[macro_export]
macro_rules! opus_decoder_ctl {
    ($st:expr,$request:expr, $($arg:expr),*) => {
        $crate::opus_decoder_ctl_impl(
            $st,
            $request,
            $crate::varargs!($($arg),*)
        )
    };
    ($st:expr,$request:expr, $($arg:expr),*,) => {
        opus_decoder_ctl!($st, $request, $($arg),*)
    };
    ($st:expr,$request:expr) => {
        opus_decoder_ctl!($st, $request,)
    };
}
pub unsafe fn opus_decoder_destroy(st: *mut OpusDecoder) {
    free(st as *mut core::ffi::c_void);
}
pub unsafe fn opus_packet_get_bandwidth(data: *const u8) -> i32 {
    let mut bandwidth: i32 = 0;
    if *data.offset(0 as isize) as i32 & 0x80 != 0 {
        bandwidth = OPUS_BANDWIDTH_MEDIUMBAND + (*data.offset(0 as isize) as i32 >> 5 & 0x3);
        if bandwidth == OPUS_BANDWIDTH_MEDIUMBAND {
            bandwidth = OPUS_BANDWIDTH_NARROWBAND;
        }
    } else if *data.offset(0 as isize) as i32 & 0x60 == 0x60 {
        bandwidth = if *data.offset(0 as isize) as i32 & 0x10 != 0 {
            OPUS_BANDWIDTH_FULLBAND
        } else {
            OPUS_BANDWIDTH_SUPERWIDEBAND
        };
    } else {
        bandwidth = OPUS_BANDWIDTH_NARROWBAND + (*data.offset(0 as isize) as i32 >> 5 & 0x3);
    }
    return bandwidth;
}
pub unsafe fn opus_packet_get_nb_channels(data: *const u8) -> i32 {
    return if *data.offset(0 as isize) as i32 & 0x4 != 0 {
        2
    } else {
        1
    };
}
pub unsafe fn opus_packet_get_nb_frames(packet: *const u8, len: i32) -> i32 {
    let mut count: i32 = 0;
    if len < 1 {
        return OPUS_BAD_ARG;
    }
    count = *packet.offset(0 as isize) as i32 & 0x3;
    if count == 0 {
        return 1;
    } else if count != 3 {
        return 2;
    } else if len < 2 {
        return OPUS_INVALID_PACKET;
    } else {
        return *packet.offset(1 as isize) as i32 & 0x3f;
    };
}
pub unsafe fn opus_packet_get_nb_samples(packet: *const u8, len: i32, Fs: i32) -> i32 {
    let mut samples: i32 = 0;
    let count: i32 = opus_packet_get_nb_frames(packet, len);
    if count < 0 {
        return count;
    }
    samples = count * opus_packet_get_samples_per_frame(packet, Fs);
    if samples * 25 > Fs * 3 {
        return OPUS_INVALID_PACKET;
    } else {
        return samples;
    };
}
pub unsafe fn opus_decoder_get_nb_samples(
    dec: *const OpusDecoder,
    packet: *const u8,
    len: i32,
) -> i32 {
    return opus_packet_get_nb_samples(packet, len, (*dec).Fs);
}
