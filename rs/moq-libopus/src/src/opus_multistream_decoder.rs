use crate::externs::{free, malloc};

#[derive(Copy, Clone)]
#[repr(C)]
pub struct OpusMSDecoder {
    pub layout: ChannelLayout,
}

pub type opus_copy_channel_out_func = Option<
    unsafe fn(
        *mut core::ffi::c_void,
        i32,
        i32,
        *const opus_val16,
        i32,
        i32,
        *mut core::ffi::c_void,
    ) -> (),
>;

pub mod arch_h {
    pub type opus_val16 = f32;
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
use self::arch_h::opus_val16;
pub use self::stddef_h::{size_t, NULL};
use crate::celt::float_cast::FLOAT2INT16;
use crate::src::opus::opus_packet_parse_impl;
use crate::src::opus_decoder::opus_decode_native;
use crate::src::opus_defines::{
    OPUS_ALLOC_FAIL, OPUS_BAD_ARG, OPUS_BUFFER_TOO_SMALL, OPUS_GET_BANDWIDTH_REQUEST,
    OPUS_GET_FINAL_RANGE_REQUEST, OPUS_GET_GAIN_REQUEST, OPUS_GET_LAST_PACKET_DURATION_REQUEST,
    OPUS_GET_PHASE_INVERSION_DISABLED_REQUEST, OPUS_GET_SAMPLE_RATE_REQUEST, OPUS_INTERNAL_ERROR,
    OPUS_INVALID_PACKET, OPUS_OK, OPUS_RESET_STATE, OPUS_SET_GAIN_REQUEST,
    OPUS_SET_PHASE_INVERSION_DISABLED_REQUEST, OPUS_UNIMPLEMENTED,
};
use crate::src::opus_multistream::{
    get_left_channel, get_mono_channel, get_right_channel, validate_layout, ChannelLayout,
};
use crate::src::opus_private::align;
use crate::varargs::VarArgs;
use crate::{
    opus_decoder_ctl, opus_decoder_get_size, opus_decoder_init, opus_multistream_decoder_ctl,
    opus_packet_get_nb_samples, OpusDecoder,
};

pub const OPUS_MULTISTREAM_GET_DECODER_STATE_REQUEST: i32 = 5122;

unsafe fn validate_ms_decoder(st: *mut OpusMSDecoder) {
    validate_layout(&mut (*st).layout);
}
pub unsafe fn opus_multistream_decoder_get_size(nb_streams: i32, nb_coupled_streams: i32) -> i32 {
    let mut coupled_size: i32 = 0;
    let mut mono_size: i32 = 0;
    if nb_streams < 1 || nb_coupled_streams > nb_streams || nb_coupled_streams < 0 {
        return 0;
    }
    coupled_size = opus_decoder_get_size(2);
    mono_size = opus_decoder_get_size(1);
    return align(::core::mem::size_of::<OpusMSDecoder>() as u64 as i32)
        + nb_coupled_streams * align(coupled_size)
        + (nb_streams - nb_coupled_streams) * align(mono_size);
}
pub unsafe fn opus_multistream_decoder_init(
    st: *mut OpusMSDecoder,
    Fs: i32,
    channels: i32,
    streams: i32,
    coupled_streams: i32,
    mapping: *const u8,
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
    (*st).layout.nb_channels = channels;
    (*st).layout.nb_streams = streams;
    (*st).layout.nb_coupled_streams = coupled_streams;
    i = 0;
    while i < (*st).layout.nb_channels {
        (*st).layout.mapping[i as usize] = *mapping.offset(i as isize);
        i += 1;
    }
    if validate_layout(&mut (*st).layout) == 0 {
        return OPUS_BAD_ARG;
    }
    ptr = (st as *mut i8)
        .offset(align(::core::mem::size_of::<OpusMSDecoder>() as u64 as i32) as isize);
    coupled_size = opus_decoder_get_size(2);
    mono_size = opus_decoder_get_size(1);
    i = 0;
    while i < (*st).layout.nb_coupled_streams {
        ret = opus_decoder_init(ptr as *mut OpusDecoder, Fs, 2);
        if ret != OPUS_OK {
            return ret;
        }
        ptr = ptr.offset(align(coupled_size) as isize);
        i += 1;
    }
    while i < (*st).layout.nb_streams {
        ret = opus_decoder_init(ptr as *mut OpusDecoder, Fs, 1);
        if ret != OPUS_OK {
            return ret;
        }
        ptr = ptr.offset(align(mono_size) as isize);
        i += 1;
    }
    return OPUS_OK;
}
pub unsafe fn opus_multistream_decoder_create(
    Fs: i32,
    channels: i32,
    streams: i32,
    coupled_streams: i32,
    mapping: *const u8,
    error: *mut i32,
) -> *mut OpusMSDecoder {
    let mut ret: i32 = 0;
    let mut st: *mut OpusMSDecoder = 0 as *mut OpusMSDecoder;
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
        return NULL as *mut OpusMSDecoder;
    }
    st = malloc(opus_multistream_decoder_get_size(streams, coupled_streams) as size_t)
        as *mut OpusMSDecoder;
    if st.is_null() {
        if !error.is_null() {
            *error = OPUS_ALLOC_FAIL;
        }
        return NULL as *mut OpusMSDecoder;
    }
    ret = opus_multistream_decoder_init(st, Fs, channels, streams, coupled_streams, mapping);
    if !error.is_null() {
        *error = ret;
    }
    if ret != OPUS_OK {
        free(st as *mut core::ffi::c_void);
        st = NULL as *mut OpusMSDecoder;
    }
    return st;
}
unsafe fn opus_multistream_packet_validate(
    mut data: *const u8,
    mut len: i32,
    nb_streams: i32,
    Fs: i32,
) -> i32 {
    let mut s: i32 = 0;
    let mut count: i32 = 0;
    let mut toc: u8 = 0;
    let mut size: [i16; 48] = [0; 48];
    let mut samples: i32 = 0;
    let mut packet_offset: i32 = 0;
    s = 0;
    while s < nb_streams {
        let mut tmp_samples: i32 = 0;
        if len <= 0 {
            return OPUS_INVALID_PACKET;
        }
        count = opus_packet_parse_impl(
            data,
            len,
            (s != nb_streams - 1) as i32,
            &mut toc,
            NULL as *mut *const u8,
            size.as_mut_ptr(),
            NULL as *mut i32,
            &mut packet_offset,
        );
        if count < 0 {
            return count;
        }
        tmp_samples = opus_packet_get_nb_samples(data, packet_offset, Fs);
        if s != 0 && samples != tmp_samples {
            return OPUS_INVALID_PACKET;
        }
        samples = tmp_samples;
        data = data.offset(packet_offset as isize);
        len -= packet_offset;
        s += 1;
    }
    return samples;
}
pub unsafe fn opus_multistream_decode_native(
    st: *mut OpusMSDecoder,
    mut data: *const u8,
    mut len: i32,
    pcm: *mut core::ffi::c_void,
    copy_channel_out: opus_copy_channel_out_func,
    mut frame_size: i32,
    decode_fec: i32,
    soft_clip: i32,
    user_data: *mut core::ffi::c_void,
) -> i32 {
    let mut Fs: i32 = 0;
    let mut coupled_size: i32 = 0;
    let mut mono_size: i32 = 0;
    let mut s: i32 = 0;
    let mut c: i32 = 0;
    let mut ptr: *mut i8 = 0 as *mut i8;
    let mut do_plc: i32 = 0;
    validate_ms_decoder(st);
    if frame_size <= 0 {
        return OPUS_BAD_ARG;
    }
    assert!(opus_multistream_decoder_ctl!(st, 4029, &mut Fs) == 0);
    frame_size = if frame_size < Fs / 25 * 3 {
        frame_size
    } else {
        Fs / 25 * 3
    };
    let vla = (2 * frame_size) as usize;
    let mut buf: Vec<opus_val16> = ::std::vec::from_elem(0., vla);
    ptr = (st as *mut i8)
        .offset(align(::core::mem::size_of::<OpusMSDecoder>() as u64 as i32) as isize);
    coupled_size = opus_decoder_get_size(2);
    mono_size = opus_decoder_get_size(1);
    if len == 0 {
        do_plc = 1;
    }
    if len < 0 {
        return OPUS_BAD_ARG;
    }
    if do_plc == 0 && len < 2 * (*st).layout.nb_streams - 1 {
        return OPUS_INVALID_PACKET;
    }
    if do_plc == 0 {
        let ret: i32 = opus_multistream_packet_validate(data, len, (*st).layout.nb_streams, Fs);
        if ret < 0 {
            return ret;
        } else {
            if ret > frame_size {
                return OPUS_BUFFER_TOO_SMALL;
            }
        }
    }
    s = 0;
    while s < (*st).layout.nb_streams {
        let mut dec: *mut OpusDecoder = 0 as *mut OpusDecoder;
        let mut packet_offset: i32 = 0;
        let mut ret_0: i32 = 0;
        dec = ptr as *mut OpusDecoder;
        ptr = ptr.offset(
            (if s < (*st).layout.nb_coupled_streams {
                align(coupled_size)
            } else {
                align(mono_size)
            }) as isize,
        );
        if do_plc == 0 && len <= 0 {
            return OPUS_INTERNAL_ERROR;
        }
        packet_offset = 0;
        ret_0 = opus_decode_native(
            dec,
            data,
            len,
            buf.as_mut_ptr(),
            frame_size,
            decode_fec,
            (s != (*st).layout.nb_streams - 1) as i32,
            &mut packet_offset,
            soft_clip,
        );
        data = data.offset(packet_offset as isize);
        len -= packet_offset;
        if ret_0 <= 0 {
            return ret_0;
        }
        frame_size = ret_0;
        if s < (*st).layout.nb_coupled_streams {
            let mut chan: i32 = 0;
            let mut prev: i32 = 0;
            prev = -1;
            loop {
                chan = get_left_channel(&mut (*st).layout, s, prev);
                if !(chan != -1) {
                    break;
                }
                (Some(copy_channel_out.expect("non-null function pointer")))
                    .expect("non-null function pointer")(
                    pcm,
                    (*st).layout.nb_channels,
                    chan,
                    buf.as_mut_ptr(),
                    2,
                    frame_size,
                    user_data,
                );
                prev = chan;
            }
            prev = -1;
            loop {
                chan = get_right_channel(&mut (*st).layout, s, prev);
                if !(chan != -1) {
                    break;
                }
                (Some(copy_channel_out.expect("non-null function pointer")))
                    .expect("non-null function pointer")(
                    pcm,
                    (*st).layout.nb_channels,
                    chan,
                    buf.as_mut_ptr().offset(1 as isize),
                    2,
                    frame_size,
                    user_data,
                );
                prev = chan;
            }
        } else {
            let mut chan_0: i32 = 0;
            let mut prev_0: i32 = 0;
            prev_0 = -1;
            loop {
                chan_0 = get_mono_channel(&mut (*st).layout, s, prev_0);
                if !(chan_0 != -1) {
                    break;
                }
                (Some(copy_channel_out.expect("non-null function pointer")))
                    .expect("non-null function pointer")(
                    pcm,
                    (*st).layout.nb_channels,
                    chan_0,
                    buf.as_mut_ptr(),
                    1,
                    frame_size,
                    user_data,
                );
                prev_0 = chan_0;
            }
        }
        s += 1;
    }
    c = 0;
    while c < (*st).layout.nb_channels {
        if (*st).layout.mapping[c as usize] as i32 == 255 {
            (Some(copy_channel_out.expect("non-null function pointer")))
                .expect("non-null function pointer")(
                pcm,
                (*st).layout.nb_channels,
                c,
                NULL as *const opus_val16,
                0,
                frame_size,
                user_data,
            );
        }
        c += 1;
    }
    return frame_size;
}
unsafe fn opus_copy_channel_out_float(
    dst: *mut core::ffi::c_void,
    dst_stride: i32,
    dst_channel: i32,
    src: *const opus_val16,
    src_stride: i32,
    frame_size: i32,
    _user_data: *mut core::ffi::c_void,
) {
    let mut float_dst: *mut f32 = 0 as *mut f32;
    let mut i: i32 = 0;
    float_dst = dst as *mut f32;
    if !src.is_null() {
        i = 0;
        while i < frame_size {
            *float_dst.offset((i * dst_stride + dst_channel) as isize) =
                *src.offset((i * src_stride) as isize);
            i += 1;
        }
    } else {
        i = 0;
        while i < frame_size {
            *float_dst.offset((i * dst_stride + dst_channel) as isize) = 0 as f32;
            i += 1;
        }
    };
}
unsafe fn opus_copy_channel_out_short(
    dst: *mut core::ffi::c_void,
    dst_stride: i32,
    dst_channel: i32,
    src: *const opus_val16,
    src_stride: i32,
    frame_size: i32,
    _user_data: *mut core::ffi::c_void,
) {
    let mut short_dst: *mut i16 = 0 as *mut i16;
    let mut i: i32 = 0;
    short_dst = dst as *mut i16;
    if !src.is_null() {
        i = 0;
        while i < frame_size {
            *short_dst.offset((i * dst_stride + dst_channel) as isize) =
                FLOAT2INT16(*src.offset((i * src_stride) as isize));
            i += 1;
        }
    } else {
        i = 0;
        while i < frame_size {
            *short_dst.offset((i * dst_stride + dst_channel) as isize) = 0;
            i += 1;
        }
    };
}
pub unsafe fn opus_multistream_decode(
    st: *mut OpusMSDecoder,
    data: *const u8,
    len: i32,
    pcm: *mut i16,
    frame_size: i32,
    decode_fec: i32,
) -> i32 {
    return opus_multistream_decode_native(
        st,
        data,
        len,
        pcm as *mut core::ffi::c_void,
        Some(
            opus_copy_channel_out_short
                as unsafe fn(
                    *mut core::ffi::c_void,
                    i32,
                    i32,
                    *const opus_val16,
                    i32,
                    i32,
                    *mut core::ffi::c_void,
                ) -> (),
        ),
        frame_size,
        decode_fec,
        1,
        NULL as *mut core::ffi::c_void,
    );
}
pub unsafe fn opus_multistream_decode_float(
    st: *mut OpusMSDecoder,
    data: *const u8,
    len: i32,
    pcm: *mut opus_val16,
    frame_size: i32,
    decode_fec: i32,
) -> i32 {
    return opus_multistream_decode_native(
        st,
        data,
        len,
        pcm as *mut core::ffi::c_void,
        Some(
            opus_copy_channel_out_float
                as unsafe fn(
                    *mut core::ffi::c_void,
                    i32,
                    i32,
                    *const opus_val16,
                    i32,
                    i32,
                    *mut core::ffi::c_void,
                ) -> (),
        ),
        frame_size,
        decode_fec,
        0,
        NULL as *mut core::ffi::c_void,
    );
}
pub unsafe fn opus_multistream_decoder_ctl_va_list(
    st: *mut OpusMSDecoder,
    request: i32,
    mut ap: VarArgs,
) -> i32 {
    let current_block: u64;
    let mut coupled_size: i32 = 0;
    let mut mono_size: i32 = 0;
    let mut ptr: *mut i8 = 0 as *mut i8;
    let mut ret: i32 = OPUS_OK;
    coupled_size = opus_decoder_get_size(2);
    mono_size = opus_decoder_get_size(1);
    ptr = (st as *mut i8)
        .offset(align(::core::mem::size_of::<OpusMSDecoder>() as u64 as i32) as isize);
    match request {
        OPUS_GET_BANDWIDTH_REQUEST
        | OPUS_GET_SAMPLE_RATE_REQUEST
        | OPUS_GET_GAIN_REQUEST
        | OPUS_GET_LAST_PACKET_DURATION_REQUEST
        | OPUS_GET_PHASE_INVERSION_DISABLED_REQUEST => {
            let mut dec: *mut OpusDecoder = 0 as *mut OpusDecoder;
            let value: &mut i32 = ap.arg::<&mut i32>();
            dec = ptr as *mut OpusDecoder;
            ret = opus_decoder_ctl!(dec, request, value);
            current_block = 7343950298149844727;
        }
        OPUS_GET_FINAL_RANGE_REQUEST => {
            let mut s: i32 = 0;
            let value_0 = ap.arg::<&mut u32>();
            let mut tmp: u32 = 0;
            *value_0 = 0;
            s = 0;
            while s < (*st).layout.nb_streams {
                let mut dec_0: *mut OpusDecoder = 0 as *mut OpusDecoder;
                dec_0 = ptr as *mut OpusDecoder;
                if s < (*st).layout.nb_coupled_streams {
                    ptr = ptr.offset(align(coupled_size) as isize);
                } else {
                    ptr = ptr.offset(align(mono_size) as isize);
                }
                ret = opus_decoder_ctl!(dec_0, request, &mut tmp);
                if ret != OPUS_OK {
                    break;
                }
                *value_0 ^= tmp;
                s += 1;
            }
            current_block = 7343950298149844727;
        }
        OPUS_RESET_STATE => {
            let mut s_0: i32 = 0;
            s_0 = 0;
            while s_0 < (*st).layout.nb_streams {
                let mut dec_1: *mut OpusDecoder = 0 as *mut OpusDecoder;
                dec_1 = ptr as *mut OpusDecoder;
                if s_0 < (*st).layout.nb_coupled_streams {
                    ptr = ptr.offset(align(coupled_size) as isize);
                } else {
                    ptr = ptr.offset(align(mono_size) as isize);
                }
                ret = opus_decoder_ctl!(dec_1, OPUS_RESET_STATE);
                if ret != OPUS_OK {
                    break;
                }
                s_0 += 1;
            }
            current_block = 7343950298149844727;
        }
        OPUS_MULTISTREAM_GET_DECODER_STATE_REQUEST => {
            let mut s_1: i32 = 0;
            let mut stream_id: i32 = 0;
            let mut value_1: *mut *mut OpusDecoder = 0 as *mut *mut OpusDecoder;
            stream_id = ap.arg::<i32>();
            if stream_id < 0 || stream_id >= (*st).layout.nb_streams {
                current_block = 15933654591644784431;
            } else {
                value_1 = ap.arg::<&mut *mut OpusDecoder>();
                s_1 = 0;
                while s_1 < stream_id {
                    if s_1 < (*st).layout.nb_coupled_streams {
                        ptr = ptr.offset(align(coupled_size) as isize);
                    } else {
                        ptr = ptr.offset(align(mono_size) as isize);
                    }
                    s_1 += 1;
                }
                *value_1 = ptr as *mut OpusDecoder;
                current_block = 7343950298149844727;
            }
        }
        OPUS_SET_GAIN_REQUEST | OPUS_SET_PHASE_INVERSION_DISABLED_REQUEST => {
            let mut s_2: i32 = 0;
            let value_2: i32 = ap.arg::<i32>();
            s_2 = 0;
            while s_2 < (*st).layout.nb_streams {
                let mut dec_2: *mut OpusDecoder = 0 as *mut OpusDecoder;
                dec_2 = ptr as *mut OpusDecoder;
                if s_2 < (*st).layout.nb_coupled_streams {
                    ptr = ptr.offset(align(coupled_size) as isize);
                } else {
                    ptr = ptr.offset(align(mono_size) as isize);
                }
                ret = opus_decoder_ctl!(dec_2, request, value_2);
                if ret != OPUS_OK {
                    break;
                }
                s_2 += 1;
            }
            current_block = 7343950298149844727;
        }
        _ => {
            ret = OPUS_UNIMPLEMENTED;
            current_block = 7343950298149844727;
        }
    }
    match current_block {
        15933654591644784431 => return OPUS_BAD_ARG,
        _ => return ret,
    };
}
pub unsafe fn opus_multistream_decoder_ctl_impl(
    st: *mut OpusMSDecoder,
    request: i32,
    args: VarArgs,
) -> i32 {
    let mut ret: i32 = 0;
    ret = opus_multistream_decoder_ctl_va_list(st, request, args);
    return ret;
}
#[macro_export]
macro_rules! opus_multistream_decoder_ctl {
    ($st:expr, $request:expr, $($arg:expr),*) => {
        $crate::opus_multistream_decoder_ctl_impl($st, $request, $crate::varargs!($($arg),*))
    };
    ($st:expr, $request:expr) => {
        opus_multistream_decoder_ctl!($st, $request,)
    };
    ($st:expr, $request:expr, $($arg:expr),*,) => {
        opus_multistream_decoder_ctl!($st, $request, $($arg),*)
    };
}
pub unsafe fn opus_multistream_decoder_destroy(st: *mut OpusMSDecoder) {
    free(st as *mut core::ffi::c_void);
}
