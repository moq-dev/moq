use crate::externs::{free, malloc};
#[derive(Copy, Clone)]
#[repr(C)]
pub struct OpusRepacketizer {
    pub(crate) toc: u8,
    pub(crate) nb_frames: i32,
    pub(crate) frames: [*const u8; 48],
    pub(crate) len: [i16; 48],
    pub(crate) framesize: i32,
}

pub mod stddef_h {
    pub type size_t = u64;
    pub const NULL: i32 = 0;
}
pub use self::stddef_h::{size_t, NULL};
use crate::src::opus_defines::{OPUS_BAD_ARG, OPUS_BUFFER_TOO_SMALL, OPUS_INVALID_PACKET, OPUS_OK};

use crate::externs::memmove;
use crate::src::opus::{encode_size, opus_packet_parse_impl};
use crate::{opus_packet_get_nb_frames, opus_packet_get_samples_per_frame};

pub unsafe fn opus_repacketizer_get_size() -> i32 {
    return ::core::mem::size_of::<OpusRepacketizer>() as u64 as i32;
}
pub unsafe fn opus_repacketizer_init(rp: *mut OpusRepacketizer) -> *mut OpusRepacketizer {
    (*rp).nb_frames = 0;
    return rp;
}
pub unsafe fn opus_repacketizer_create() -> *mut OpusRepacketizer {
    let mut rp: *mut OpusRepacketizer = 0 as *mut OpusRepacketizer;
    rp = malloc(opus_repacketizer_get_size() as size_t) as *mut OpusRepacketizer;
    if rp.is_null() {
        return NULL as *mut OpusRepacketizer;
    }
    return opus_repacketizer_init(rp);
}
pub unsafe fn opus_repacketizer_destroy(rp: *mut OpusRepacketizer) {
    free(rp as *mut core::ffi::c_void);
}
unsafe fn opus_repacketizer_cat_impl(
    rp: *mut OpusRepacketizer,
    data: *const u8,
    len: i32,
    self_delimited: i32,
) -> i32 {
    let mut tmp_toc: u8 = 0;
    let mut curr_nb_frames: i32 = 0;
    let mut ret: i32 = 0;
    if len < 1 {
        return OPUS_INVALID_PACKET;
    }
    if (*rp).nb_frames == 0 {
        (*rp).toc = *data.offset(0 as isize);
        (*rp).framesize = opus_packet_get_samples_per_frame(data, 8000);
    } else if (*rp).toc as i32 & 0xfc != *data.offset(0 as isize) as i32 & 0xfc {
        return OPUS_INVALID_PACKET;
    }
    curr_nb_frames = opus_packet_get_nb_frames(data, len);
    if curr_nb_frames < 1 {
        return OPUS_INVALID_PACKET;
    }
    if (curr_nb_frames + (*rp).nb_frames) * (*rp).framesize > 960 {
        return OPUS_INVALID_PACKET;
    }
    ret = opus_packet_parse_impl(
        data,
        len,
        self_delimited,
        &mut tmp_toc,
        &mut *((*rp).frames).as_mut_ptr().offset((*rp).nb_frames as isize),
        &mut *((*rp).len).as_mut_ptr().offset((*rp).nb_frames as isize),
        NULL as *mut i32,
        NULL as *mut i32,
    );
    if ret < 1 {
        return ret;
    }
    (*rp).nb_frames += curr_nb_frames;
    return OPUS_OK;
}
pub unsafe fn opus_repacketizer_cat(rp: *mut OpusRepacketizer, data: *const u8, len: i32) -> i32 {
    return opus_repacketizer_cat_impl(rp, data, len, 0);
}
pub unsafe fn opus_repacketizer_get_nb_frames(rp: *mut OpusRepacketizer) -> i32 {
    return (*rp).nb_frames;
}
pub unsafe fn opus_repacketizer_out_range_impl(
    rp: *mut OpusRepacketizer,
    begin: i32,
    end: i32,
    data: *mut u8,
    maxlen: i32,
    self_delimited: i32,
    pad: i32,
) -> i32 {
    let mut i: i32 = 0;
    let mut count: i32 = 0;
    let mut tot_size: i32 = 0;
    let mut len: *mut i16 = 0 as *mut i16;
    let mut frames: *mut *const u8 = 0 as *mut *const u8;
    let mut ptr: *mut u8 = 0 as *mut u8;
    if begin < 0 || begin >= end || end > (*rp).nb_frames {
        return OPUS_BAD_ARG;
    }
    count = end - begin;
    len = ((*rp).len).as_mut_ptr().offset(begin as isize);
    frames = ((*rp).frames).as_mut_ptr().offset(begin as isize);
    if self_delimited != 0 {
        tot_size = 1 + (*len.offset((count - 1) as isize) as i32 >= 252) as i32;
    } else {
        tot_size = 0;
    }
    ptr = data;
    if count == 1 {
        tot_size += *len.offset(0 as isize) as i32 + 1;
        if tot_size > maxlen {
            return OPUS_BUFFER_TOO_SMALL;
        }
        let fresh0 = ptr;
        ptr = ptr.offset(1);
        *fresh0 = ((*rp).toc as i32 & 0xfc) as u8;
    } else if count == 2 {
        if *len.offset(1 as isize) as i32 == *len.offset(0 as isize) as i32 {
            tot_size += 2 * *len.offset(0 as isize) as i32 + 1;
            if tot_size > maxlen {
                return OPUS_BUFFER_TOO_SMALL;
            }
            let fresh1 = ptr;
            ptr = ptr.offset(1);
            *fresh1 = ((*rp).toc as i32 & 0xfc | 0x1) as u8;
        } else {
            tot_size += *len.offset(0 as isize) as i32
                + *len.offset(1 as isize) as i32
                + 2
                + (*len.offset(0 as isize) as i32 >= 252) as i32;
            if tot_size > maxlen {
                return OPUS_BUFFER_TOO_SMALL;
            }
            let fresh2 = ptr;
            ptr = ptr.offset(1);
            *fresh2 = ((*rp).toc as i32 & 0xfc | 0x2) as u8;
            ptr = ptr.offset(encode_size(*len.offset(0 as isize) as i32, ptr) as isize);
        }
    }
    if count > 2 || pad != 0 && tot_size < maxlen {
        let mut vbr: i32 = 0;
        let mut pad_amount: i32 = 0;
        ptr = data;
        if self_delimited != 0 {
            tot_size = 1 + (*len.offset((count - 1) as isize) as i32 >= 252) as i32;
        } else {
            tot_size = 0;
        }
        vbr = 0;
        i = 1;
        while i < count {
            if *len.offset(i as isize) as i32 != *len.offset(0 as isize) as i32 {
                vbr = 1;
                break;
            } else {
                i += 1;
            }
        }
        if vbr != 0 {
            tot_size += 2;
            i = 0;
            while i < count - 1 {
                tot_size += 1
                    + (*len.offset(i as isize) as i32 >= 252) as i32
                    + *len.offset(i as isize) as i32;
                i += 1;
            }
            tot_size += *len.offset((count - 1) as isize) as i32;
            if tot_size > maxlen {
                return OPUS_BUFFER_TOO_SMALL;
            }
            let fresh3 = ptr;
            ptr = ptr.offset(1);
            *fresh3 = ((*rp).toc as i32 & 0xfc | 0x3) as u8;
            let fresh4 = ptr;
            ptr = ptr.offset(1);
            *fresh4 = (count | 0x80) as u8;
        } else {
            tot_size += count * *len.offset(0 as isize) as i32 + 2;
            if tot_size > maxlen {
                return OPUS_BUFFER_TOO_SMALL;
            }
            let fresh5 = ptr;
            ptr = ptr.offset(1);
            *fresh5 = ((*rp).toc as i32 & 0xfc | 0x3) as u8;
            let fresh6 = ptr;
            ptr = ptr.offset(1);
            *fresh6 = count as u8;
        }
        pad_amount = if pad != 0 { maxlen - tot_size } else { 0 };
        if pad_amount != 0 {
            let mut nb_255s: i32 = 0;
            let ref mut fresh7 = *data.offset(1 as isize);
            *fresh7 = (*fresh7 as i32 | 0x40) as u8;
            nb_255s = (pad_amount - 1) / 255;
            i = 0;
            while i < nb_255s {
                let fresh8 = ptr;
                ptr = ptr.offset(1);
                *fresh8 = 255;
                i += 1;
            }
            let fresh9 = ptr;
            ptr = ptr.offset(1);
            *fresh9 = (pad_amount - 255 * nb_255s - 1) as u8;
            tot_size += pad_amount;
        }
        if vbr != 0 {
            i = 0;
            while i < count - 1 {
                ptr = ptr.offset(encode_size(*len.offset(i as isize) as i32, ptr) as isize);
                i += 1;
            }
        }
    }
    if self_delimited != 0 {
        let sdlen: i32 = encode_size(*len.offset((count - 1) as isize) as i32, ptr);
        ptr = ptr.offset(sdlen as isize);
    }
    i = 0;
    while i < count {
        memmove(
            ptr as *mut core::ffi::c_void,
            *frames.offset(i as isize) as *const core::ffi::c_void,
            (*len.offset(i as isize) as u64)
                .wrapping_mul(::core::mem::size_of::<u8>() as u64)
                .wrapping_add((0 * ptr.offset_from(*frames.offset(i as isize)) as i64) as u64),
        );
        ptr = ptr.offset(*len.offset(i as isize) as i32 as isize);
        i += 1;
    }
    if pad != 0 {
        while ptr < data.offset(maxlen as isize) {
            let fresh10 = ptr;
            ptr = ptr.offset(1);
            *fresh10 = 0;
        }
    }
    return tot_size;
}
pub unsafe fn opus_repacketizer_out_range(
    rp: *mut OpusRepacketizer,
    begin: i32,
    end: i32,
    data: *mut u8,
    maxlen: i32,
) -> i32 {
    return opus_repacketizer_out_range_impl(rp, begin, end, data, maxlen, 0, 0);
}
pub unsafe fn opus_repacketizer_out(rp: *mut OpusRepacketizer, data: *mut u8, maxlen: i32) -> i32 {
    return opus_repacketizer_out_range_impl(rp, 0, (*rp).nb_frames, data, maxlen, 0, 0);
}
pub unsafe fn opus_packet_pad(data: *mut u8, len: i32, new_len: i32) -> i32 {
    let mut rp: OpusRepacketizer = OpusRepacketizer {
        toc: 0,
        nb_frames: 0,
        frames: [0 as *const u8; 48],
        len: [0; 48],
        framesize: 0,
    };
    let mut ret: i32 = 0;
    if len < 1 {
        return OPUS_BAD_ARG;
    }
    if len == new_len {
        return OPUS_OK;
    } else {
        if len > new_len {
            return OPUS_BAD_ARG;
        }
    }
    opus_repacketizer_init(&mut rp);
    memmove(
        data.offset(new_len as isize).offset(-(len as isize)) as *mut core::ffi::c_void,
        data as *const core::ffi::c_void,
        (len as u64)
            .wrapping_mul(::core::mem::size_of::<u8>() as u64)
            .wrapping_add(
                (0 * data
                    .offset(new_len as isize)
                    .offset(-(len as isize))
                    .offset_from(data) as i64) as u64,
            ),
    );
    ret = opus_repacketizer_cat(
        &mut rp,
        data.offset(new_len as isize).offset(-(len as isize)),
        len,
    );
    if ret != OPUS_OK {
        return ret;
    }
    ret = opus_repacketizer_out_range_impl(&mut rp, 0, rp.nb_frames, data, new_len, 0, 1);
    if ret > 0 {
        return OPUS_OK;
    } else {
        return ret;
    };
}
pub unsafe fn opus_packet_unpad(data: *mut u8, len: i32) -> i32 {
    let mut rp: OpusRepacketizer = OpusRepacketizer {
        toc: 0,
        nb_frames: 0,
        frames: [0 as *const u8; 48],
        len: [0; 48],
        framesize: 0,
    };
    let mut ret: i32 = 0;
    if len < 1 {
        return OPUS_BAD_ARG;
    }
    opus_repacketizer_init(&mut rp);
    ret = opus_repacketizer_cat(&mut rp, data, len);
    if ret < 0 {
        return ret;
    }
    ret = opus_repacketizer_out_range_impl(&mut rp, 0, rp.nb_frames, data, len, 0, 0);
    assert!(ret > 0 && ret <= len);
    return ret;
}
pub unsafe fn opus_multistream_packet_pad(
    mut data: *mut u8,
    mut len: i32,
    new_len: i32,
    nb_streams: i32,
) -> i32 {
    let mut s: i32 = 0;
    let mut count: i32 = 0;
    let mut toc: u8 = 0;
    let mut size: [i16; 48] = [0; 48];
    let mut packet_offset: i32 = 0;
    let mut amount: i32 = 0;
    if len < 1 {
        return OPUS_BAD_ARG;
    }
    if len == new_len {
        return OPUS_OK;
    } else {
        if len > new_len {
            return OPUS_BAD_ARG;
        }
    }
    amount = new_len - len;
    s = 0;
    while s < nb_streams - 1 {
        if len <= 0 {
            return OPUS_INVALID_PACKET;
        }
        count = opus_packet_parse_impl(
            data,
            len,
            1,
            &mut toc,
            NULL as *mut *const u8,
            size.as_mut_ptr(),
            NULL as *mut i32,
            &mut packet_offset,
        );
        if count < 0 {
            return count;
        }
        data = data.offset(packet_offset as isize);
        len -= packet_offset;
        s += 1;
    }
    return opus_packet_pad(data, len, len + amount);
}
pub unsafe fn opus_multistream_packet_unpad(
    mut data: *mut u8,
    mut len: i32,
    nb_streams: i32,
) -> i32 {
    let mut s: i32 = 0;
    let mut toc: u8 = 0;
    let mut size: [i16; 48] = [0; 48];
    let mut packet_offset: i32 = 0;
    let mut rp: OpusRepacketizer = OpusRepacketizer {
        toc: 0,
        nb_frames: 0,
        frames: [0 as *const u8; 48],
        len: [0; 48],
        framesize: 0,
    };
    let mut dst: *mut u8 = 0 as *mut u8;
    let mut dst_len: i32 = 0;
    if len < 1 {
        return OPUS_BAD_ARG;
    }
    dst = data;
    dst_len = 0;
    s = 0;
    while s < nb_streams {
        let mut ret: i32 = 0;
        let self_delimited: i32 = (s != nb_streams - 1) as i32;
        if len <= 0 {
            return OPUS_INVALID_PACKET;
        }
        opus_repacketizer_init(&mut rp);
        ret = opus_packet_parse_impl(
            data,
            len,
            self_delimited,
            &mut toc,
            NULL as *mut *const u8,
            size.as_mut_ptr(),
            NULL as *mut i32,
            &mut packet_offset,
        );
        if ret < 0 {
            return ret;
        }
        ret = opus_repacketizer_cat_impl(&mut rp, data, packet_offset, self_delimited);
        if ret < 0 {
            return ret;
        }
        ret =
            opus_repacketizer_out_range_impl(&mut rp, 0, rp.nb_frames, dst, len, self_delimited, 0);
        if ret < 0 {
            return ret;
        } else {
            dst_len += ret;
        }
        dst = dst.offset(ret as isize);
        data = data.offset(packet_offset as isize);
        len -= packet_offset;
        s += 1;
    }
    return dst_len;
}
