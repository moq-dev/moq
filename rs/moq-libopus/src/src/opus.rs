pub mod stddef_h {
    pub const NULL: i32 = 0;
}
pub use self::stddef_h::NULL;
use crate::src::opus_defines::{OPUS_BAD_ARG, OPUS_INVALID_PACKET};

pub unsafe fn opus_pcm_soft_clip(mut _x: *mut f32, N: i32, C: i32, declip_mem: *mut f32) {
    let mut c: i32 = 0;
    let mut i: i32 = 0;
    let mut x: *mut f32 = 0 as *mut f32;
    if C < 1 || N < 1 || _x.is_null() || declip_mem.is_null() {
        return;
    }
    i = 0;
    while i < N * C {
        *_x.offset(i as isize) = if -2.0f32
            > (if 2.0f32 < *_x.offset(i as isize) {
                2.0f32
            } else {
                *_x.offset(i as isize)
            }) {
            -2.0f32
        } else if 2.0f32 < *_x.offset(i as isize) {
            2.0f32
        } else {
            *_x.offset(i as isize)
        };
        i += 1;
    }
    c = 0;
    while c < C {
        let mut a: f32 = 0.;
        let mut x0: f32 = 0.;
        let mut curr: i32 = 0;
        x = _x.offset(c as isize);
        a = *declip_mem.offset(c as isize);
        i = 0;
        while i < N {
            if *x.offset((i * C) as isize) * a >= 0 as f32 {
                break;
            }
            *x.offset((i * C) as isize) = *x.offset((i * C) as isize)
                + a * *x.offset((i * C) as isize) * *x.offset((i * C) as isize);
            i += 1;
        }
        curr = 0;
        x0 = *x.offset(0 as isize);
        loop {
            let mut start: i32 = 0;
            let mut end: i32 = 0;
            let mut maxval: f32 = 0.;
            let mut special: i32 = 0;
            let mut peak_pos: i32 = 0;
            i = curr;
            while i < N {
                if *x.offset((i * C) as isize) > 1 as f32 || *x.offset((i * C) as isize) < -1 as f32
                {
                    break;
                }
                i += 1;
            }
            if i == N {
                a = 0 as f32;
                break;
            } else {
                peak_pos = i;
                end = i;
                start = end;
                maxval = (*x.offset((i * C) as isize)).abs();
                while start > 0
                    && *x.offset((i * C) as isize) * *x.offset(((start - 1) * C) as isize)
                        >= 0 as f32
                {
                    start -= 1;
                }
                while end < N
                    && *x.offset((i * C) as isize) * *x.offset((end * C) as isize) >= 0 as f32
                {
                    if (*x.offset((end * C) as isize)).abs() > maxval {
                        maxval = (*x.offset((end * C) as isize)).abs();
                        peak_pos = end;
                    }
                    end += 1;
                }
                special = (start == 0
                    && *x.offset((i * C) as isize) * *x.offset(0 as isize) >= 0 as f32)
                    as i32;
                a = (maxval - 1 as f32) / (maxval * maxval);
                a += a * 2.4e-7f32;
                if *x.offset((i * C) as isize) > 0 as f32 {
                    a = -a;
                }
                i = start;
                while i < end {
                    *x.offset((i * C) as isize) = *x.offset((i * C) as isize)
                        + a * *x.offset((i * C) as isize) * *x.offset((i * C) as isize);
                    i += 1;
                }
                if special != 0 && peak_pos >= 2 {
                    let mut delta: f32 = 0.;
                    let mut offset: f32 = x0 - *x.offset(0 as isize);
                    delta = offset / peak_pos as f32;
                    i = curr;
                    while i < peak_pos {
                        offset -= delta;
                        *x.offset((i * C) as isize) += offset;
                        *x.offset((i * C) as isize) = if -1.0f32
                            > (if 1.0f32 < *x.offset((i * C) as isize) {
                                1.0f32
                            } else {
                                *x.offset((i * C) as isize)
                            }) {
                            -1.0f32
                        } else if 1.0f32 < *x.offset((i * C) as isize) {
                            1.0f32
                        } else {
                            *x.offset((i * C) as isize)
                        };
                        i += 1;
                    }
                }
                curr = end;
                if curr == N {
                    break;
                }
            }
        }
        *declip_mem.offset(c as isize) = a;
        c += 1;
    }
}
pub unsafe fn encode_size(size: i32, data: *mut u8) -> i32 {
    if size < 252 {
        *data.offset(0 as isize) = size as u8;
        return 1;
    } else {
        *data.offset(0 as isize) = (252 + (size & 0x3)) as u8;
        *data.offset(1 as isize) = (size - *data.offset(0 as isize) as i32 >> 2) as u8;
        return 2;
    };
}
unsafe fn parse_size(data: *const u8, len: i32, size: *mut i16) -> i32 {
    if len < 1 {
        *size = -1 as i16;
        return -1;
    } else if (*data.offset(0 as isize) as i32) < 252 {
        *size = *data.offset(0 as isize) as i16;
        return 1;
    } else if len < 2 {
        *size = -1 as i16;
        return -1;
    } else {
        *size = (4 * *data.offset(1 as isize) as i32 + *data.offset(0 as isize) as i32) as i16;
        return 2;
    };
}
pub unsafe fn opus_packet_get_samples_per_frame(data: *const u8, Fs: i32) -> i32 {
    let mut audiosize: i32 = 0;
    if *data.offset(0 as isize) as i32 & 0x80 != 0 {
        audiosize = *data.offset(0 as isize) as i32 >> 3 & 0x3;
        audiosize = (Fs << audiosize) / 400;
    } else if *data.offset(0 as isize) as i32 & 0x60 == 0x60 {
        audiosize = if *data.offset(0 as isize) as i32 & 0x8 != 0 {
            Fs / 50
        } else {
            Fs / 100
        };
    } else {
        audiosize = *data.offset(0 as isize) as i32 >> 3 & 0x3;
        if audiosize == 3 {
            audiosize = Fs * 60 / 1000;
        } else {
            audiosize = (Fs << audiosize) / 100;
        }
    }
    return audiosize;
}
pub unsafe fn opus_packet_parse_impl(
    mut data: *const u8,
    mut len: i32,
    self_delimited: i32,
    out_toc: *mut u8,
    frames: *mut *const u8,
    size: *mut i16,
    payload_offset: *mut i32,
    packet_offset: *mut i32,
) -> i32 {
    let mut i: i32 = 0;
    let mut bytes: i32 = 0;
    let mut count: i32 = 0;
    let mut cbr: i32 = 0;
    let mut ch: u8 = 0;
    let mut toc: u8 = 0;
    let mut framesize: i32 = 0;
    let mut last_size: i32 = 0;
    let mut pad: i32 = 0;
    let data0: *const u8 = data;
    if size.is_null() || len < 0 {
        return OPUS_BAD_ARG;
    }
    if len == 0 {
        return OPUS_INVALID_PACKET;
    }
    framesize = opus_packet_get_samples_per_frame(data, 48000);
    cbr = 0;
    let fresh0 = data;
    data = data.offset(1);
    toc = *fresh0;
    len -= 1;
    last_size = len;
    match toc as i32 & 0x3 {
        0 => {
            count = 1;
        }
        1 => {
            count = 2;
            cbr = 1;
            if self_delimited == 0 {
                if len & 0x1 != 0 {
                    return OPUS_INVALID_PACKET;
                }
                last_size = len / 2;
                *size.offset(0 as isize) = last_size as i16;
            }
        }
        2 => {
            count = 2;
            bytes = parse_size(data, len, size);
            len -= bytes;
            if (*size.offset(0 as isize) as i32) < 0 || *size.offset(0 as isize) as i32 > len {
                return OPUS_INVALID_PACKET;
            }
            data = data.offset(bytes as isize);
            last_size = len - *size.offset(0 as isize) as i32;
        }
        _ => {
            if len < 1 {
                return OPUS_INVALID_PACKET;
            }
            let fresh1 = data;
            data = data.offset(1);
            ch = *fresh1;
            count = ch as i32 & 0x3f;
            if count <= 0 || framesize * count > 5760 {
                return OPUS_INVALID_PACKET;
            }
            len -= 1;
            if ch as i32 & 0x40 != 0 {
                let mut p: i32 = 0;
                loop {
                    let mut tmp: i32 = 0;
                    if len <= 0 {
                        return OPUS_INVALID_PACKET;
                    }
                    let fresh2 = data;
                    data = data.offset(1);
                    p = *fresh2 as i32;
                    len -= 1;
                    tmp = if p == 255 { 254 } else { p };
                    len -= tmp;
                    pad += tmp;
                    if !(p == 255) {
                        break;
                    }
                }
            }
            if len < 0 {
                return OPUS_INVALID_PACKET;
            }
            cbr = (ch as i32 & 0x80 == 0) as i32;
            if cbr == 0 {
                last_size = len;
                i = 0;
                while i < count - 1 {
                    bytes = parse_size(data, len, size.offset(i as isize));
                    len -= bytes;
                    if (*size.offset(i as isize) as i32) < 0
                        || *size.offset(i as isize) as i32 > len
                    {
                        return OPUS_INVALID_PACKET;
                    }
                    data = data.offset(bytes as isize);
                    last_size -= bytes + *size.offset(i as isize) as i32;
                    i += 1;
                }
                if last_size < 0 {
                    return OPUS_INVALID_PACKET;
                }
            } else if self_delimited == 0 {
                last_size = len / count;
                if last_size * count != len {
                    return OPUS_INVALID_PACKET;
                }
                i = 0;
                while i < count - 1 {
                    *size.offset(i as isize) = last_size as i16;
                    i += 1;
                }
            }
        }
    }
    if self_delimited != 0 {
        bytes = parse_size(data, len, size.offset(count as isize).offset(-(1 as isize)));
        len -= bytes;
        if (*size.offset((count - 1) as isize) as i32) < 0
            || *size.offset((count - 1) as isize) as i32 > len
        {
            return OPUS_INVALID_PACKET;
        }
        data = data.offset(bytes as isize);
        if cbr != 0 {
            if *size.offset((count - 1) as isize) as i32 * count > len {
                return OPUS_INVALID_PACKET;
            }
            i = 0;
            while i < count - 1 {
                *size.offset(i as isize) = *size.offset((count - 1) as isize);
                i += 1;
            }
        } else if bytes + *size.offset((count - 1) as isize) as i32 > last_size {
            return OPUS_INVALID_PACKET;
        }
    } else {
        if last_size > 1275 {
            return OPUS_INVALID_PACKET;
        }
        *size.offset((count - 1) as isize) = last_size as i16;
    }
    if !payload_offset.is_null() {
        *payload_offset = data.offset_from(data0) as i64 as i32;
    }
    i = 0;
    while i < count {
        if !frames.is_null() {
            let ref mut fresh3 = *frames.offset(i as isize);
            *fresh3 = data;
        }
        data = data.offset(*size.offset(i as isize) as i32 as isize);
        i += 1;
    }
    if !packet_offset.is_null() {
        *packet_offset = pad + data.offset_from(data0) as i64 as i32;
    }
    if !out_toc.is_null() {
        *out_toc = toc;
    }
    return count;
}
pub unsafe fn opus_packet_parse(
    data: *const u8,
    len: i32,
    out_toc: *mut u8,
    frames: *mut *const u8,
    size: *mut i16,
    payload_offset: *mut i32,
) -> i32 {
    return opus_packet_parse_impl(
        data,
        len,
        0,
        out_toc,
        frames,
        size,
        payload_offset,
        NULL as *mut i32,
    );
}
