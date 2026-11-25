use crate::externs::{free, malloc};

#[derive(Copy, Clone)]
#[repr(C)]
pub struct OpusMSEncoder {
    pub(crate) layout: ChannelLayout,
    pub(crate) arch: i32,
    pub(crate) lfe_stream: i32,
    pub(crate) application: i32,
    pub(crate) variable_duration: i32,
    pub(crate) mapping_type: MappingType,
    pub(crate) bitrate_bps: i32,
}
pub type MappingType = u32;
pub const MAPPING_TYPE_AMBISONICS: MappingType = 2;
pub const MAPPING_TYPE_SURROUND: MappingType = 1;
pub const MAPPING_TYPE_NONE: MappingType = 0;
pub type opus_copy_channel_in_func = Option<
    unsafe fn(
        *mut opus_val16,
        i32,
        *const core::ffi::c_void,
        i32,
        i32,
        i32,
        *mut core::ffi::c_void,
    ) -> (),
>;
pub mod arch_h {
    pub type opus_val32 = f32;
    pub type opus_val16 = f32;
}
pub mod stddef_h {
    pub type size_t = u64;
    pub const NULL: i32 = 0;
}

pub mod cpu_support_h {
    #[inline]
    pub unsafe fn opus_select_arch() -> i32 {
        return 0;
    }
}
pub use self::arch_h::{opus_val16, opus_val32};
pub use self::cpu_support_h::opus_select_arch;
pub use self::stddef_h::{size_t, NULL};
use crate::celt::bands::compute_band_energies;
use crate::celt::celt::resampling_factor;
use crate::celt::celt::{
    CELT_GET_MODE_REQUEST, OPUS_SET_ENERGY_MASK_REQUEST, OPUS_SET_LFE_REQUEST,
};
use crate::celt::celt_encoder::celt_preemphasis;
use crate::celt::mathops::{celt_log2, isqrt32};
use crate::celt::mdct::clt_mdct_forward_c;
use crate::celt::modes::OpusCustomMode;
use crate::celt::pitch::celt_inner_prod_c;
use crate::celt::quant_bands::amp2Log2;
use crate::externs::{memcpy, memset};
use crate::src::analysis::downmix_func;
use crate::src::opus_defines::{
    OPUS_ALLOC_FAIL, OPUS_AUTO, OPUS_BAD_ARG, OPUS_BITRATE_MAX, OPUS_BUFFER_TOO_SMALL,
    OPUS_FRAMESIZE_ARG, OPUS_GET_APPLICATION_REQUEST, OPUS_GET_BANDWIDTH_REQUEST,
    OPUS_GET_BITRATE_REQUEST, OPUS_GET_COMPLEXITY_REQUEST, OPUS_GET_DTX_REQUEST,
    OPUS_GET_EXPERT_FRAME_DURATION_REQUEST, OPUS_GET_FINAL_RANGE_REQUEST,
    OPUS_GET_FORCE_CHANNELS_REQUEST, OPUS_GET_INBAND_FEC_REQUEST, OPUS_GET_LOOKAHEAD_REQUEST,
    OPUS_GET_LSB_DEPTH_REQUEST, OPUS_GET_PACKET_LOSS_PERC_REQUEST,
    OPUS_GET_PHASE_INVERSION_DISABLED_REQUEST, OPUS_GET_PREDICTION_DISABLED_REQUEST,
    OPUS_GET_SAMPLE_RATE_REQUEST, OPUS_GET_SIGNAL_REQUEST, OPUS_GET_VBR_CONSTRAINT_REQUEST,
    OPUS_GET_VBR_REQUEST, OPUS_INTERNAL_ERROR, OPUS_OK, OPUS_RESET_STATE,
    OPUS_SET_APPLICATION_REQUEST, OPUS_SET_BANDWIDTH_REQUEST, OPUS_SET_BITRATE_REQUEST,
    OPUS_SET_COMPLEXITY_REQUEST, OPUS_SET_DTX_REQUEST, OPUS_SET_EXPERT_FRAME_DURATION_REQUEST,
    OPUS_SET_FORCE_CHANNELS_REQUEST, OPUS_SET_INBAND_FEC_REQUEST, OPUS_SET_LSB_DEPTH_REQUEST,
    OPUS_SET_MAX_BANDWIDTH_REQUEST, OPUS_SET_PACKET_LOSS_PERC_REQUEST,
    OPUS_SET_PHASE_INVERSION_DISABLED_REQUEST, OPUS_SET_PREDICTION_DISABLED_REQUEST,
    OPUS_SET_SIGNAL_REQUEST, OPUS_SET_VBR_CONSTRAINT_REQUEST, OPUS_SET_VBR_REQUEST,
    OPUS_UNIMPLEMENTED,
};
use crate::src::opus_encoder::{downmix_float, downmix_int, frame_size_select, opus_encode_native};
use crate::src::opus_multistream::{
    get_left_channel, get_mono_channel, get_right_channel, validate_layout, ChannelLayout,
};
use crate::src::opus_private::{align, OPUS_GET_VOICE_RATIO_REQUEST, OPUS_SET_FORCE_MODE_REQUEST};
use crate::varargs::VarArgs;
use crate::{
    opus_encoder_ctl, opus_encoder_get_size, opus_encoder_init, opus_repacketizer_cat,
    opus_repacketizer_get_nb_frames, opus_repacketizer_init, opus_repacketizer_out_range_impl,
    OpusEncoder, OpusRepacketizer,
};

pub const OPUS_MULTISTREAM_GET_ENCODER_STATE_REQUEST: i32 = 5120;

#[derive(Copy, Clone)]
#[repr(C)]
pub struct VorbisLayout {
    pub nb_streams: i32,
    pub nb_coupled_streams: i32,
    pub mapping: [u8; 8],
}
static mut vorbis_mappings: [VorbisLayout; 8] = [
    {
        let init = VorbisLayout {
            nb_streams: 1,
            nb_coupled_streams: 0,
            mapping: [0, 0, 0, 0, 0, 0, 0, 0],
        };
        init
    },
    {
        let init = VorbisLayout {
            nb_streams: 1,
            nb_coupled_streams: 1,
            mapping: [0, 1, 0, 0, 0, 0, 0, 0],
        };
        init
    },
    {
        let init = VorbisLayout {
            nb_streams: 2,
            nb_coupled_streams: 1,
            mapping: [0, 2, 1, 0, 0, 0, 0, 0],
        };
        init
    },
    {
        let init = VorbisLayout {
            nb_streams: 2,
            nb_coupled_streams: 2,
            mapping: [0, 1, 2, 3, 0, 0, 0, 0],
        };
        init
    },
    {
        let init = VorbisLayout {
            nb_streams: 3,
            nb_coupled_streams: 2,
            mapping: [0, 4, 1, 2, 3, 0, 0, 0],
        };
        init
    },
    {
        let init = VorbisLayout {
            nb_streams: 4,
            nb_coupled_streams: 2,
            mapping: [0, 4, 1, 2, 3, 5, 0, 0],
        };
        init
    },
    {
        let init = VorbisLayout {
            nb_streams: 4,
            nb_coupled_streams: 3,
            mapping: [0, 4, 1, 2, 3, 5, 6, 0],
        };
        init
    },
    {
        let init = VorbisLayout {
            nb_streams: 5,
            nb_coupled_streams: 3,
            mapping: [0, 6, 1, 2, 3, 4, 5, 7],
        };
        init
    },
];
unsafe fn ms_get_preemph_mem(st: *mut OpusMSEncoder) -> *mut opus_val32 {
    let mut s: i32 = 0;
    let mut ptr: *mut i8 = 0 as *mut i8;
    let mut coupled_size: i32 = 0;
    let mut mono_size: i32 = 0;
    coupled_size = opus_encoder_get_size(2);
    mono_size = opus_encoder_get_size(1);
    ptr = (st as *mut i8)
        .offset(align(::core::mem::size_of::<OpusMSEncoder>() as u64 as i32) as isize);
    s = 0;
    while s < (*st).layout.nb_streams {
        if s < (*st).layout.nb_coupled_streams {
            ptr = ptr.offset(align(coupled_size) as isize);
        } else {
            ptr = ptr.offset(align(mono_size) as isize);
        }
        s += 1;
    }
    return ptr.offset(
        (((*st).layout.nb_channels * 120) as u64)
            .wrapping_mul(::core::mem::size_of::<opus_val32>() as u64) as isize,
    ) as *mut core::ffi::c_void as *mut opus_val32;
}
unsafe fn ms_get_window_mem(st: *mut OpusMSEncoder) -> *mut opus_val32 {
    let mut s: i32 = 0;
    let mut ptr: *mut i8 = 0 as *mut i8;
    let mut coupled_size: i32 = 0;
    let mut mono_size: i32 = 0;
    coupled_size = opus_encoder_get_size(2);
    mono_size = opus_encoder_get_size(1);
    ptr = (st as *mut i8)
        .offset(align(::core::mem::size_of::<OpusMSEncoder>() as u64 as i32) as isize);
    s = 0;
    while s < (*st).layout.nb_streams {
        if s < (*st).layout.nb_coupled_streams {
            ptr = ptr.offset(align(coupled_size) as isize);
        } else {
            ptr = ptr.offset(align(mono_size) as isize);
        }
        s += 1;
    }
    return ptr as *mut core::ffi::c_void as *mut opus_val32;
}
unsafe fn validate_ambisonics(
    nb_channels: i32,
    nb_streams: *mut i32,
    nb_coupled_streams: *mut i32,
) -> i32 {
    let mut order_plus_one: i32 = 0;
    let mut acn_channels: i32 = 0;
    let mut nondiegetic_channels: i32 = 0;
    if nb_channels < 1 || nb_channels > 227 {
        return 0;
    }
    order_plus_one = isqrt32(nb_channels as u32) as i32;
    acn_channels = order_plus_one * order_plus_one;
    nondiegetic_channels = nb_channels - acn_channels;
    if nondiegetic_channels != 0 && nondiegetic_channels != 2 {
        return 0;
    }
    if !nb_streams.is_null() {
        *nb_streams = acn_channels + (nondiegetic_channels != 0) as i32;
    }
    if !nb_coupled_streams.is_null() {
        *nb_coupled_streams = (nondiegetic_channels != 0) as i32;
    }
    return 1;
}
unsafe fn validate_encoder_layout(layout: *const ChannelLayout) -> i32 {
    let mut s: i32 = 0;
    s = 0;
    while s < (*layout).nb_streams {
        if s < (*layout).nb_coupled_streams {
            if get_left_channel(layout, s, -1) == -1 {
                return 0;
            }
            if get_right_channel(layout, s, -1) == -1 {
                return 0;
            }
        } else if get_mono_channel(layout, s, -1) == -1 {
            return 0;
        }
        s += 1;
    }
    return 1;
}
unsafe fn channel_pos(channels: i32, pos: *mut i32) {
    if channels == 4 {
        *pos.offset(0 as isize) = 1;
        *pos.offset(1 as isize) = 3;
        *pos.offset(2 as isize) = 1;
        *pos.offset(3 as isize) = 3;
    } else if channels == 3 || channels == 5 || channels == 6 {
        *pos.offset(0 as isize) = 1;
        *pos.offset(1 as isize) = 2;
        *pos.offset(2 as isize) = 3;
        *pos.offset(3 as isize) = 1;
        *pos.offset(4 as isize) = 3;
        *pos.offset(5 as isize) = 0;
    } else if channels == 7 {
        *pos.offset(0 as isize) = 1;
        *pos.offset(1 as isize) = 2;
        *pos.offset(2 as isize) = 3;
        *pos.offset(3 as isize) = 1;
        *pos.offset(4 as isize) = 3;
        *pos.offset(5 as isize) = 2;
        *pos.offset(6 as isize) = 0;
    } else if channels == 8 {
        *pos.offset(0 as isize) = 1;
        *pos.offset(1 as isize) = 2;
        *pos.offset(2 as isize) = 3;
        *pos.offset(3 as isize) = 1;
        *pos.offset(4 as isize) = 3;
        *pos.offset(5 as isize) = 1;
        *pos.offset(6 as isize) = 3;
        *pos.offset(7 as isize) = 0;
    }
}
unsafe fn logSum(a: opus_val16, b: opus_val16) -> opus_val16 {
    let mut max: opus_val16 = 0.;
    let mut diff: opus_val32 = 0.;
    let mut frac: opus_val16 = 0.;
    static mut diff_table: [opus_val16; 17] = [
        0.5000000f32,
        0.2924813f32,
        0.1609640f32,
        0.0849625f32,
        0.0437314f32,
        0.0221971f32,
        0.0111839f32,
        0.0056136f32,
        0.0028123f32,
        0.,
        0.,
        0.,
        0.,
        0.,
        0.,
        0.,
        0.,
    ];
    let mut low: i32 = 0;
    if a > b {
        max = a;
        diff = a - b;
    } else {
        max = b;
        diff = b - a;
    }
    if !(diff < 8.0f32) {
        return max;
    }
    low = (2 as f32 * diff).floor() as i32;
    frac = 2 as f32 * diff - low as f32;
    return max
        + diff_table[low as usize]
        + frac * (diff_table[(low + 1) as usize] - diff_table[low as usize]);
}
pub unsafe fn surround_analysis(
    celt_mode: *const OpusCustomMode,
    pcm: *const core::ffi::c_void,
    bandLogE: *mut opus_val16,
    mem: *mut opus_val32,
    preemph_mem: *mut opus_val32,
    len: i32,
    overlap: i32,
    channels: i32,
    rate: i32,
    copy_channel_in: opus_copy_channel_in_func,
    arch: i32,
) {
    let mut c: i32 = 0;
    let mut i: i32 = 0;
    let mut LM: i32 = 0;
    let mut pos: [i32; 8] = [0, 0, 0, 0, 0, 0, 0, 0];
    let mut upsample: i32 = 0;
    let mut frame_size: i32 = 0;
    let mut freq_size: i32 = 0;
    let mut channel_offset: opus_val16 = 0.;
    let mut bandE: [opus_val32; 21] = [0.; 21];
    let mut maskLogE: [[opus_val16; 21]; 3] = [[0.; 21]; 3];
    upsample = resampling_factor(rate);
    frame_size = len * upsample;
    freq_size = if (960) < frame_size { 960 } else { frame_size };
    LM = 0;
    while LM < (*celt_mode).maxLM {
        if (*celt_mode).shortMdctSize << LM == frame_size {
            break;
        }
        LM += 1;
    }
    let vla = (frame_size + overlap) as usize;
    let mut in_0: Vec<opus_val32> = ::std::vec::from_elem(0., vla);
    let vla_0 = len as usize;
    let mut x: Vec<opus_val16> = ::std::vec::from_elem(0., vla_0);
    let vla_1 = freq_size as usize;
    let mut freq: Vec<opus_val32> = ::std::vec::from_elem(0., vla_1);
    channel_pos(channels, pos.as_mut_ptr());
    c = 0;
    while c < 3 {
        i = 0;
        while i < 21 {
            maskLogE[c as usize][i as usize] = -28.0f32;
            i += 1;
        }
        c += 1;
    }
    c = 0;
    while c < channels {
        let mut frame: i32 = 0;
        let nb_frames: i32 = frame_size / freq_size;
        assert!(nb_frames * freq_size == frame_size);
        memcpy(
            in_0.as_mut_ptr() as *mut core::ffi::c_void,
            mem.offset((c * overlap) as isize) as *const core::ffi::c_void,
            (overlap as u64)
                .wrapping_mul(::core::mem::size_of::<opus_val32>() as u64)
                .wrapping_add(
                    (0 * in_0
                        .as_mut_ptr()
                        .offset_from(mem.offset((c * overlap) as isize))
                        as i64) as u64,
                ),
        );
        (Some(copy_channel_in.expect("non-null function pointer")))
            .expect("non-null function pointer")(
            x.as_mut_ptr(),
            1,
            pcm,
            channels,
            c,
            len,
            NULL as *mut core::ffi::c_void,
        );
        celt_preemphasis(
            x.as_mut_ptr(),
            in_0.as_mut_ptr().offset(overlap as isize),
            frame_size,
            1,
            upsample,
            ((*celt_mode).preemph).as_ptr(),
            preemph_mem.offset(c as isize),
            0,
        );
        let mut sum: opus_val32 = 0.;
        sum = celt_inner_prod_c(in_0.as_mut_ptr(), in_0.as_mut_ptr(), frame_size + overlap);
        if !(sum < 1e18f32) || sum != sum {
            memset(
                in_0.as_mut_ptr() as *mut core::ffi::c_void,
                0,
                ((frame_size + overlap) as u64)
                    .wrapping_mul(::core::mem::size_of::<opus_val32>() as u64),
            );
            *preemph_mem.offset(c as isize) = 0 as opus_val32;
        }
        memset(
            bandE.as_mut_ptr() as *mut core::ffi::c_void,
            0,
            21_u64.wrapping_mul(::core::mem::size_of::<opus_val32>() as u64),
        );
        frame = 0;
        while frame < nb_frames {
            let mut tmpE: [opus_val32; 21] = [0.; 21];
            clt_mdct_forward_c(
                &(*celt_mode).mdct,
                in_0.as_mut_ptr().offset((960 * frame) as isize),
                freq.as_mut_ptr(),
                (*celt_mode).window,
                overlap,
                (*celt_mode).maxLM - LM,
                1,
                arch,
            );
            if upsample != 1 {
                let bound: i32 = freq_size / upsample;
                i = 0;
                while i < bound {
                    let ref mut fresh0 = *freq.as_mut_ptr().offset(i as isize);
                    *fresh0 *= upsample as f32;
                    i += 1;
                }
                while i < freq_size {
                    *freq.as_mut_ptr().offset(i as isize) = 0 as opus_val32;
                    i += 1;
                }
            }
            compute_band_energies(
                celt_mode,
                freq.as_mut_ptr(),
                tmpE.as_mut_ptr(),
                21,
                1,
                LM,
                arch,
            );
            i = 0;
            while i < 21 {
                bandE[i as usize] = if bandE[i as usize] > tmpE[i as usize] {
                    bandE[i as usize]
                } else {
                    tmpE[i as usize]
                };
                i += 1;
            }
            frame += 1;
        }
        amp2Log2(
            celt_mode,
            21,
            21,
            bandE.as_mut_ptr(),
            bandLogE.offset((21 * c) as isize),
            1,
        );
        i = 1;
        while i < 21 {
            *bandLogE.offset((21 * c + i) as isize) = if *bandLogE.offset((21 * c + i) as isize)
                > *bandLogE.offset((21 * c + i - 1) as isize) - 1.0f32
            {
                *bandLogE.offset((21 * c + i) as isize)
            } else {
                *bandLogE.offset((21 * c + i - 1) as isize) - 1.0f32
            };
            i += 1;
        }
        i = 19;
        while i >= 0 {
            *bandLogE.offset((21 * c + i) as isize) = if *bandLogE.offset((21 * c + i) as isize)
                > *bandLogE.offset((21 * c + i + 1) as isize) - 2.0f32
            {
                *bandLogE.offset((21 * c + i) as isize)
            } else {
                *bandLogE.offset((21 * c + i + 1) as isize) - 2.0f32
            };
            i -= 1;
        }
        if pos[c as usize] == 1 {
            i = 0;
            while i < 21 {
                maskLogE[0 as usize][i as usize] = logSum(
                    maskLogE[0 as usize][i as usize],
                    *bandLogE.offset((21 * c + i) as isize),
                );
                i += 1;
            }
        } else if pos[c as usize] == 3 {
            i = 0;
            while i < 21 {
                maskLogE[2 as usize][i as usize] = logSum(
                    maskLogE[2 as usize][i as usize],
                    *bandLogE.offset((21 * c + i) as isize),
                );
                i += 1;
            }
        } else if pos[c as usize] == 2 {
            i = 0;
            while i < 21 {
                maskLogE[0 as usize][i as usize] = logSum(
                    maskLogE[0 as usize][i as usize],
                    *bandLogE.offset((21 * c + i) as isize) - 0.5f32,
                );
                maskLogE[2 as usize][i as usize] = logSum(
                    maskLogE[2 as usize][i as usize],
                    *bandLogE.offset((21 * c + i) as isize) - 0.5f32,
                );
                i += 1;
            }
        }
        memcpy(
            mem.offset((c * overlap) as isize) as *mut core::ffi::c_void,
            in_0.as_mut_ptr().offset(frame_size as isize) as *const core::ffi::c_void,
            (overlap as u64)
                .wrapping_mul(::core::mem::size_of::<opus_val32>() as u64)
                .wrapping_add(
                    (0 * mem
                        .offset((c * overlap) as isize)
                        .offset_from(in_0.as_mut_ptr().offset(frame_size as isize))
                        as i64) as u64,
                ),
        );
        c += 1;
    }
    i = 0;
    while i < 21 {
        maskLogE[1 as usize][i as usize] =
            if maskLogE[0 as usize][i as usize] < maskLogE[2 as usize][i as usize] {
                maskLogE[0 as usize][i as usize]
            } else {
                maskLogE[2 as usize][i as usize]
            };
        i += 1;
    }
    channel_offset = 0.5f32 * celt_log2(2.0f32 / (channels - 1) as f32);
    c = 0;
    while c < 3 {
        i = 0;
        while i < 21 {
            maskLogE[c as usize][i as usize] += channel_offset;
            i += 1;
        }
        c += 1;
    }
    c = 0;
    while c < channels {
        let mut mask: *mut opus_val16 = 0 as *mut opus_val16;
        if pos[c as usize] != 0 {
            mask = &mut *(*maskLogE
                .as_mut_ptr()
                .offset((*pos.as_mut_ptr().offset(c as isize) - 1) as isize))
            .as_mut_ptr()
            .offset(0 as isize) as *mut opus_val16;
            i = 0;
            while i < 21 {
                *bandLogE.offset((21 * c + i) as isize) =
                    *bandLogE.offset((21 * c + i) as isize) - *mask.offset(i as isize);
                i += 1;
            }
        } else {
            i = 0;
            while i < 21 {
                *bandLogE.offset((21 * c + i) as isize) = 0 as opus_val16;
                i += 1;
            }
        }
        c += 1;
    }
}
pub unsafe fn opus_multistream_encoder_get_size(nb_streams: i32, nb_coupled_streams: i32) -> i32 {
    let mut coupled_size: i32 = 0;
    let mut mono_size: i32 = 0;
    if nb_streams < 1 || nb_coupled_streams > nb_streams || nb_coupled_streams < 0 {
        return 0;
    }
    coupled_size = opus_encoder_get_size(2);
    mono_size = opus_encoder_get_size(1);
    return align(::core::mem::size_of::<OpusMSEncoder>() as u64 as i32)
        + nb_coupled_streams * align(coupled_size)
        + (nb_streams - nb_coupled_streams) * align(mono_size);
}
pub unsafe fn opus_multistream_surround_encoder_get_size(
    channels: i32,
    mapping_family: i32,
) -> i32 {
    let mut nb_streams: i32 = 0;
    let mut nb_coupled_streams: i32 = 0;
    let mut size: i32 = 0;
    if mapping_family == 0 {
        if channels == 1 {
            nb_streams = 1;
            nb_coupled_streams = 0;
        } else if channels == 2 {
            nb_streams = 1;
            nb_coupled_streams = 1;
        } else {
            return 0;
        }
    } else if mapping_family == 1 && channels <= 8 && channels >= 1 {
        nb_streams = vorbis_mappings[(channels - 1) as usize].nb_streams;
        nb_coupled_streams = vorbis_mappings[(channels - 1) as usize].nb_coupled_streams;
    } else if mapping_family == 255 {
        nb_streams = channels;
        nb_coupled_streams = 0;
    } else if mapping_family == 2 {
        if validate_ambisonics(channels, &mut nb_streams, &mut nb_coupled_streams) == 0 {
            return 0;
        }
    } else {
        return 0;
    }
    size = opus_multistream_encoder_get_size(nb_streams, nb_coupled_streams);
    if channels > 2 {
        size = (size as u64).wrapping_add(
            (channels as u64).wrapping_mul(
                120_u64
                    .wrapping_mul(::core::mem::size_of::<opus_val32>() as u64)
                    .wrapping_add(::core::mem::size_of::<opus_val32>() as u64),
            ),
        ) as i32 as i32;
    }
    return size;
}
unsafe fn opus_multistream_encoder_init_impl(
    st: *mut OpusMSEncoder,
    Fs: i32,
    channels: i32,
    streams: i32,
    coupled_streams: i32,
    mapping: *const u8,
    application: i32,
    mapping_type: MappingType,
) -> i32 {
    let mut coupled_size: i32 = 0;
    let mut mono_size: i32 = 0;
    let mut i: i32 = 0;
    let mut ret: i32 = 0;
    let mut ptr: *mut i8 = 0 as *mut i8;
    if channels > 255
        || channels < 1
        || coupled_streams > streams
        || streams < 1
        || coupled_streams < 0
        || streams > 255 - coupled_streams
    {
        return OPUS_BAD_ARG;
    }
    (*st).arch = opus_select_arch();
    (*st).layout.nb_channels = channels;
    (*st).layout.nb_streams = streams;
    (*st).layout.nb_coupled_streams = coupled_streams;
    if mapping_type as u32 != MAPPING_TYPE_SURROUND as i32 as u32 {
        (*st).lfe_stream = -1;
    }
    (*st).bitrate_bps = OPUS_AUTO;
    (*st).application = application;
    (*st).variable_duration = OPUS_FRAMESIZE_ARG;
    i = 0;
    while i < (*st).layout.nb_channels {
        (*st).layout.mapping[i as usize] = *mapping.offset(i as isize);
        i += 1;
    }
    if validate_layout(&mut (*st).layout) == 0 {
        return OPUS_BAD_ARG;
    }
    if mapping_type as u32 == MAPPING_TYPE_SURROUND as i32 as u32
        && validate_encoder_layout(&mut (*st).layout) == 0
    {
        return OPUS_BAD_ARG;
    }
    if mapping_type as u32 == MAPPING_TYPE_AMBISONICS as i32 as u32
        && validate_ambisonics((*st).layout.nb_channels, NULL as *mut i32, NULL as *mut i32) == 0
    {
        return OPUS_BAD_ARG;
    }
    ptr = (st as *mut i8)
        .offset(align(::core::mem::size_of::<OpusMSEncoder>() as u64 as i32) as isize);
    coupled_size = opus_encoder_get_size(2);
    mono_size = opus_encoder_get_size(1);
    i = 0;
    while i < (*st).layout.nb_coupled_streams {
        ret = opus_encoder_init(ptr as *mut OpusEncoder, Fs, 2, application);
        if ret != OPUS_OK {
            return ret;
        }
        if i == (*st).lfe_stream {
            opus_encoder_ctl!(ptr as *mut OpusEncoder, OPUS_SET_LFE_REQUEST, 1,);
        }
        ptr = ptr.offset(align(coupled_size) as isize);
        i += 1;
    }
    while i < (*st).layout.nb_streams {
        ret = opus_encoder_init(ptr as *mut OpusEncoder, Fs, 1, application);
        if i == (*st).lfe_stream {
            opus_encoder_ctl!(ptr as *mut OpusEncoder, OPUS_SET_LFE_REQUEST, 1,);
        }
        if ret != OPUS_OK {
            return ret;
        }
        ptr = ptr.offset(align(mono_size) as isize);
        i += 1;
    }
    if mapping_type as u32 == MAPPING_TYPE_SURROUND as i32 as u32 {
        memset(
            ms_get_preemph_mem(st) as *mut core::ffi::c_void,
            0,
            (channels as u64).wrapping_mul(::core::mem::size_of::<opus_val32>() as u64),
        );
        memset(
            ms_get_window_mem(st) as *mut core::ffi::c_void,
            0,
            ((channels * 120) as u64).wrapping_mul(::core::mem::size_of::<opus_val32>() as u64),
        );
    }
    (*st).mapping_type = mapping_type;
    return OPUS_OK;
}
pub unsafe fn opus_multistream_encoder_init(
    st: *mut OpusMSEncoder,
    Fs: i32,
    channels: i32,
    streams: i32,
    coupled_streams: i32,
    mapping: *const u8,
    application: i32,
) -> i32 {
    return opus_multistream_encoder_init_impl(
        st,
        Fs,
        channels,
        streams,
        coupled_streams,
        mapping,
        application,
        MAPPING_TYPE_NONE,
    );
}
pub unsafe fn opus_multistream_surround_encoder_init(
    st: *mut OpusMSEncoder,
    Fs: i32,
    channels: i32,
    mapping_family: i32,
    streams: *mut i32,
    coupled_streams: *mut i32,
    mapping: *mut u8,
    application: i32,
) -> i32 {
    let mut mapping_type: MappingType = MAPPING_TYPE_NONE;
    if channels > 255 || channels < 1 {
        return OPUS_BAD_ARG;
    }
    (*st).lfe_stream = -1;
    if mapping_family == 0 {
        if channels == 1 {
            *streams = 1;
            *coupled_streams = 0;
            *mapping.offset(0 as isize) = 0;
        } else if channels == 2 {
            *streams = 1;
            *coupled_streams = 1;
            *mapping.offset(0 as isize) = 0;
            *mapping.offset(1 as isize) = 1;
        } else {
            return OPUS_UNIMPLEMENTED;
        }
    } else if mapping_family == 1 && channels <= 8 && channels >= 1 {
        let mut i: i32 = 0;
        *streams = vorbis_mappings[(channels - 1) as usize].nb_streams;
        *coupled_streams = vorbis_mappings[(channels - 1) as usize].nb_coupled_streams;
        i = 0;
        while i < channels {
            *mapping.offset(i as isize) =
                vorbis_mappings[(channels - 1) as usize].mapping[i as usize];
            i += 1;
        }
        if channels >= 6 {
            (*st).lfe_stream = *streams - 1;
        }
    } else if mapping_family == 255 {
        let mut i_0: i32 = 0;
        *streams = channels;
        *coupled_streams = 0;
        i_0 = 0;
        while i_0 < channels {
            *mapping.offset(i_0 as isize) = i_0 as u8;
            i_0 += 1;
        }
    } else if mapping_family == 2 {
        let mut i_1: i32 = 0;
        if validate_ambisonics(channels, streams, coupled_streams) == 0 {
            return OPUS_BAD_ARG;
        }
        i_1 = 0;
        while i_1 < *streams - *coupled_streams {
            *mapping.offset(i_1 as isize) = (i_1 + *coupled_streams * 2) as u8;
            i_1 += 1;
        }
        i_1 = 0;
        while i_1 < *coupled_streams * 2 {
            *mapping.offset((i_1 + (*streams - *coupled_streams)) as isize) = i_1 as u8;
            i_1 += 1;
        }
    } else {
        return OPUS_UNIMPLEMENTED;
    }
    if channels > 2 && mapping_family == 1 {
        mapping_type = MAPPING_TYPE_SURROUND;
    } else if mapping_family == 2 {
        mapping_type = MAPPING_TYPE_AMBISONICS;
    } else {
        mapping_type = MAPPING_TYPE_NONE;
    }
    return opus_multistream_encoder_init_impl(
        st,
        Fs,
        channels,
        *streams,
        *coupled_streams,
        mapping,
        application,
        mapping_type,
    );
}
pub unsafe fn opus_multistream_encoder_create(
    Fs: i32,
    channels: i32,
    streams: i32,
    coupled_streams: i32,
    mapping: *const u8,
    application: i32,
    error: *mut i32,
) -> *mut OpusMSEncoder {
    let mut ret: i32 = 0;
    let mut st: *mut OpusMSEncoder = 0 as *mut OpusMSEncoder;
    if channels > 255
        || channels < 1
        || coupled_streams > streams
        || streams < 1
        || coupled_streams < 0
        || streams > 255 - coupled_streams
    {
        if !error.is_null() {
            *error = OPUS_BAD_ARG;
        }
        return NULL as *mut OpusMSEncoder;
    }
    st = malloc(opus_multistream_encoder_get_size(streams, coupled_streams) as size_t)
        as *mut OpusMSEncoder;
    if st.is_null() {
        if !error.is_null() {
            *error = OPUS_ALLOC_FAIL;
        }
        return NULL as *mut OpusMSEncoder;
    }
    ret = opus_multistream_encoder_init(
        st,
        Fs,
        channels,
        streams,
        coupled_streams,
        mapping,
        application,
    );
    if ret != OPUS_OK {
        free(st as *mut core::ffi::c_void);
        st = NULL as *mut OpusMSEncoder;
    }
    if !error.is_null() {
        *error = ret;
    }
    return st;
}
pub unsafe fn opus_multistream_surround_encoder_create(
    Fs: i32,
    channels: i32,
    mapping_family: i32,
    streams: *mut i32,
    coupled_streams: *mut i32,
    mapping: *mut u8,
    application: i32,
    error: *mut i32,
) -> *mut OpusMSEncoder {
    let mut ret: i32 = 0;
    let mut size: i32 = 0;
    let mut st: *mut OpusMSEncoder = 0 as *mut OpusMSEncoder;
    if channels > 255 || channels < 1 {
        if !error.is_null() {
            *error = OPUS_BAD_ARG;
        }
        return NULL as *mut OpusMSEncoder;
    }
    size = opus_multistream_surround_encoder_get_size(channels, mapping_family);
    if size == 0 {
        if !error.is_null() {
            *error = OPUS_UNIMPLEMENTED;
        }
        return NULL as *mut OpusMSEncoder;
    }
    st = malloc(size as size_t) as *mut OpusMSEncoder;
    if st.is_null() {
        if !error.is_null() {
            *error = OPUS_ALLOC_FAIL;
        }
        return NULL as *mut OpusMSEncoder;
    }
    ret = opus_multistream_surround_encoder_init(
        st,
        Fs,
        channels,
        mapping_family,
        streams,
        coupled_streams,
        mapping,
        application,
    );
    if ret != OPUS_OK {
        free(st as *mut core::ffi::c_void);
        st = NULL as *mut OpusMSEncoder;
    }
    if !error.is_null() {
        *error = ret;
    }
    return st;
}
unsafe fn surround_rate_allocation(
    st: *mut OpusMSEncoder,
    rate: *mut i32,
    frame_size: i32,
    Fs: i32,
) {
    let mut i: i32 = 0;
    let mut channel_rate: i32 = 0;
    let mut stream_offset: i32 = 0;
    let mut lfe_offset: i32 = 0;
    let mut coupled_ratio: i32 = 0;
    let mut lfe_ratio: i32 = 0;
    let mut nb_lfe: i32 = 0;
    let mut nb_uncoupled: i32 = 0;
    let mut nb_coupled: i32 = 0;
    let mut nb_normal: i32 = 0;
    let mut channel_offset: i32 = 0;
    let mut bitrate: i32 = 0;
    let mut total: i32 = 0;
    nb_lfe = ((*st).lfe_stream != -1) as i32;
    nb_coupled = (*st).layout.nb_coupled_streams;
    nb_uncoupled = (*st).layout.nb_streams - nb_coupled - nb_lfe;
    nb_normal = 2 * nb_coupled + nb_uncoupled;
    channel_offset = 40
        * (if 50 > Fs / frame_size {
            50
        } else {
            Fs / frame_size
        });
    if (*st).bitrate_bps == OPUS_AUTO {
        bitrate = nb_normal * (channel_offset + Fs + 10000) + 8000 * nb_lfe;
    } else if (*st).bitrate_bps == OPUS_BITRATE_MAX {
        bitrate = nb_normal * 300000 + nb_lfe * 128000;
    } else {
        bitrate = (*st).bitrate_bps;
    }
    lfe_offset = (if (bitrate / 20) < 3000 {
        bitrate / 20
    } else {
        3000
    }) + 15
        * (if 50 > Fs / frame_size {
            50
        } else {
            Fs / frame_size
        });
    stream_offset = (bitrate - channel_offset * nb_normal - lfe_offset * nb_lfe) / nb_normal / 2;
    stream_offset = if 0
        > (if (20000) < stream_offset {
            20000
        } else {
            stream_offset
        }) {
        0
    } else if (20000) < stream_offset {
        20000
    } else {
        stream_offset
    };
    coupled_ratio = 512;
    lfe_ratio = 32;
    total = (nb_uncoupled << 8) + coupled_ratio * nb_coupled + nb_lfe * lfe_ratio;
    channel_rate = (256
        * (bitrate
            - lfe_offset * nb_lfe
            - stream_offset * (nb_coupled + nb_uncoupled)
            - channel_offset * nb_normal) as i64
        / total as i64) as i32;
    i = 0;
    while i < (*st).layout.nb_streams {
        if i < (*st).layout.nb_coupled_streams {
            *rate.offset(i as isize) = 2 * channel_offset
                + (if 0 > stream_offset + (channel_rate * coupled_ratio >> 8) {
                    0
                } else {
                    stream_offset + (channel_rate * coupled_ratio >> 8)
                });
        } else if i != (*st).lfe_stream {
            *rate.offset(i as isize) = channel_offset
                + (if 0 > stream_offset + channel_rate {
                    0
                } else {
                    stream_offset + channel_rate
                });
        } else {
            *rate.offset(i as isize) = if 0 > lfe_offset + (channel_rate * lfe_ratio >> 8) {
                0
            } else {
                lfe_offset + (channel_rate * lfe_ratio >> 8)
            };
        }
        i += 1;
    }
}
unsafe fn ambisonics_rate_allocation(
    st: *mut OpusMSEncoder,
    rate: *mut i32,
    frame_size: i32,
    Fs: i32,
) {
    let mut i: i32 = 0;
    let mut total_rate: i32 = 0;
    let mut per_stream_rate: i32 = 0;
    let nb_channels: i32 = (*st).layout.nb_streams + (*st).layout.nb_coupled_streams;
    if (*st).bitrate_bps == OPUS_AUTO {
        total_rate = ((*st).layout.nb_coupled_streams + (*st).layout.nb_streams)
            * (Fs + 60 * Fs / frame_size)
            + (*st).layout.nb_streams * 15000;
    } else if (*st).bitrate_bps == OPUS_BITRATE_MAX {
        total_rate = nb_channels * 320000;
    } else {
        total_rate = (*st).bitrate_bps;
    }
    per_stream_rate = total_rate / (*st).layout.nb_streams;
    i = 0;
    while i < (*st).layout.nb_streams {
        *rate.offset(i as isize) = per_stream_rate;
        i += 1;
    }
}
unsafe fn rate_allocation(st: *mut OpusMSEncoder, rate: *mut i32, frame_size: i32) -> i32 {
    let mut i: i32 = 0;
    let mut rate_sum: i32 = 0;
    let mut Fs: i32 = 0;
    let mut ptr: *mut i8 = 0 as *mut i8;
    ptr = (st as *mut i8)
        .offset(align(::core::mem::size_of::<OpusMSEncoder>() as u64 as i32) as isize);
    opus_encoder_ctl!(
        ptr as *mut OpusEncoder,
        OPUS_GET_SAMPLE_RATE_REQUEST,
        &mut Fs
    );
    if (*st).mapping_type as u32 == MAPPING_TYPE_AMBISONICS as i32 as u32 {
        ambisonics_rate_allocation(st, rate, frame_size, Fs);
    } else {
        surround_rate_allocation(st, rate, frame_size, Fs);
    }
    i = 0;
    while i < (*st).layout.nb_streams {
        *rate.offset(i as isize) = if *rate.offset(i as isize) > 500 {
            *rate.offset(i as isize)
        } else {
            500
        };
        rate_sum += *rate.offset(i as isize);
        i += 1;
    }
    return rate_sum;
}
pub unsafe fn opus_multistream_encode_native(
    st: *mut OpusMSEncoder,
    copy_channel_in: opus_copy_channel_in_func,
    pcm: *const core::ffi::c_void,
    analysis_frame_size: i32,
    mut data: *mut u8,
    mut max_data_bytes: i32,
    lsb_depth: i32,
    downmix: downmix_func,
    float_api: i32,
    user_data: *mut core::ffi::c_void,
) -> i32 {
    let mut Fs: i32 = 0;
    let mut coupled_size: i32 = 0;
    let mut mono_size: i32 = 0;
    let mut s: i32 = 0;
    let mut ptr: *mut i8 = 0 as *mut i8;
    let mut tot_size: i32 = 0;
    let mut tmp_data: [u8; 7662] = [0; 7662];
    let mut rp: OpusRepacketizer = OpusRepacketizer {
        toc: 0,
        nb_frames: 0,
        frames: [0 as *const u8; 48],
        len: [0; 48],
        framesize: 0,
    };
    let mut vbr: i32 = 0;
    let mut celt_mode: *const OpusCustomMode = 0 as *const OpusCustomMode;
    let mut bitrates: [i32; 256] = [0; 256];
    let mut bandLogE: [opus_val16; 42] = [0.; 42];
    let mut mem: *mut opus_val32 = NULL as *mut opus_val32;
    let mut preemph_mem: *mut opus_val32 = NULL as *mut opus_val32;
    let mut frame_size: i32 = 0;
    let mut rate_sum: i32 = 0;
    let mut smallest_packet: i32 = 0;
    if (*st).mapping_type as u32 == MAPPING_TYPE_SURROUND as i32 as u32 {
        preemph_mem = ms_get_preemph_mem(st);
        mem = ms_get_window_mem(st);
    }
    ptr = (st as *mut i8)
        .offset(align(::core::mem::size_of::<OpusMSEncoder>() as u64 as i32) as isize);
    opus_encoder_ctl!(
        ptr as *mut OpusEncoder,
        OPUS_GET_SAMPLE_RATE_REQUEST,
        &mut Fs
    );
    opus_encoder_ctl!(ptr as *mut OpusEncoder, OPUS_GET_VBR_REQUEST, &mut vbr);
    opus_encoder_ctl!(
        ptr as *mut OpusEncoder,
        CELT_GET_MODE_REQUEST,
        &mut celt_mode
    );
    frame_size = frame_size_select(analysis_frame_size, (*st).variable_duration, Fs);
    if frame_size <= 0 {
        return OPUS_BAD_ARG;
    }
    smallest_packet = (*st).layout.nb_streams * 2 - 1;
    if Fs / frame_size == 10 {
        smallest_packet += (*st).layout.nb_streams;
    }
    if max_data_bytes < smallest_packet {
        return OPUS_BUFFER_TOO_SMALL;
    }
    let vla = (2 * frame_size) as usize;
    let mut buf: Vec<opus_val16> = ::std::vec::from_elem(0., vla);
    coupled_size = opus_encoder_get_size(2);
    mono_size = opus_encoder_get_size(1);
    let vla_0 = (21 * (*st).layout.nb_channels) as usize;
    let mut bandSMR: Vec<opus_val16> = ::std::vec::from_elem(0., vla_0);
    if (*st).mapping_type as u32 == MAPPING_TYPE_SURROUND as i32 as u32 {
        surround_analysis(
            celt_mode,
            pcm,
            bandSMR.as_mut_ptr(),
            mem,
            preemph_mem,
            frame_size,
            120,
            (*st).layout.nb_channels,
            Fs,
            copy_channel_in,
            (*st).arch,
        );
    }
    rate_sum = rate_allocation(st, bitrates.as_mut_ptr(), frame_size);
    if vbr == 0 {
        if (*st).bitrate_bps == OPUS_AUTO {
            max_data_bytes = if max_data_bytes < 3 * rate_sum / (3 * 8 * Fs / frame_size) {
                max_data_bytes
            } else {
                3 * rate_sum / (3 * 8 * Fs / frame_size)
            };
        } else if (*st).bitrate_bps != OPUS_BITRATE_MAX {
            max_data_bytes = if max_data_bytes
                < (if smallest_packet > 3 * (*st).bitrate_bps / (3 * 8 * Fs / frame_size) {
                    smallest_packet
                } else {
                    3 * (*st).bitrate_bps / (3 * 8 * Fs / frame_size)
                }) {
                max_data_bytes
            } else if smallest_packet > 3 * (*st).bitrate_bps / (3 * 8 * Fs / frame_size) {
                smallest_packet
            } else {
                3 * (*st).bitrate_bps / (3 * 8 * Fs / frame_size)
            };
        }
    }
    ptr = (st as *mut i8)
        .offset(align(::core::mem::size_of::<OpusMSEncoder>() as u64 as i32) as isize);
    s = 0;
    while s < (*st).layout.nb_streams {
        let mut enc: *mut OpusEncoder = 0 as *mut OpusEncoder;
        enc = ptr as *mut OpusEncoder;
        if s < (*st).layout.nb_coupled_streams {
            ptr = ptr.offset(align(coupled_size) as isize);
        } else {
            ptr = ptr.offset(align(mono_size) as isize);
        }
        opus_encoder_ctl!(enc, OPUS_SET_BITRATE_REQUEST, bitrates[s as usize]);
        if (*st).mapping_type as u32 == MAPPING_TYPE_SURROUND as i32 as u32 {
            let mut equiv_rate: i32 = 0;
            equiv_rate = (*st).bitrate_bps;
            if (frame_size * 50) < Fs {
                equiv_rate -= 60 * (Fs / frame_size - 50) * (*st).layout.nb_channels;
            }
            if equiv_rate > 10000 * (*st).layout.nb_channels {
                opus_encoder_ctl!(enc, OPUS_SET_BANDWIDTH_REQUEST, 1105);
            } else if equiv_rate > 7000 * (*st).layout.nb_channels {
                opus_encoder_ctl!(enc, OPUS_SET_BANDWIDTH_REQUEST, 1104);
            } else if equiv_rate > 5000 * (*st).layout.nb_channels {
                opus_encoder_ctl!(enc, OPUS_SET_BANDWIDTH_REQUEST, 1103);
            } else {
                opus_encoder_ctl!(enc, OPUS_SET_BANDWIDTH_REQUEST, 1101);
            }
            if s < (*st).layout.nb_coupled_streams {
                opus_encoder_ctl!(enc, OPUS_SET_FORCE_MODE_REQUEST, 1002);
                opus_encoder_ctl!(enc, OPUS_SET_FORCE_CHANNELS_REQUEST, 2);
            }
        } else if (*st).mapping_type as u32 == MAPPING_TYPE_AMBISONICS as i32 as u32 {
            opus_encoder_ctl!(enc, OPUS_SET_FORCE_MODE_REQUEST, 1002);
        }
        s += 1;
    }
    ptr = (st as *mut i8)
        .offset(align(::core::mem::size_of::<OpusMSEncoder>() as u64 as i32) as isize);
    tot_size = 0;
    s = 0;
    while s < (*st).layout.nb_streams {
        let mut enc_0: *mut OpusEncoder = 0 as *mut OpusEncoder;
        let mut len: i32 = 0;
        let mut curr_max: i32 = 0;
        let mut c1: i32 = 0;
        let mut c2: i32 = 0;
        let mut ret: i32 = 0;
        opus_repacketizer_init(&mut rp);
        enc_0 = ptr as *mut OpusEncoder;
        if s < (*st).layout.nb_coupled_streams {
            let mut i: i32 = 0;
            let mut left: i32 = 0;
            let mut right: i32 = 0;
            left = get_left_channel(&mut (*st).layout, s, -1);
            right = get_right_channel(&mut (*st).layout, s, -1);
            (Some(copy_channel_in.expect("non-null function pointer")))
                .expect("non-null function pointer")(
                buf.as_mut_ptr(),
                2,
                pcm,
                (*st).layout.nb_channels,
                left,
                frame_size,
                user_data,
            );
            (Some(copy_channel_in.expect("non-null function pointer")))
                .expect("non-null function pointer")(
                buf.as_mut_ptr().offset(1 as isize),
                2,
                pcm,
                (*st).layout.nb_channels,
                right,
                frame_size,
                user_data,
            );
            ptr = ptr.offset(align(coupled_size) as isize);
            if (*st).mapping_type as u32 == MAPPING_TYPE_SURROUND as i32 as u32 {
                i = 0;
                while i < 21 {
                    bandLogE[i as usize] = *bandSMR.as_mut_ptr().offset((21 * left + i) as isize);
                    bandLogE[(21 + i) as usize] =
                        *bandSMR.as_mut_ptr().offset((21 * right + i) as isize);
                    i += 1;
                }
            }
            c1 = left;
            c2 = right;
        } else {
            let mut i_0: i32 = 0;
            let chan: i32 = get_mono_channel(&mut (*st).layout, s, -1);
            (Some(copy_channel_in.expect("non-null function pointer")))
                .expect("non-null function pointer")(
                buf.as_mut_ptr(),
                1,
                pcm,
                (*st).layout.nb_channels,
                chan,
                frame_size,
                user_data,
            );
            ptr = ptr.offset(align(mono_size) as isize);
            if (*st).mapping_type as u32 == MAPPING_TYPE_SURROUND as i32 as u32 {
                i_0 = 0;
                while i_0 < 21 {
                    bandLogE[i_0 as usize] =
                        *bandSMR.as_mut_ptr().offset((21 * chan + i_0) as isize);
                    i_0 += 1;
                }
            }
            c1 = chan;
            c2 = -1;
        }
        if (*st).mapping_type as u32 == MAPPING_TYPE_SURROUND as i32 as u32 {
            opus_encoder_ctl!(
                enc_0,
                OPUS_SET_ENERGY_MASK_REQUEST,
                bandLogE.as_mut_ptr().offset(
                    bandLogE.as_mut_ptr().offset_from(bandLogE.as_mut_ptr()) as i64 as isize,
                ),
            );
        }
        curr_max = max_data_bytes - tot_size;
        curr_max -= if 0 > 2 * ((*st).layout.nb_streams - s - 1) - 1 {
            0
        } else {
            2 * ((*st).layout.nb_streams - s - 1) - 1
        };
        if Fs / frame_size == 10 {
            curr_max -= (*st).layout.nb_streams - s - 1;
        }
        curr_max = if curr_max < 6 * 1275 + 12 {
            curr_max
        } else {
            6 * 1275 + 12
        };
        if s != (*st).layout.nb_streams - 1 {
            curr_max -= if curr_max > 253 { 2 } else { 1 };
        }
        if vbr == 0 && s == (*st).layout.nb_streams - 1 {
            opus_encoder_ctl!(
                enc_0,
                OPUS_SET_BITRATE_REQUEST,
                curr_max * (8 * Fs / frame_size),
            );
        }
        len = opus_encode_native(
            enc_0,
            buf.as_mut_ptr(),
            frame_size,
            tmp_data.as_mut_ptr(),
            curr_max,
            lsb_depth,
            pcm,
            analysis_frame_size,
            c1,
            c2,
            (*st).layout.nb_channels,
            downmix,
            float_api,
        );
        if len < 0 {
            return len;
        }
        ret = opus_repacketizer_cat(&mut rp, tmp_data.as_mut_ptr(), len);
        if ret != OPUS_OK {
            return OPUS_INTERNAL_ERROR;
        }
        len = opus_repacketizer_out_range_impl(
            &mut rp,
            0,
            opus_repacketizer_get_nb_frames(&mut rp),
            data,
            max_data_bytes - tot_size,
            (s != (*st).layout.nb_streams - 1) as i32,
            (vbr == 0 && s == (*st).layout.nb_streams - 1) as i32,
        );
        data = data.offset(len as isize);
        tot_size += len;
        s += 1;
    }
    return tot_size;
}
unsafe fn opus_copy_channel_in_float(
    dst: *mut opus_val16,
    dst_stride: i32,
    src: *const core::ffi::c_void,
    src_stride: i32,
    src_channel: i32,
    frame_size: i32,
    _user_data: *mut core::ffi::c_void,
) {
    let mut float_src: *const f32 = 0 as *const f32;
    let mut i: i32 = 0;
    float_src = src as *const f32;
    i = 0;
    while i < frame_size {
        *dst.offset((i * dst_stride) as isize) =
            *float_src.offset((i * src_stride + src_channel) as isize);
        i += 1;
    }
}
unsafe fn opus_copy_channel_in_short(
    dst: *mut opus_val16,
    dst_stride: i32,
    src: *const core::ffi::c_void,
    src_stride: i32,
    src_channel: i32,
    frame_size: i32,
    _user_data: *mut core::ffi::c_void,
) {
    let mut short_src: *const i16 = 0 as *const i16;
    let mut i: i32 = 0;
    short_src = src as *const i16;
    i = 0;
    while i < frame_size {
        *dst.offset((i * dst_stride) as isize) = 1 as f32 / 32768.0f32
            * *short_src.offset((i * src_stride + src_channel) as isize) as i32 as f32;
        i += 1;
    }
}
pub unsafe fn opus_multistream_encode_float(
    st: *mut OpusMSEncoder,
    pcm: *const opus_val16,
    frame_size: i32,
    data: *mut u8,
    max_data_bytes: i32,
) -> i32 {
    return opus_multistream_encode_native(
        st,
        Some(
            opus_copy_channel_in_float
                as unsafe fn(
                    *mut opus_val16,
                    i32,
                    *const core::ffi::c_void,
                    i32,
                    i32,
                    i32,
                    *mut core::ffi::c_void,
                ) -> (),
        ),
        pcm as *const core::ffi::c_void,
        frame_size,
        data,
        max_data_bytes,
        24,
        Some(
            downmix_float
                as unsafe fn(
                    *const core::ffi::c_void,
                    *mut opus_val32,
                    i32,
                    i32,
                    i32,
                    i32,
                    i32,
                ) -> (),
        ),
        1,
        NULL as *mut core::ffi::c_void,
    );
}
pub unsafe fn opus_multistream_encode(
    st: *mut OpusMSEncoder,
    pcm: *const i16,
    frame_size: i32,
    data: *mut u8,
    max_data_bytes: i32,
) -> i32 {
    return opus_multistream_encode_native(
        st,
        Some(
            opus_copy_channel_in_short
                as unsafe fn(
                    *mut opus_val16,
                    i32,
                    *const core::ffi::c_void,
                    i32,
                    i32,
                    i32,
                    *mut core::ffi::c_void,
                ) -> (),
        ),
        pcm as *const core::ffi::c_void,
        frame_size,
        data,
        max_data_bytes,
        16,
        Some(
            downmix_int
                as unsafe fn(
                    *const core::ffi::c_void,
                    *mut opus_val32,
                    i32,
                    i32,
                    i32,
                    i32,
                    i32,
                ) -> (),
        ),
        0,
        NULL as *mut core::ffi::c_void,
    );
}
pub unsafe fn opus_multistream_encoder_ctl_va_list(
    st: *mut OpusMSEncoder,
    request: i32,
    mut ap: VarArgs,
) -> i32 {
    let mut current_block: u64;
    let mut coupled_size: i32 = 0;
    let mut mono_size: i32 = 0;
    let mut ptr: *mut i8 = 0 as *mut i8;
    let mut ret: i32 = OPUS_OK;
    coupled_size = opus_encoder_get_size(2);
    mono_size = opus_encoder_get_size(1);
    ptr = (st as *mut i8)
        .offset(align(::core::mem::size_of::<OpusMSEncoder>() as u64 as i32) as isize);
    match request {
        OPUS_SET_BITRATE_REQUEST => {
            let mut value: i32 = ap.arg::<i32>();
            if value != OPUS_AUTO && value != OPUS_BITRATE_MAX {
                if value <= 0 {
                    current_block = 11382675479785311092;
                } else {
                    value = if 300000 * (*st).layout.nb_channels
                        < (if 500 * (*st).layout.nb_channels > value {
                            500 * (*st).layout.nb_channels
                        } else {
                            value
                        }) {
                        300000 * (*st).layout.nb_channels
                    } else if 500 * (*st).layout.nb_channels > value {
                        500 * (*st).layout.nb_channels
                    } else {
                        value
                    };
                    current_block = 10879442775620481940;
                }
            } else {
                current_block = 10879442775620481940;
            }
            match current_block {
                11382675479785311092 => {}
                _ => {
                    (*st).bitrate_bps = value;
                    current_block = 2616667235040759262;
                }
            }
        }
        OPUS_GET_BITRATE_REQUEST => {
            let mut s: i32 = 0;
            let value_0 = ap.arg::<&mut i32>();
            *value_0 = 0;
            s = 0;
            while s < (*st).layout.nb_streams {
                let mut rate: i32 = 0;
                let mut enc: *mut OpusEncoder = 0 as *mut OpusEncoder;
                enc = ptr as *mut OpusEncoder;
                if s < (*st).layout.nb_coupled_streams {
                    ptr = ptr.offset(align(coupled_size) as isize);
                } else {
                    ptr = ptr.offset(align(mono_size) as isize);
                }
                opus_encoder_ctl!(enc, request, &mut rate);
                *value_0 += rate;
                s += 1;
            }
            current_block = 2616667235040759262;
        }
        OPUS_GET_LSB_DEPTH_REQUEST
        | OPUS_GET_VBR_REQUEST
        | OPUS_GET_APPLICATION_REQUEST
        | OPUS_GET_BANDWIDTH_REQUEST
        | OPUS_GET_COMPLEXITY_REQUEST
        | OPUS_GET_PACKET_LOSS_PERC_REQUEST
        | OPUS_GET_DTX_REQUEST
        | OPUS_GET_VOICE_RATIO_REQUEST
        | OPUS_GET_VBR_CONSTRAINT_REQUEST
        | OPUS_GET_SIGNAL_REQUEST
        | OPUS_GET_LOOKAHEAD_REQUEST
        | OPUS_GET_SAMPLE_RATE_REQUEST
        | OPUS_GET_INBAND_FEC_REQUEST
        | OPUS_GET_FORCE_CHANNELS_REQUEST
        | OPUS_GET_PREDICTION_DISABLED_REQUEST
        | OPUS_GET_PHASE_INVERSION_DISABLED_REQUEST => {
            let mut enc_0: *mut OpusEncoder = 0 as *mut OpusEncoder;
            let value_1 = ap.arg::<&mut i32>();
            enc_0 = ptr as *mut OpusEncoder;
            ret = opus_encoder_ctl!(enc_0, request, value_1);
            current_block = 2616667235040759262;
        }
        OPUS_GET_FINAL_RANGE_REQUEST => {
            let mut s_0: i32 = 0;
            let value_2 = ap.arg::<&mut u32>();
            let mut tmp: u32 = 0;
            *value_2 = 0;
            s_0 = 0;
            while s_0 < (*st).layout.nb_streams {
                let mut enc_1: *mut OpusEncoder = 0 as *mut OpusEncoder;
                enc_1 = ptr as *mut OpusEncoder;
                if s_0 < (*st).layout.nb_coupled_streams {
                    ptr = ptr.offset(align(coupled_size) as isize);
                } else {
                    ptr = ptr.offset(align(mono_size) as isize);
                }
                ret = opus_encoder_ctl!(enc_1, request, &mut tmp);
                if ret != OPUS_OK {
                    break;
                }
                *value_2 ^= tmp;
                s_0 += 1;
            }
            current_block = 2616667235040759262;
        }
        OPUS_SET_LSB_DEPTH_REQUEST
        | OPUS_SET_COMPLEXITY_REQUEST
        | OPUS_SET_VBR_REQUEST
        | OPUS_SET_VBR_CONSTRAINT_REQUEST
        | OPUS_SET_MAX_BANDWIDTH_REQUEST
        | OPUS_SET_BANDWIDTH_REQUEST
        | OPUS_SET_SIGNAL_REQUEST
        | OPUS_SET_APPLICATION_REQUEST
        | OPUS_SET_INBAND_FEC_REQUEST
        | OPUS_SET_PACKET_LOSS_PERC_REQUEST
        | OPUS_SET_DTX_REQUEST
        | OPUS_SET_FORCE_MODE_REQUEST
        | OPUS_SET_FORCE_CHANNELS_REQUEST
        | OPUS_SET_PREDICTION_DISABLED_REQUEST
        | OPUS_SET_PHASE_INVERSION_DISABLED_REQUEST => {
            let mut s_1: i32 = 0;
            let value_3: i32 = ap.arg::<i32>();
            s_1 = 0;
            while s_1 < (*st).layout.nb_streams {
                let mut enc_2: *mut OpusEncoder = 0 as *mut OpusEncoder;
                enc_2 = ptr as *mut OpusEncoder;
                if s_1 < (*st).layout.nb_coupled_streams {
                    ptr = ptr.offset(align(coupled_size) as isize);
                } else {
                    ptr = ptr.offset(align(mono_size) as isize);
                }
                ret = opus_encoder_ctl!(enc_2, request, value_3);
                if ret != OPUS_OK {
                    break;
                }
                s_1 += 1;
            }
            current_block = 2616667235040759262;
        }
        OPUS_MULTISTREAM_GET_ENCODER_STATE_REQUEST => {
            let mut s_2: i32 = 0;
            let mut stream_id: i32 = 0;
            let mut value_4: *mut *mut OpusEncoder = 0 as *mut *mut OpusEncoder;
            stream_id = ap.arg::<i32>();
            if stream_id < 0 || stream_id >= (*st).layout.nb_streams {
                current_block = 11382675479785311092;
            } else {
                value_4 = ap.arg::<&mut *mut OpusEncoder>();
                s_2 = 0;
                while s_2 < stream_id {
                    if s_2 < (*st).layout.nb_coupled_streams {
                        ptr = ptr.offset(align(coupled_size) as isize);
                    } else {
                        ptr = ptr.offset(align(mono_size) as isize);
                    }
                    s_2 += 1;
                }
                *value_4 = ptr as *mut OpusEncoder;
                current_block = 2616667235040759262;
            }
        }
        OPUS_SET_EXPERT_FRAME_DURATION_REQUEST => {
            let value_5: i32 = ap.arg::<i32>();
            (*st).variable_duration = value_5;
            current_block = 2616667235040759262;
        }
        OPUS_GET_EXPERT_FRAME_DURATION_REQUEST => {
            let value_6 = ap.arg::<&mut i32>();
            *value_6 = (*st).variable_duration;
            current_block = 2616667235040759262;
        }
        OPUS_RESET_STATE => {
            let mut s_3: i32 = 0;
            if (*st).mapping_type as u32 == MAPPING_TYPE_SURROUND as i32 as u32 {
                memset(
                    ms_get_preemph_mem(st) as *mut core::ffi::c_void,
                    0,
                    ((*st).layout.nb_channels as u64)
                        .wrapping_mul(::core::mem::size_of::<opus_val32>() as u64),
                );
                memset(
                    ms_get_window_mem(st) as *mut core::ffi::c_void,
                    0,
                    (((*st).layout.nb_channels * 120) as u64)
                        .wrapping_mul(::core::mem::size_of::<opus_val32>() as u64),
                );
            }
            s_3 = 0;
            while s_3 < (*st).layout.nb_streams {
                let mut enc_3: *mut OpusEncoder = 0 as *mut OpusEncoder;
                enc_3 = ptr as *mut OpusEncoder;
                if s_3 < (*st).layout.nb_coupled_streams {
                    ptr = ptr.offset(align(coupled_size) as isize);
                } else {
                    ptr = ptr.offset(align(mono_size) as isize);
                }
                ret = opus_encoder_ctl!(enc_3, OPUS_RESET_STATE);
                if ret != OPUS_OK {
                    break;
                }
                s_3 += 1;
            }
            current_block = 2616667235040759262;
        }
        _ => {
            ret = OPUS_UNIMPLEMENTED;
            current_block = 2616667235040759262;
        }
    }
    match current_block {
        2616667235040759262 => return ret,
        _ => return OPUS_BAD_ARG,
    };
}
pub unsafe fn opus_multistream_encoder_ctl_impl(
    st: *mut OpusMSEncoder,
    request: i32,
    args: VarArgs,
) -> i32 {
    let mut ret: i32 = 0;
    ret = opus_multistream_encoder_ctl_va_list(st, request, args);
    return ret;
}
#[macro_export]
macro_rules! opus_multistream_encoder_ctl {
    ($st:expr, $request:expr, $($arg:expr),*) => {
        $crate::opus_multistream_encoder_ctl_impl($st, $request, $crate::varargs!($($arg),*))
    };
    ($st:expr, $request:expr) => {
        opus_multistream_encoder_ctl!($st, $request,)
    };
    ($st:expr, $request:expr, $($arg:expr),*,) => {
        opus_multistream_encoder_ctl!($st, $request, $($arg),*)
    };
}
pub unsafe fn opus_multistream_encoder_destroy(st: *mut OpusMSEncoder) {
    free(st as *mut core::ffi::c_void);
}
