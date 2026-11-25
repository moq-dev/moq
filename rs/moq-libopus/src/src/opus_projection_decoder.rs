use crate::externs::{free, malloc};

pub mod arch_h {
    pub type opus_val16 = f32;
}
pub mod stddef_h {
    pub type size_t = u64;
    pub const NULL: i32 = 0;
}
pub use self::arch_h::opus_val16;
pub use self::stddef_h::{size_t, NULL};
use crate::src::opus_defines::{OPUS_ALLOC_FAIL, OPUS_BAD_ARG, OPUS_OK};

use crate::externs::memset;
use crate::src::mapping_matrix::{
    mapping_matrix_get_size, mapping_matrix_init, mapping_matrix_multiply_channel_out_float,
    mapping_matrix_multiply_channel_out_short, MappingMatrix,
};
use crate::src::opus_multistream_decoder::{
    opus_multistream_decode_native, opus_multistream_decoder_ctl_va_list,
};
use crate::src::opus_private::align;
use crate::varargs::VarArgs;
use crate::{opus_multistream_decoder_get_size, opus_multistream_decoder_init, OpusMSDecoder};

#[derive(Copy, Clone)]
#[repr(C)]
pub struct OpusProjectionDecoder {
    pub demixing_matrix_size_in_bytes: i32,
}
unsafe fn opus_projection_copy_channel_out_float(
    dst: *mut core::ffi::c_void,
    dst_stride: i32,
    dst_channel: i32,
    src: *const opus_val16,
    src_stride: i32,
    frame_size: i32,
    user_data: *mut core::ffi::c_void,
) {
    let mut float_dst: *mut f32 = 0 as *mut f32;
    let mut matrix: *const MappingMatrix = 0 as *const MappingMatrix;
    float_dst = dst as *mut f32;
    matrix = user_data as *const MappingMatrix;
    if dst_channel == 0 {
        memset(
            float_dst as *mut core::ffi::c_void,
            0,
            ((frame_size * dst_stride) as u64).wrapping_mul(::core::mem::size_of::<f32>() as u64),
        );
    }
    if !src.is_null() {
        mapping_matrix_multiply_channel_out_float(
            matrix,
            src,
            dst_channel,
            src_stride,
            float_dst,
            dst_stride,
            frame_size,
        );
    }
}
unsafe fn opus_projection_copy_channel_out_short(
    dst: *mut core::ffi::c_void,
    dst_stride: i32,
    dst_channel: i32,
    src: *const opus_val16,
    src_stride: i32,
    frame_size: i32,
    user_data: *mut core::ffi::c_void,
) {
    let mut short_dst: *mut i16 = 0 as *mut i16;
    let mut matrix: *const MappingMatrix = 0 as *const MappingMatrix;
    short_dst = dst as *mut i16;
    matrix = user_data as *const MappingMatrix;
    if dst_channel == 0 {
        memset(
            short_dst as *mut core::ffi::c_void,
            0,
            ((frame_size * dst_stride) as u64).wrapping_mul(::core::mem::size_of::<i16>() as u64),
        );
    }
    if !src.is_null() {
        mapping_matrix_multiply_channel_out_short(
            matrix,
            src,
            dst_channel,
            src_stride,
            short_dst,
            dst_stride,
            frame_size,
        );
    }
}
unsafe fn get_dec_demixing_matrix(st: *mut OpusProjectionDecoder) -> *mut MappingMatrix {
    return (st as *mut i8)
        .offset(align(::core::mem::size_of::<OpusProjectionDecoder>() as u64 as i32) as isize)
        as *mut core::ffi::c_void as *mut MappingMatrix;
}
unsafe fn get_multistream_decoder(st: *mut OpusProjectionDecoder) -> *mut OpusMSDecoder {
    return (st as *mut i8).offset(align(
        (::core::mem::size_of::<OpusProjectionDecoder>() as u64)
            .wrapping_add((*st).demixing_matrix_size_in_bytes as u64) as i32,
    ) as isize) as *mut core::ffi::c_void as *mut OpusMSDecoder;
}
pub unsafe fn opus_projection_decoder_get_size(
    channels: i32,
    streams: i32,
    coupled_streams: i32,
) -> i32 {
    let mut matrix_size: i32 = 0;
    let mut decoder_size: i32 = 0;
    matrix_size = mapping_matrix_get_size(streams + coupled_streams, channels);
    if matrix_size == 0 {
        return 0;
    }
    decoder_size = opus_multistream_decoder_get_size(streams, coupled_streams);
    if decoder_size == 0 {
        return 0;
    }
    return align(::core::mem::size_of::<OpusProjectionDecoder>() as u64 as i32)
        + matrix_size
        + decoder_size;
}
pub unsafe fn opus_projection_decoder_init(
    st: *mut OpusProjectionDecoder,
    Fs: i32,
    channels: i32,
    streams: i32,
    coupled_streams: i32,
    demixing_matrix: *mut u8,
    demixing_matrix_size: i32,
) -> i32 {
    let mut nb_input_streams: i32 = 0;
    let mut expected_matrix_size: i32 = 0;
    let mut i: i32 = 0;
    let mut ret: i32 = 0;
    let mut mapping: [u8; 255] = [0; 255];
    nb_input_streams = streams + coupled_streams;
    expected_matrix_size = ((nb_input_streams * channels) as u64)
        .wrapping_mul(::core::mem::size_of::<i16>() as u64) as i32;
    if expected_matrix_size != demixing_matrix_size {
        return OPUS_BAD_ARG;
    }
    let vla = (nb_input_streams * channels) as usize;
    let mut buf: Vec<i16> = ::std::vec::from_elem(0, vla);
    i = 0;
    while i < nb_input_streams * channels {
        let mut s: i32 = (*demixing_matrix.offset((2 * i + 1) as isize) as i32) << 8
            | *demixing_matrix.offset((2 * i) as isize) as i32;
        s = (s & 0xffff ^ 0x8000) - 0x8000;
        *buf.as_mut_ptr().offset(i as isize) = s as i16;
        i += 1;
    }
    (*st).demixing_matrix_size_in_bytes = mapping_matrix_get_size(channels, nb_input_streams);
    if (*st).demixing_matrix_size_in_bytes == 0 {
        return OPUS_BAD_ARG;
    }
    mapping_matrix_init(
        get_dec_demixing_matrix(st),
        channels,
        nb_input_streams,
        0,
        buf.as_mut_ptr(),
        demixing_matrix_size,
    );
    i = 0;
    while i < channels {
        mapping[i as usize] = i as u8;
        i += 1;
    }
    ret = opus_multistream_decoder_init(
        get_multistream_decoder(st),
        Fs,
        channels,
        streams,
        coupled_streams,
        mapping.as_mut_ptr(),
    );
    return ret;
}
pub unsafe fn opus_projection_decoder_create(
    Fs: i32,
    channels: i32,
    streams: i32,
    coupled_streams: i32,
    demixing_matrix: *mut u8,
    demixing_matrix_size: i32,
    error: *mut i32,
) -> *mut OpusProjectionDecoder {
    let mut size: i32 = 0;
    let mut ret: i32 = 0;
    let mut st: *mut OpusProjectionDecoder = 0 as *mut OpusProjectionDecoder;
    size = opus_projection_decoder_get_size(channels, streams, coupled_streams);
    if size == 0 {
        if !error.is_null() {
            *error = OPUS_ALLOC_FAIL;
        }
        return NULL as *mut OpusProjectionDecoder;
    }
    st = malloc(size as size_t) as *mut OpusProjectionDecoder;
    if st.is_null() {
        if !error.is_null() {
            *error = OPUS_ALLOC_FAIL;
        }
        return NULL as *mut OpusProjectionDecoder;
    }
    ret = opus_projection_decoder_init(
        st,
        Fs,
        channels,
        streams,
        coupled_streams,
        demixing_matrix,
        demixing_matrix_size,
    );
    if ret != OPUS_OK {
        free(st as *mut core::ffi::c_void);
        st = NULL as *mut OpusProjectionDecoder;
    }
    if !error.is_null() {
        *error = ret;
    }
    return st;
}
pub unsafe fn opus_projection_decode(
    st: *mut OpusProjectionDecoder,
    data: *const u8,
    len: i32,
    pcm: *mut i16,
    frame_size: i32,
    decode_fec: i32,
) -> i32 {
    return opus_multistream_decode_native(
        get_multistream_decoder(st),
        data,
        len,
        pcm as *mut core::ffi::c_void,
        Some(
            opus_projection_copy_channel_out_short
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
        get_dec_demixing_matrix(st) as *mut core::ffi::c_void,
    );
}
pub unsafe fn opus_projection_decode_float(
    st: *mut OpusProjectionDecoder,
    data: *const u8,
    len: i32,
    pcm: *mut f32,
    frame_size: i32,
    decode_fec: i32,
) -> i32 {
    return opus_multistream_decode_native(
        get_multistream_decoder(st),
        data,
        len,
        pcm as *mut core::ffi::c_void,
        Some(
            opus_projection_copy_channel_out_float
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
        get_dec_demixing_matrix(st) as *mut core::ffi::c_void,
    );
}
pub unsafe fn opus_projection_decoder_ctl_impl(
    st: *mut OpusProjectionDecoder,
    request: i32,
    args: VarArgs,
) -> i32 {
    let mut ret: i32 = OPUS_OK;
    ret = opus_multistream_decoder_ctl_va_list(get_multistream_decoder(st), request, args);
    return ret;
}
#[macro_export]
macro_rules! opus_projection_decoder_ctl {
    ($st:expr, $request:expr, $($arg:expr),*) => {
        $crate::opus_projection_decoder_ctl_impl($st, $request, $crate::varargs!($($arg),*))
    };
    ($st:expr, $request:expr) => {
        opus_projection_decoder_ctl!($st, $request,)
    };
    ($st:expr, $request:expr, $($arg:expr),*,) => {
        opus_projection_decoder_ctl!($st, $request, $($arg),*)
    };
}
pub unsafe fn opus_projection_decoder_destroy(st: *mut OpusProjectionDecoder) {
    free(st as *mut core::ffi::c_void);
}
