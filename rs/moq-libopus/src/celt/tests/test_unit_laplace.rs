pub mod stddef_h {
        pub type size_t = u64;
}
pub mod entcode_h {
        pub type ec_window = u32;
    #[derive(Copy, Clone)]
    #[repr(C)]
        pub struct ec_ctx {
        pub buf: *mut u8,
        pub storage: u32,
        pub end_offs: u32,
        pub end_window: ec_window,
        pub nend_bits: i32,
        pub nbits_total: i32,
        pub offs: u32,
        pub rng: u32,
        pub val: u32,
        pub ext: u32,
        pub rem: i32,
        pub error: i32,
    }
        pub type ec_enc = ec_ctx;
        pub type ec_dec = ec_ctx;
    #[inline]
        pub unsafe fn ec_range_bytes(mut _this: *mut ec_ctx) -> u32 {
        return (*_this).offs;
    }
    #[inline]
        pub unsafe fn ec_get_buffer(mut _this: *mut ec_ctx) -> *mut u8 {
        return (*_this).buf;
    }
    #[inline]
        pub unsafe fn celt_udiv(mut n: u32, mut d: u32) -> u32 {
        return n.wrapping_div(d);
    }
}
pub mod stdio_h {
    use super::FILE_h::FILE;
    {
                pub static mut stderr: *mut FILE;
                pub fn fprintf(_: *mut FILE, _: *const i8, _: ...) -> i32;
    }
}
pub mod stdlib_h {
    {
                pub fn rand() -> i32;
                pub fn malloc(_: u64) -> *mut core::ffi::c_void;
                pub fn free(_: *mut core::ffi::c_void);
    }
}
pub mod entenc_c {
        pub unsafe fn ec_enc_carry_out(mut _this: *mut ec_enc, mut _c: i32) {
        if _c as u32
            != ((1 as u32) << 8)
                .wrapping_sub(1)
        {
            let mut carry: i32 = 0;
            carry = _c >> 8;
            if (*_this).rem >= 0 {
                (*_this).error |= ec_write_byte(_this, ((*_this).rem + carry) as u32);
            }
            if (*_this).ext > 0 {
                let mut sym: u32 = 0;
                sym = ((1 as u32) << 8)
                    .wrapping_sub(1)
                    .wrapping_add(carry as u32)
                    & ((1 as u32) << 8)
                        .wrapping_sub(1);
                loop {
                    (*_this).error |= ec_write_byte(_this, sym);
                    (*_this).ext = ((*_this).ext).wrapping_sub(1);
                    if !((*_this).ext > 0) {
                        break;
                    }
                }
            }
            (*_this).rem = (_c as u32
                & ((1 as u32) << 8)
                    .wrapping_sub(1))
                as i32;
        } else {
            (*_this).ext = ((*_this).ext).wrapping_add(1);
        };
    }
        pub unsafe fn ec_write_byte(
        mut _this: *mut ec_enc,
        mut _value: u32,
    ) -> i32 {
        if ((*_this).offs).wrapping_add((*_this).end_offs) >= (*_this).storage {
            return -1;
        }
        let fresh0 = (*_this).offs;
        (*_this).offs = ((*_this).offs).wrapping_add(1);
        *((*_this).buf).offset(fresh0 as isize) = _value as u8;
        return 0;
    }
        pub unsafe fn ec_write_byte_at_end(
        mut _this: *mut ec_enc,
        mut _value: u32,
    ) -> i32 {
        if ((*_this).offs).wrapping_add((*_this).end_offs) >= (*_this).storage {
            return -1;
        }
        (*_this).end_offs = ((*_this).end_offs).wrapping_add(1);
        *((*_this).buf).offset(((*_this).storage).wrapping_sub((*_this).end_offs) as isize) =
            _value as u8;
        return 0;
    }
    #[inline]
        pub unsafe fn ec_enc_normalize(mut _this: *mut ec_enc) {
        while (*_this).rng
            <= (1 as u32) << 32 - 1 >> 8
        {
            ec_enc_carry_out(
                _this,
                ((*_this).val >> 32 - 8 - 1)
                    as i32,
            );
            (*_this).val = (*_this).val << 8
                & ((1 as u32) << 32 - 1)
                    .wrapping_sub(1);
            (*_this).rng <<= 8;
            (*_this).nbits_total += 8;
        }
    }
        pub unsafe fn ec_enc_init(
        mut _this: *mut ec_enc,
        mut _buf: *mut u8,
        mut _size: u32,
    ) {
        (*_this).buf = _buf;
        (*_this).end_offs = 0;
        (*_this).end_window = 0 as ec_window;
        (*_this).nend_bits = 0;
        (*_this).nbits_total = 32 + 1;
        (*_this).offs = 0;
        (*_this).rng = (1 as u32) << 32 - 1;
        (*_this).rem = -1;
        (*_this).val = 0;
        (*_this).ext = 0;
        (*_this).storage = _size;
        (*_this).error = 0;
    }
        pub unsafe fn ec_encode(
        mut _this: *mut ec_enc,
        mut _fl: u32,
        mut _fh: u32,
        mut _ft: u32,
    ) {
        let mut r: u32 = 0;
        r = celt_udiv((*_this).rng, _ft);
        if _fl > 0 {
            (*_this).val = ((*_this).val as u32)
                .wrapping_add(((*_this).rng).wrapping_sub(r.wrapping_mul(_ft.wrapping_sub(_fl))))
                as u32 as u32;
            (*_this).rng = r.wrapping_mul(_fh.wrapping_sub(_fl));
        } else {
            (*_this).rng = ((*_this).rng as u32)
                .wrapping_sub(r.wrapping_mul(_ft.wrapping_sub(_fh)))
                as u32 as u32;
        }
        ec_enc_normalize(_this);
    }
        pub unsafe fn ec_encode_bin(
        mut _this: *mut ec_enc,
        mut _fl: u32,
        mut _fh: u32,
        mut _bits: u32,
    ) {
        let mut r: u32 = 0;
        r = (*_this).rng >> _bits;
        if _fl > 0 {
            (*_this).val = ((*_this).val as u32).wrapping_add(
                ((*_this).rng)
                    .wrapping_sub(r.wrapping_mul(((1 as u32) << _bits).wrapping_sub(_fl))),
            ) as u32 as u32;
            (*_this).rng = r.wrapping_mul(_fh.wrapping_sub(_fl));
        } else {
            (*_this).rng = ((*_this).rng as u32)
                .wrapping_sub(r.wrapping_mul(((1 as u32) << _bits).wrapping_sub(_fh)))
                as u32 as u32;
        }
        ec_enc_normalize(_this);
    }
        pub unsafe fn ec_enc_bit_logp(
        mut _this: *mut ec_enc,
        mut _val: i32,
        mut _logp: u32,
    ) {
        let mut r: u32 = 0;
        let mut s: u32 = 0;
        let mut l: u32 = 0;
        r = (*_this).rng;
        l = (*_this).val;
        s = r >> _logp;
        r = (r as u32).wrapping_sub(s) as u32 as u32;
        if _val != 0 {
            (*_this).val = l.wrapping_add(r);
        }
        (*_this).rng = if _val != 0 { s } else { r };
        ec_enc_normalize(_this);
    }
        pub unsafe fn ec_enc_icdf(
        mut _this: *mut ec_enc,
        mut _s: i32,
        mut _icdf: *const u8,
        mut _ftb: u32,
    ) {
        let mut r: u32 = 0;
        r = (*_this).rng >> _ftb;
        if _s > 0 {
            (*_this).val = ((*_this).val as u32).wrapping_add(((*_this).rng).wrapping_sub(
                r.wrapping_mul(*_icdf.offset((_s - 1) as isize) as u32),
            )) as u32 as u32;
            (*_this).rng = r.wrapping_mul(
                (*_icdf.offset((_s - 1) as isize) as i32
                    - *_icdf.offset(_s as isize) as i32) as u32,
            );
        } else {
            (*_this).rng = ((*_this).rng as u32)
                .wrapping_sub(r.wrapping_mul(*_icdf.offset(_s as isize) as u32))
                as u32 as u32;
        }
        ec_enc_normalize(_this);
    }
        pub unsafe fn ec_enc_uint(mut _this: *mut ec_enc, mut _fl: u32, mut _ft: u32) {
        let mut ft: u32 = 0;
        let mut fl: u32 = 0;
        let mut ftb: i32 = 0;
        _ft = _ft.wrapping_sub(1);
        ftb = ::core::mem::size_of::<u32>() as u64 as i32
            * 8
            - _ft.leading_zeros() as i32;
        if ftb > 8 {
            ftb -= 8;
            ft = (_ft >> ftb).wrapping_add(1);
            fl = _fl >> ftb;
            ec_encode(
                _this,
                fl,
                fl.wrapping_add(1),
                ft,
            );
            ec_enc_bits(
                _this,
                _fl & ((1) << ftb).wrapping_sub(1 as u32),
                ftb as u32,
            );
        } else {
            ec_encode(
                _this,
                _fl,
                _fl.wrapping_add(1),
                _ft.wrapping_add(1),
            );
        };
    }
        pub unsafe fn ec_enc_bits(
        mut _this: *mut ec_enc,
        mut _fl: u32,
        mut _bits: u32,
    ) {
        let mut window: ec_window = 0;
        let mut used: i32 = 0;
        window = (*_this).end_window;
        used = (*_this).nend_bits;
        if (used as u32).wrapping_add(_bits)
            > (::core::mem::size_of::<ec_window>() as u64 as i32
                * 8) as u32
        {
            loop {
                (*_this).error |= ec_write_byte_at_end(
                    _this,
                    window
                        & ((1 as u32) << 8)
                            .wrapping_sub(1),
                );
                window >>= 8;
                used -= 8;
                if !(used >= 8) {
                    break;
                }
            }
        }
        window |= _fl << used;
        used = (used as u32).wrapping_add(_bits) as i32 as i32;
        (*_this).end_window = window;
        (*_this).nend_bits = used;
        (*_this).nbits_total = ((*_this).nbits_total as u32).wrapping_add(_bits)
            as i32 as i32;
    }
        pub unsafe fn ec_enc_patch_initial_bits(
        mut _this: *mut ec_enc,
        mut _val: u32,
        mut _nbits: u32,
    ) {
        let mut shift: i32 = 0;
        let mut mask: u32 = 0;
        shift = (8).wrapping_sub(_nbits) as i32;
        mask = ((((1) << _nbits) - 1) << shift) as u32;
        if (*_this).offs > 0 {
            *((*_this).buf).offset(0 as isize) =
                (*((*_this).buf).offset(0 as isize) as u32 & !mask
                    | _val << shift) as u8;
        } else if (*_this).rem >= 0 {
            (*_this).rem = ((*_this).rem as u32 & !mask | _val << shift) as i32;
        } else if (*_this).rng
            <= (1 as u32) << 32 - 1 >> _nbits
        {
            (*_this).val = (*_this).val
                & !(mask << 32 - 8 - 1)
                | _val << 32 - 8 - 1 + shift;
        } else {
            (*_this).error = -1;
        };
    }
        pub unsafe fn ec_enc_shrink(mut _this: *mut ec_enc, mut _size: u32) {
        memmove(
            ((*_this).buf)
                .offset(_size as isize)
                .offset(-((*_this).end_offs as isize)) as *mut core::ffi::c_void,
            ((*_this).buf)
                .offset((*_this).storage as isize)
                .offset(-((*_this).end_offs as isize)) as *const core::ffi::c_void,
            ((*_this).end_offs as u64)
                .wrapping_mul(::core::mem::size_of::<u8>() as u64)
                .wrapping_add(
                    (0
                        * ((*_this).buf)
                            .offset(_size as isize)
                            .offset(-((*_this).end_offs as isize))
                            .offset_from(
                                ((*_this).buf)
                                    .offset((*_this).storage as isize)
                                    .offset(-((*_this).end_offs as isize)),
                            ) as i64) as u64,
                ),
        );
        (*_this).storage = _size;
    }
        pub unsafe fn ec_enc_done(mut _this: *mut ec_enc) {
        let mut window: ec_window = 0;
        let mut used: i32 = 0;
        let mut msk: u32 = 0;
        let mut end: u32 = 0;
        let mut l: i32 = 0;
        l = 32
            - (::core::mem::size_of::<u32>() as u64 as i32
                * 8
                - ((*_this).rng).leading_zeros() as i32);
        msk = ((1 as u32) << 32 - 1)
            .wrapping_sub(1)
            >> l;
        end = ((*_this).val).wrapping_add(msk) & !msk;
        if end | msk >= ((*_this).val).wrapping_add((*_this).rng) {
            l += 1;
            msk >>= 1;
            end = ((*_this).val).wrapping_add(msk) & !msk;
        }
        while l > 0 {
            ec_enc_carry_out(
                _this,
                (end >> 32 - 8 - 1) as i32,
            );
            end = end << 8
                & ((1 as u32) << 32 - 1)
                    .wrapping_sub(1);
            l -= 8;
        }
        if (*_this).rem >= 0 || (*_this).ext > 0 {
            ec_enc_carry_out(_this, 0);
        }
        window = (*_this).end_window;
        used = (*_this).nend_bits;
        while used >= 8 {
            (*_this).error |= ec_write_byte_at_end(
                _this,
                window
                    & ((1 as u32) << 8)
                        .wrapping_sub(1),
            );
            window >>= 8;
            used -= 8;
        }
        if (*_this).error == 0 {
            memset(
                ((*_this).buf).offset((*_this).offs as isize) as *mut core::ffi::c_void,
                0,
                (((*_this).storage)
                    .wrapping_sub((*_this).offs)
                    .wrapping_sub((*_this).end_offs) as u64)
                    .wrapping_mul(::core::mem::size_of::<u8>() as u64),
            );
            if used > 0 {
                if (*_this).end_offs >= (*_this).storage {
                    (*_this).error = -1;
                } else {
                    l = -l;
                    if ((*_this).offs).wrapping_add((*_this).end_offs) >= (*_this).storage
                        && l < used
                    {
                        window &= (((1) << l) - 1) as u32;
                        (*_this).error = -1;
                    }
                    let ref mut fresh1 = *((*_this).buf).offset(
                        ((*_this).storage)
                            .wrapping_sub((*_this).end_offs)
                            .wrapping_sub(1)
                            as isize,
                    );
                    *fresh1 = (*fresh1 as i32 | window as u8 as i32)
                        as u8;
                }
            }
        }
    }
    use super::entcode_h::{celt_udiv, ec_enc, ec_window};
    use crate::externs::{memmove, memset};
}
pub mod entdec_c {
        pub unsafe fn ec_read_byte(mut _this: &mut ec_dec) -> i32 {
        return if (*_this).offs < (*_this).storage {
            let fresh2 = (*_this).offs;
            (*_this).offs = ((*_this).offs).wrapping_add(1);
            *((*_this).buf).offset(fresh2 as isize) as i32
        } else {
            0
        };
    }
        pub unsafe fn ec_read_byte_from_end(mut _this: &mut ec_dec) -> i32 {
        return if (*_this).end_offs < (*_this).storage {
            (*_this).end_offs = ((*_this).end_offs).wrapping_add(1);
            *((*_this).buf).offset(((*_this).storage).wrapping_sub((*_this).end_offs) as isize)
                as i32
        } else {
            0
        };
    }
        pub unsafe fn ec_dec_normalize(mut _this: &mut ec_dec) {
        while (*_this).rng
            <= (1 as u32) << 32 - 1 >> 8
        {
            let mut sym: i32 = 0;
            (*_this).nbits_total += 8;
            (*_this).rng <<= 8;
            sym = (*_this).rem;
            (*_this).rem = ec_read_byte(_this);
            sym = (sym << 8 | (*_this).rem)
                >> 8
                    - ((32 - 2) % 8
                        + 1);
            (*_this).val = ((*_this).val << 8).wrapping_add(
                ((1 as u32) << 8)
                    .wrapping_sub(1)
                    & !sym as u32,
            ) & ((1 as u32) << 32 - 1)
                .wrapping_sub(1);
        }
    }
        pub unsafe fn ec_dec_init(
        mut _this: &mut ec_dec,
        mut _buf: *mut u8,
        mut _storage: u32,
    ) {
        (*_this).buf = _buf;
        (*_this).storage = _storage;
        (*_this).end_offs = 0;
        (*_this).end_window = 0 as ec_window;
        (*_this).nend_bits = 0;
        (*_this).nbits_total = 32 + 1
            - (32
                - ((32 - 2) % 8 + 1))
                / 8
                * 8;
        (*_this).offs = 0;
        (*_this).rng = (1 as u32)
            << (32 - 2) % 8 + 1;
        (*_this).rem = ec_read_byte(_this);
        (*_this).val = ((*_this).rng)
            .wrapping_sub(1)
            .wrapping_sub(
                ((*_this).rem
                    >> 8
                        - ((32 - 2) % 8
                            + 1)) as u32,
            );
        (*_this).error = 0;
        ec_dec_normalize(_this);
    }
        pub unsafe fn ec_decode(
        mut _this: &mut ec_dec,
        mut _ft: u32,
    ) -> u32 {
        let mut s: u32 = 0;
        (*_this).ext = celt_udiv((*_this).rng, _ft);
        s = ((*_this).val).wrapping_div((*_this).ext);
        return _ft.wrapping_sub(
            s.wrapping_add(1)
                .wrapping_add(
                    _ft.wrapping_sub(s.wrapping_add(1))
                        & -((_ft < s.wrapping_add(1)) as i32)
                            as u32,
                ),
        );
    }
        pub unsafe fn ec_decode_bin(
        mut _this: &mut ec_dec,
        mut _bits: u32,
    ) -> u32 {
        let mut s: u32 = 0;
        (*_this).ext = (*_this).rng >> _bits;
        s = ((*_this).val).wrapping_div((*_this).ext);
        return ((1 as u32) << _bits).wrapping_sub(
            s.wrapping_add(1 as u32).wrapping_add(
                ((1 as u32) << _bits).wrapping_sub(s.wrapping_add(1 as u32))
                    & -(((1 as u32) << _bits < s.wrapping_add(1 as u32))
                        as i32) as u32,
            ),
        );
    }
        pub unsafe fn ec_dec_update(
        mut _this: &mut ec_dec,
        mut _fl: u32,
        mut _fh: u32,
        mut _ft: u32,
    ) {
        let mut s: u32 = 0;
        s = ((*_this).ext).wrapping_mul(_ft.wrapping_sub(_fh));
        (*_this).val = ((*_this).val as u32).wrapping_sub(s) as u32 as u32;
        (*_this).rng = if _fl > 0 {
            ((*_this).ext).wrapping_mul(_fh.wrapping_sub(_fl))
        } else {
            ((*_this).rng).wrapping_sub(s)
        };
        ec_dec_normalize(_this);
    }
        pub unsafe fn ec_dec_bit_logp(
        mut _this: &mut ec_dec,
        mut _logp: u32,
    ) -> i32 {
        let mut r: u32 = 0;
        let mut d: u32 = 0;
        let mut s: u32 = 0;
        let mut ret: i32 = 0;
        r = (*_this).rng;
        d = (*_this).val;
        s = r >> _logp;
        ret = (d < s) as i32;
        if ret == 0 {
            (*_this).val = d.wrapping_sub(s);
        }
        (*_this).rng = if ret != 0 { s } else { r.wrapping_sub(s) };
        ec_dec_normalize(_this);
        return ret;
    }
        pub unsafe fn ec_dec_icdf(
        mut _this: &mut ec_dec,
        mut _icdf: *const u8,
        mut _ftb: u32,
    ) -> i32 {
        let mut r: u32 = 0;
        let mut d: u32 = 0;
        let mut s: u32 = 0;
        let mut t: u32 = 0;
        let mut ret: i32 = 0;
        s = (*_this).rng;
        d = (*_this).val;
        r = s >> _ftb;
        ret = -1;
        loop {
            t = s;
            ret += 1;
            s = r.wrapping_mul(*_icdf.offset(ret as isize) as u32);
            if !(d < s) {
                break;
            }
        }
        (*_this).val = d.wrapping_sub(s);
        (*_this).rng = t.wrapping_sub(s);
        ec_dec_normalize(_this);
        return ret;
    }
        pub unsafe fn ec_dec_uint(mut _this: &mut ec_dec, mut _ft: u32) -> u32 {
        let mut ft: u32 = 0;
        let mut s: u32 = 0;
        let mut ftb: i32 = 0;
        _ft = _ft.wrapping_sub(1);
        ftb = ::core::mem::size_of::<u32>() as u64 as i32
            * 8
            - _ft.leading_zeros() as i32;
        if ftb > 8 {
            let mut t: u32 = 0;
            ftb -= 8;
            ft = (_ft >> ftb).wrapping_add(1);
            s = ec_decode(_this, ft);
            ec_dec_update(
                _this,
                s,
                s.wrapping_add(1),
                ft,
            );
            t = s << ftb | ec_dec_bits(_this, ftb as u32);
            if t <= _ft {
                return t;
            }
            (*_this).error = 1;
            return _ft;
        } else {
            _ft = _ft.wrapping_add(1);
            s = ec_decode(_this, _ft);
            ec_dec_update(
                _this,
                s,
                s.wrapping_add(1),
                _ft,
            );
            return s;
        };
    }
        pub unsafe fn ec_dec_bits(mut _this: &mut ec_dec, mut _bits: u32) -> u32 {
        let mut window: ec_window = 0;
        let mut available: i32 = 0;
        let mut ret: u32 = 0;
        window = (*_this).end_window;
        available = (*_this).nend_bits;
        if (available as u32) < _bits {
            loop {
                window |= (ec_read_byte_from_end(_this) as ec_window) << available;
                available += 8;
                if !(available
                    <= ::core::mem::size_of::<ec_window>() as u64 as i32
                        * 8
                        - 8)
                {
                    break;
                }
            }
        }
        ret = window & ((1) << _bits).wrapping_sub(1 as u32);
        window >>= _bits;
        available = (available as u32).wrapping_sub(_bits) as i32 as i32;
        (*_this).end_window = window;
        (*_this).nend_bits = available;
        (*_this).nbits_total = ((*_this).nbits_total as u32).wrapping_add(_bits)
            as i32 as i32;
        return ret;
    }
    use super::entcode_h::{celt_udiv, ec_dec, ec_window};
}
pub mod entcode_c {
        pub unsafe fn ec_tell_frac(mut _this: *mut ec_ctx) -> u32 {
        pub static mut correction: [u32; 8] = [
            35733,
            38967,
            42495,
            46340,
            50535,
            55109,
            60097,
            65535,
        ];
        let mut nbits: u32 = 0;
        let mut r: u32 = 0;
        let mut l: i32 = 0;
        let mut b: u32 = 0;
        nbits = ((*_this).nbits_total << 3) as u32;
        l = ::core::mem::size_of::<u32>() as u64 as i32
            * 8
            - ((*_this).rng).leading_zeros() as i32;
        r = (*_this).rng >> l - 16;
        b = (r >> 12).wrapping_sub(8);
        b = b.wrapping_add((r > correction[b as usize]) as i32 as u32);
        l = ((l << 3) as u32).wrapping_add(b) as i32;
        return nbits.wrapping_sub(l as u32);
    }
    use super::entcode_h::ec_ctx;
}
pub mod laplace_c {
        pub unsafe fn ec_laplace_get_freq1(
        mut fs0: u32,
        mut decay: i32,
    ) -> u32 {
        let mut ft: u32 = 0;
        ft = ((32768
            - ((1) << 0) * (2 * 16))
            as u32)
            .wrapping_sub(fs0);
        return ft.wrapping_mul((16384 - decay) as u32)
            >> 15;
    }
        pub unsafe fn ec_laplace_encode(
        mut enc: *mut ec_enc,
        mut value: *mut i32,
        mut fs: u32,
        mut decay: i32,
    ) {
        let mut fl: u32 = 0;
        let mut val: i32 = *value;
        fl = 0;
        if val != 0 {
            let mut s: i32 = 0;
            let mut i: i32 = 0;
            s = -((val < 0) as i32);
            val = val + s ^ s;
            fl = fs;
            fs = ec_laplace_get_freq1(fs, decay);
            i = 1;
            while fs > 0 && i < val {
                fs = fs.wrapping_mul(2);
                fl = fl.wrapping_add(fs.wrapping_add(
                    (2 * ((1) << 0)) as u32,
                ));
                fs = fs.wrapping_mul(decay as u32) >> 15;
                i += 1;
            }
            if fs == 0 {
                let mut di: i32 = 0;
                let mut ndi_max: i32 = 0;
                ndi_max = ((32768)
                    .wrapping_sub(fl)
                    .wrapping_add(((1) << 0) as u32)
                    .wrapping_sub(1)
                    >> 0) as i32;
                ndi_max = ndi_max - s >> 1;
                di = if val - i < ndi_max - 1 {
                    val - i
                } else {
                    ndi_max - 1
                };
                fl = fl.wrapping_add(
                    ((2 * di + 1 + s)
                        * ((1) << 0))
                        as u32,
                );
                fs = if (((1) << 0) as u32)
                    < (32768).wrapping_sub(fl)
                {
                    ((1) << 0) as u32
                } else {
                    (32768).wrapping_sub(fl)
                };
                *value = i + di + s ^ s;
            } else {
                fs = fs.wrapping_add(((1) << 0) as u32);
                fl = fl.wrapping_add(fs & !s as u32);
            }
        }
        ec_encode_bin(
            enc,
            fl,
            fl.wrapping_add(fs),
            15,
        );
    }
        pub unsafe fn ec_laplace_decode(
        mut dec: &mut ec_dec,
        mut fs: u32,
        mut decay: i32,
    ) -> i32 {
        let mut val: i32 = 0;
        let mut fl: u32 = 0;
        let mut fm: u32 = 0;
        fm = ec_decode_bin(dec, 15);
        fl = 0;
        if fm >= fs {
            val += 1;
            fl = fs;
            fs = (ec_laplace_get_freq1(fs, decay))
                .wrapping_add(((1) << 0) as u32);
            while fs > ((1) << 0) as u32
                && fm >= fl.wrapping_add((2).wrapping_mul(fs))
            {
                fs = fs.wrapping_mul(2);
                fl = fl.wrapping_add(fs);
                fs = fs
                    .wrapping_sub(
                        (2 * ((1) << 0))
                            as u32,
                    )
                    .wrapping_mul(decay as u32)
                    >> 15;
                fs = fs.wrapping_add(((1) << 0) as u32);
                val += 1;
            }
            if fs <= ((1) << 0) as u32 {
                let mut di: i32 = 0;
                di = (fm.wrapping_sub(fl) >> 0 + 1) as i32;
                val += di;
                fl = fl.wrapping_add(
                    (2 * di * ((1) << 0))
                        as u32,
                );
            }
            if fm < fl.wrapping_add(fs) {
                val = -val;
            } else {
                fl = fl.wrapping_add(fs);
            }
        }
        ec_dec_update(
            dec,
            fl,
            if fl.wrapping_add(fs) < 32768 {
                fl.wrapping_add(fs)
            } else {
                32768
            },
            32768,
        );
        return val;
    }
    use super::entcode_h::{ec_dec, ec_enc};
    use super::entdec_c::{ec_dec_update, ec_decode_bin};
    use super::entenc_c::ec_encode_bin;
}
pub use self::entcode_c::ec_tell_frac;
pub use self::entcode_h::{
    celt_udiv, ec_ctx, ec_dec, ec_enc, ec_get_buffer, ec_range_bytes, ec_window,
};
pub use self::entdec_c::{
    ec_dec_bit_logp, ec_dec_bits, ec_dec_icdf, ec_dec_init, ec_dec_normalize, ec_dec_uint,
    ec_dec_update, ec_decode, ec_decode_bin, ec_read_byte, ec_read_byte_from_end,
};
pub use self::entenc_c::{
    ec_enc_bit_logp, ec_enc_bits, ec_enc_carry_out, ec_enc_done, ec_enc_icdf, ec_enc_init,
    ec_enc_normalize, ec_enc_patch_initial_bits, ec_enc_shrink, ec_enc_uint, ec_encode,
    ec_encode_bin, ec_write_byte, ec_write_byte_at_end,
};
pub use self::laplace_c::{ec_laplace_decode, ec_laplace_encode, ec_laplace_get_freq1};
pub use self::stddef_h::size_t;

use self::stdio_h::{fprintf, stderr};
use self::stdlib_h::{free, malloc, rand};
pub use self::struct_FILE_h::{_IO_codecvt, _IO_lock_t, _IO_marker, _IO_wide_data, _IO_FILE};

pub use self::FILE_h::FILE;
use crate::externs::{memmove, memset};
pub unsafe fn ec_laplace_get_start_freq(mut decay: i32) -> i32 {
    let mut ft: u32 = (32768
        - ((1) << 0)
            * (2 * 16 + 1))
        as u32;
    let mut fs: i32 =
        ft.wrapping_mul((16384 - decay) as u32)
            .wrapping_div((16384 + decay) as u32) as i32;
    return fs + ((1) << 0);
}
unsafe fn main_0() -> i32 {
    let mut i: i32 = 0;
    let mut ret: i32 = 0;
    let mut enc: ec_enc = ec_enc {
        buf: 0 as *mut u8,
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
    let mut dec: ec_dec = ec_enc {
        buf: 0 as *mut u8,
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
    let mut ptr: *mut u8 = 0 as *mut u8;
    let mut val: [i32; 10000] = [0; 10000];
    let mut decay: [i32; 10000] = [0; 10000];
    ptr = malloc(40000) as *mut u8;
    ec_enc_init(&mut enc, ptr, 40000);
    val[0 as usize] = 3;
    decay[0 as usize] = 6000;
    val[1 as usize] = 0;
    decay[1 as usize] = 5800;
    val[2 as usize] = -1;
    decay[2 as usize] = 5600;
    i = 3;
    while i < 10000 {
        val[i as usize] = rand() % 15 - 7;
        decay[i as usize] = rand() % 11000 + 5000;
        i += 1;
    }
    i = 0;
    while i < 10000 {
        ec_laplace_encode(
            &mut enc,
            &mut *val.as_mut_ptr().offset(i as isize),
            ec_laplace_get_start_freq(decay[i as usize]) as u32,
            decay[i as usize],
        );
        i += 1;
    }
    ec_enc_done(&mut enc);
    ec_dec_init(dec, ec_get_buffer(&mut enc), ec_range_bytes(&mut enc));
    i = 0;
    while i < 10000 {
        let mut d: i32 = ec_laplace_decode(
            dec,
            ec_laplace_get_start_freq(decay[i as usize]) as u32,
            decay[i as usize],
        );
        if d != val[i as usize] {
            fprintf(
                stderr(),
                b"Got %d instead of %d\n\0" as *const u8 as *const i8,
                d,
                val[i as usize],
            );
            ret = 1;
        }
        i += 1;
    }
    free(ptr as *mut core::ffi::c_void);
    return ret;
}
pub fn main() {
    unsafe { ::std::process::exit(main_0() as i32) }
}
