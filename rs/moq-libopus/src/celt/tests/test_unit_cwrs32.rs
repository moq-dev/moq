pub mod stddef_h {
        pub type size_t = u64;
}
pub mod arch_h {
        pub type opus_val32 = f32;
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
        pub unsafe fn celt_udiv(mut n: u32, mut d: u32) -> u32 {
        return n.wrapping_div(d);
    }
}
pub mod stdio_h {
    use super::FILE_h::FILE;
    {
                pub static mut stderr: *mut FILE;
                pub fn fprintf(_: *mut FILE, _: *const i8, _: ...) -> i32;
                pub fn printf(_: *const i8, _: ...) -> i32;
    }
}
pub mod entenc_c {
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
                    let ref mut fresh0 = *((*_this).buf).offset(
                        ((*_this).storage)
                            .wrapping_sub((*_this).end_offs)
                            .wrapping_sub(1)
                            as isize,
                    );
                    *fresh0 = (*fresh0 as i32 | window as u8 as i32)
                        as u8;
                }
            }
        }
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
        pub unsafe fn ec_write_byte(
        mut _this: *mut ec_enc,
        mut _value: u32,
    ) -> i32 {
        if ((*_this).offs).wrapping_add((*_this).end_offs) >= (*_this).storage {
            return -1;
        }
        let fresh1 = (*_this).offs;
        (*_this).offs = ((*_this).offs).wrapping_add(1);
        *((*_this).buf).offset(fresh1 as isize) = _value as u8;
        return 0;
    }
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
    use super::entcode_h::{celt_udiv, ec_enc, ec_window};
    use crate::externs::{memmove, memset};
}
pub mod stdlib_h {
    {
                pub fn abs(_: i32) -> i32;
    }
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
pub mod cwrs_c {
        pub unsafe fn log2_frac(mut val: u32, mut frac: i32) -> i32 {
        let mut l: i32 = 0;
        l = ::core::mem::size_of::<u32>() as u64 as i32
            * 8
            - val.leading_zeros() as i32;
        if val & val.wrapping_sub(1) != 0 {
            if l > 16 {
                val = (val.wrapping_sub(1) >> l - 16)
                    .wrapping_add(1);
            } else {
                val <<= 16 - l;
            }
            l = (l - 1) << frac;
            loop {
                let mut b: i32 = 0;
                b = (val >> 16) as i32;
                l += b << frac;
                val = val.wrapping_add(b as u32) >> b;
                val = val
                    .wrapping_mul(val)
                    .wrapping_add(0x7fff)
                    >> 15;
                let fresh3 = frac;
                frac = frac - 1;
                if !(fresh3 > 0) {
                    break;
                }
            }
            return l + (val > 0x8000) as i32;
        } else {
            return (l - 1) << frac;
        };
    }
        pub static mut CELT_PVQ_U_DATA: [u32; 1488] = [
        1,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        1,
        3,
        5,
        7,
        9,
        11,
        13,
        15,
        17,
        19,
        21,
        23,
        25,
        27,
        29,
        31,
        33,
        35,
        37,
        39,
        41,
        43,
        45,
        47,
        49,
        51,
        53,
        55,
        57,
        59,
        61,
        63,
        65,
        67,
        69,
        71,
        73,
        75,
        77,
        79,
        81,
        83,
        85,
        87,
        89,
        91,
        93,
        95,
        97,
        99,
        101,
        103,
        105,
        107,
        109,
        111,
        113,
        115,
        117,
        119,
        121,
        123,
        125,
        127,
        129,
        131,
        133,
        135,
        137,
        139,
        141,
        143,
        145,
        147,
        149,
        151,
        153,
        155,
        157,
        159,
        161,
        163,
        165,
        167,
        169,
        171,
        173,
        175,
        177,
        179,
        181,
        183,
        185,
        187,
        189,
        191,
        193,
        195,
        197,
        199,
        201,
        203,
        205,
        207,
        209,
        211,
        213,
        215,
        217,
        219,
        221,
        223,
        225,
        227,
        229,
        231,
        233,
        235,
        237,
        239,
        241,
        243,
        245,
        247,
        249,
        251,
        253,
        255,
        257,
        259,
        261,
        263,
        265,
        267,
        269,
        271,
        273,
        275,
        277,
        279,
        281,
        283,
        285,
        287,
        289,
        291,
        293,
        295,
        297,
        299,
        301,
        303,
        305,
        307,
        309,
        311,
        313,
        315,
        317,
        319,
        321,
        323,
        325,
        327,
        329,
        331,
        333,
        335,
        337,
        339,
        341,
        343,
        345,
        347,
        349,
        351,
        353,
        355,
        357,
        359,
        361,
        363,
        365,
        367,
        369,
        371,
        373,
        375,
        377,
        379,
        381,
        383,
        385,
        387,
        389,
        391,
        393,
        395,
        397,
        399,
        401,
        403,
        405,
        407,
        409,
        411,
        413,
        415,
        13,
        25,
        41,
        61,
        85,
        113,
        145,
        181,
        221,
        265,
        313,
        365,
        421,
        481,
        545,
        613,
        685,
        761,
        841,
        925,
        1013,
        1105,
        1201,
        1301,
        1405,
        1513,
        1625,
        1741,
        1861,
        1985,
        2113,
        2245,
        2381,
        2521,
        2665,
        2813,
        2965,
        3121,
        3281,
        3445,
        3613,
        3785,
        3961,
        4141,
        4325,
        4513,
        4705,
        4901,
        5101,
        5305,
        5513,
        5725,
        5941,
        6161,
        6385,
        6613,
        6845,
        7081,
        7321,
        7565,
        7813,
        8065,
        8321,
        8581,
        8845,
        9113,
        9385,
        9661,
        9941,
        10225,
        10513,
        10805,
        11101,
        11401,
        11705,
        12013,
        12325,
        12641,
        12961,
        13285,
        13613,
        13945,
        14281,
        14621,
        14965,
        15313,
        15665,
        16021,
        16381,
        16745,
        17113,
        17485,
        17861,
        18241,
        18625,
        19013,
        19405,
        19801,
        20201,
        20605,
        21013,
        21425,
        21841,
        22261,
        22685,
        23113,
        23545,
        23981,
        24421,
        24865,
        25313,
        25765,
        26221,
        26681,
        27145,
        27613,
        28085,
        28561,
        29041,
        29525,
        30013,
        30505,
        31001,
        31501,
        32005,
        32513,
        33025,
        33541,
        34061,
        34585,
        35113,
        35645,
        36181,
        36721,
        37265,
        37813,
        38365,
        38921,
        39481,
        40045,
        40613,
        41185,
        41761,
        42341,
        42925,
        43513,
        44105,
        44701,
        45301,
        45905,
        46513,
        47125,
        47741,
        48361,
        48985,
        49613,
        50245,
        50881,
        51521,
        52165,
        52813,
        53465,
        54121,
        54781,
        55445,
        56113,
        56785,
        57461,
        58141,
        58825,
        59513,
        60205,
        60901,
        61601,
        62305,
        63013,
        63725,
        64441,
        65161,
        65885,
        66613,
        67345,
        68081,
        68821,
        69565,
        70313,
        71065,
        71821,
        72581,
        73345,
        74113,
        74885,
        75661,
        76441,
        77225,
        78013,
        78805,
        79601,
        80401,
        81205,
        82013,
        82825,
        83641,
        84461,
        85285,
        86113,
        63,
        129,
        231,
        377,
        575,
        833,
        1159,
        1561,
        2047,
        2625,
        3303,
        4089,
        4991,
        6017,
        7175,
        8473,
        9919,
        11521,
        13287,
        15225,
        17343,
        19649,
        22151,
        24857,
        27775,
        30913,
        34279,
        37881,
        41727,
        45825,
        50183,
        54809,
        59711,
        64897,
        70375,
        76153,
        82239,
        88641,
        95367,
        102425,
        109823,
        117569,
        125671,
        134137,
        142975,
        152193,
        161799,
        171801,
        182207,
        193025,
        204263,
        215929,
        228031,
        240577,
        253575,
        267033,
        280959,
        295361,
        310247,
        325625,
        341503,
        357889,
        374791,
        392217,
        410175,
        428673,
        447719,
        467321,
        487487,
        508225,
        529543,
        551449,
        573951,
        597057,
        620775,
        645113,
        670079,
        695681,
        721927,
        748825,
        776383,
        804609,
        833511,
        863097,
        893375,
        924353,
        956039,
        988441,
        1021567,
        1055425,
        1090023,
        1125369,
        1161471,
        1198337,
        1235975,
        1274393,
        1313599,
        1353601,
        1394407,
        1436025,
        1478463,
        1521729,
        1565831,
        1610777,
        1656575,
        1703233,
        1750759,
        1799161,
        1848447,
        1898625,
        1949703,
        2001689,
        2054591,
        2108417,
        2163175,
        2218873,
        2275519,
        2333121,
        2391687,
        2451225,
        2511743,
        2573249,
        2635751,
        2699257,
        2763775,
        2829313,
        2895879,
        2963481,
        3032127,
        3101825,
        3172583,
        3244409,
        3317311,
        3391297,
        3466375,
        3542553,
        3619839,
        3698241,
        3777767,
        3858425,
        3940223,
        4023169,
        4107271,
        4192537,
        4278975,
        4366593,
        4455399,
        4545401,
        4636607,
        4729025,
        4822663,
        4917529,
        5013631,
        5110977,
        5209575,
        5309433,
        5410559,
        5512961,
        5616647,
        5721625,
        5827903,
        5935489,
        6044391,
        6154617,
        6266175,
        6379073,
        6493319,
        6608921,
        6725887,
        6844225,
        6963943,
        7085049,
        7207551,
        7331457,
        7456775,
        7583513,
        7711679,
        7841281,
        7972327,
        8104825,
        8238783,
        8374209,
        8511111,
        8649497,
        8789375,
        8930753,
        9073639,
        9218041,
        9363967,
        9511425,
        9660423,
        9810969,
        9963071,
        10116737,
        10271975,
        10428793,
        10587199,
        10747201,
        10908807,
        11072025,
        11236863,
        11403329,
        11571431,
        11741177,
        11912575,
        321,
        681,
        1289,
        2241,
        3649,
        5641,
        8361,
        11969,
        16641,
        22569,
        29961,
        39041,
        50049,
        63241,
        78889,
        97281,
        118721,
        143529,
        172041,
        204609,
        241601,
        283401,
        330409,
        383041,
        441729,
        506921,
        579081,
        658689,
        746241,
        842249,
        947241,
        1061761,
        1186369,
        1321641,
        1468169,
        1626561,
        1797441,
        1981449,
        2179241,
        2391489,
        2618881,
        2862121,
        3121929,
        3399041,
        3694209,
        4008201,
        4341801,
        4695809,
        5071041,
        5468329,
        5888521,
        6332481,
        6801089,
        7295241,
        7815849,
        8363841,
        8940161,
        9545769,
        10181641,
        10848769,
        11548161,
        12280841,
        13047849,
        13850241,
        14689089,
        15565481,
        16480521,
        17435329,
        18431041,
        19468809,
        20549801,
        21675201,
        22846209,
        24064041,
        25329929,
        26645121,
        28010881,
        29428489,
        30899241,
        32424449,
        34005441,
        35643561,
        37340169,
        39096641,
        40914369,
        42794761,
        44739241,
        46749249,
        48826241,
        50971689,
        53187081,
        55473921,
        57833729,
        60268041,
        62778409,
        65366401,
        68033601,
        70781609,
        73612041,
        76526529,
        79526721,
        82614281,
        85790889,
        89058241,
        92418049,
        95872041,
        99421961,
        103069569,
        106816641,
        110664969,
        114616361,
        118672641,
        122835649,
        127107241,
        131489289,
        135983681,
        140592321,
        145317129,
        150160041,
        155123009,
        160208001,
        165417001,
        170752009,
        176215041,
        181808129,
        187533321,
        193392681,
        199388289,
        205522241,
        211796649,
        218213641,
        224775361,
        231483969,
        238341641,
        245350569,
        252512961,
        259831041,
        267307049,
        274943241,
        282741889,
        290705281,
        298835721,
        307135529,
        315607041,
        324252609,
        333074601,
        342075401,
        351257409,
        360623041,
        370174729,
        379914921,
        389846081,
        399970689,
        410291241,
        420810249,
        431530241,
        442453761,
        453583369,
        464921641,
        476471169,
        488234561,
        500214441,
        512413449,
        524834241,
        537479489,
        550351881,
        563454121,
        576788929,
        590359041,
        604167209,
        618216201,
        632508801,
        647047809,
        661836041,
        676876329,
        692171521,
        707724481,
        723538089,
        739615241,
        755958849,
        772571841,
        789457161,
        806617769,
        824056641,
        841776769,
        859781161,
        878072841,
        896654849,
        915530241,
        934702089,
        954173481,
        973947521,
        994027329,
        1014416041,
        1035116809,
        1056132801,
        1077467201,
        1099123209,
        1121104041,
        1143412929,
        1166053121,
        1189027881,
        1212340489,
        1235994241,
        1683,
        3653,
        7183,
        13073,
        22363,
        36365,
        56695,
        85305,
        124515,
        177045,
        246047,
        335137,
        448427,
        590557,
        766727,
        982729,
        1244979,
        1560549,
        1937199,
        2383409,
        2908411,
        3522221,
        4235671,
        5060441,
        6009091,
        7095093,
        8332863,
        9737793,
        11326283,
        13115773,
        15124775,
        17372905,
        19880915,
        22670725,
        25765455,
        29189457,
        32968347,
        37129037,
        41699767,
        46710137,
        52191139,
        58175189,
        64696159,
        71789409,
        79491819,
        87841821,
        96879431,
        106646281,
        117185651,
        128542501,
        140763503,
        153897073,
        167993403,
        183104493,
        199284183,
        216588185,
        235074115,
        254801525,
        275831935,
        298228865,
        322057867,
        347386557,
        374284647,
        402823977,
        433078547,
        465124549,
        499040399,
        534906769,
        572806619,
        612825229,
        655050231,
        699571641,
        746481891,
        795875861,
        847850911,
        902506913,
        959946283,
        1020274013,
        1083597703,
        1150027593,
        1219676595,
        1292660325,
        1369097135,
        1449108145,
        1532817275,
        1620351277,
        1711839767,
        1807415257,
        1907213187,
        2011371957,
        2120032959,
        2233340609 as u32,
        2351442379 as u32,
        2474488829 as u32,
        2602633639 as u32,
        2736033641 as u32,
        2874848851 as u32,
        3019242501 as u32,
        3169381071 as u32,
        3325434321 as u32,
        3487575323 as u32,
        3655980493 as u32,
        3830829623 as u32,
        4012305913 as u32,
        8989,
        19825,
        40081,
        75517,
        134245,
        227305,
        369305,
        579125,
        880685,
        1303777,
        1884961,
        2668525,
        3707509,
        5064793,
        6814249,
        9041957,
        11847485,
        15345233,
        19665841,
        24957661,
        31388293,
        39146185,
        48442297,
        59511829,
        72616013,
        88043969,
        106114625,
        127178701,
        151620757,
        179861305,
        212358985,
        249612805,
        292164445,
        340600625,
        395555537,
        457713341,
        527810725,
        606639529,
        695049433,
        793950709,
        904317037,
        1027188385,
        1163673953,
        1314955181,
        1482288821,
        1667010073,
        1870535785,
        2094367717,
        2340095869 as u32,
        2609401873 as u32,
        2904062449 as u32,
        3225952925 as u32,
        3577050821 as u32,
        3959439497 as u32,
        48639,
        108545,
        224143,
        433905,
        795455,
        1392065,
        2340495,
        3800305,
        5984767,
        9173505,
        13726991,
        20103025,
        28875327,
        40754369,
        56610575,
        77500017,
        104692735,
        139703809,
        184327311,
        240673265,
        311207743,
        398796225,
        506750351,
        638878193,
        799538175,
        993696769,
        1226990095,
        1505789553,
        1837271615,
        2229491905 as u32,
        2691463695 as u32,
        3233240945 as u32,
        3866006015 as u32,
        265729,
        598417,
        1256465,
        2485825,
        4673345,
        8405905,
        14546705,
        24331777,
        39490049,
        62390545,
        96220561,
        145198913,
        214828609,
        312193553,
        446304145,
        628496897,
        872893441,
        1196924561,
        1621925137,
        2173806145 as u32,
        2883810113 as u32,
        1462563,
        3317445,
        7059735,
        14218905,
        27298155,
        50250765,
        89129247,
        152951073,
        254831667,
        413442773,
        654862247,
        1014889769,
        1541911931,
        2300409629 as u32,
        3375210671 as u32,
        8097453,
        18474633,
        39753273,
        81270333,
        158819253,
        298199265,
        540279585,
        948062325,
        1616336765,
        2684641785 as u32,
        45046719,
        103274625,
        224298231,
        464387817,
        921406335,
        1759885185,
        3248227095 as u32,
        251595969,
        579168825,
        1267854873,
        2653649025 as u32,
        1409933619,
    ];
        pub static mut CELT_PVQ_U_ROW: [*const u32; 15] = unsafe { [
            CELT_PVQ_U_DATA.as_ptr().offset(0 as isize),
            CELT_PVQ_U_DATA.as_ptr().offset(208 as isize),
            CELT_PVQ_U_DATA.as_ptr().offset(415 as isize),
            CELT_PVQ_U_DATA.as_ptr().offset(621 as isize),
            CELT_PVQ_U_DATA.as_ptr().offset(826 as isize),
            CELT_PVQ_U_DATA
                .as_ptr()
                .offset(1030 as isize),
            CELT_PVQ_U_DATA
                .as_ptr()
                .offset(1233 as isize),
            CELT_PVQ_U_DATA
                .as_ptr()
                .offset(1336 as isize),
            CELT_PVQ_U_DATA
                .as_ptr()
                .offset(1389 as isize),
            CELT_PVQ_U_DATA
                .as_ptr()
                .offset(1421 as isize),
            CELT_PVQ_U_DATA
                .as_ptr()
                .offset(1441 as isize),
            CELT_PVQ_U_DATA
                .as_ptr()
                .offset(1455 as isize),
            CELT_PVQ_U_DATA
                .as_ptr()
                .offset(1464 as isize),
            CELT_PVQ_U_DATA
                .as_ptr()
                .offset(1470 as isize),
            CELT_PVQ_U_DATA
                .as_ptr()
                .offset(1473 as isize),
        ] };
        pub unsafe fn get_required_bits(
        mut _bits: *mut i16,
        mut _n: i32,
        mut _maxk: i32,
        mut _frac: i32,
    ) {
        let mut k: i32 = 0;
        *_bits.offset(0 as isize) = 0;
        k = 1;
        while k <= _maxk {
            *_bits.offset(k as isize) = log2_frac(
                (*(CELT_PVQ_U_ROW[(if _n < k { _n } else { k }) as usize])
                    .offset((if _n > k { _n } else { k }) as isize))
                .wrapping_add(
                    *(CELT_PVQ_U_ROW[(if _n < k + 1 {
                        _n
                    } else {
                        k + 1
                    }) as usize])
                        .offset(
                            (if _n > k + 1 {
                                _n
                            } else {
                                k + 1
                            }) as isize,
                        ),
                ),
                _frac,
            ) as i16;
            k += 1;
        }
    }
        pub unsafe fn icwrs(mut _n: i32, mut _y: *const i32) -> u32 {
        let mut i: u32 = 0;
        let mut j: i32 = 0;
        let mut k: i32 = 0;
        j = _n - 1;
        i = (*_y.offset(j as isize) < 0) as i32 as u32;
        k = abs(*_y.offset(j as isize));
        loop {
            j -= 1;
            i = (i as u32).wrapping_add(
                *(CELT_PVQ_U_ROW[(if _n - j < k { _n - j } else { k }) as usize])
                    .offset((if _n - j > k { _n - j } else { k }) as isize),
            ) as u32 as u32;
            k += abs(*_y.offset(j as isize));
            if *_y.offset(j as isize) < 0 {
                i = (i as u32).wrapping_add(
                    *(CELT_PVQ_U_ROW[(if _n - j < k + 1 {
                        _n - j
                    } else {
                        k + 1
                    }) as usize])
                        .offset(
                            (if _n - j > k + 1 {
                                _n - j
                            } else {
                                k + 1
                            }) as isize,
                        ),
                ) as u32 as u32;
            }
            if !(j > 0) {
                break;
            }
        }
        return i;
    }
        pub unsafe fn encode_pulses(
        mut _y: *const i32,
        mut _n: i32,
        mut _k: i32,
        mut _enc: *mut ec_enc,
    ) {
        ec_enc_uint(
            _enc,
            icwrs(_n, _y),
            (*(CELT_PVQ_U_ROW[(if _n < _k { _n } else { _k }) as usize])
                .offset((if _n > _k { _n } else { _k }) as isize))
            .wrapping_add(
                *(CELT_PVQ_U_ROW[(if _n < _k + 1 {
                    _n
                } else {
                    _k + 1
                }) as usize])
                    .offset(
                        (if _n > _k + 1 {
                            _n
                        } else {
                            _k + 1
                        }) as isize,
                    ),
            ),
        );
    }
        pub unsafe fn cwrsi(
        mut _n: i32,
        mut _k: i32,
        mut _i: u32,
        mut _y: *mut i32,
    ) -> opus_val32 {
        let mut p: u32 = 0;
        let mut s: i32 = 0;
        let mut k0: i32 = 0;
        let mut val: i16 = 0;
        let mut yy: opus_val32 = 0 as opus_val32;
        while _n > 2 {
            let mut q: u32 = 0;
            if _k >= _n {
                let mut row: *const u32 = 0 as *const u32;
                row = CELT_PVQ_U_ROW[_n as usize];
                p = *row.offset((_k + 1) as isize);
                s = -((_i >= p) as i32);
                _i = (_i as u32).wrapping_sub(p & s as u32) as u32 as u32;
                k0 = _k;
                q = *row.offset(_n as isize);
                if q > _i {
                    _k = _n;
                    loop {
                        _k -= 1;
                        p = *(CELT_PVQ_U_ROW[_k as usize]).offset(_n as isize);
                        if !(p > _i) {
                            break;
                        }
                    }
                } else {
                    p = *row.offset(_k as isize);
                    while p > _i {
                        _k -= 1;
                        p = *row.offset(_k as isize);
                    }
                }
                _i = (_i as u32).wrapping_sub(p) as u32 as u32;
                val = (k0 - _k + s ^ s) as i16;
                let fresh4 = _y;
                _y = _y.offset(1);
                *fresh4 = val as i32;
                yy = yy + val as opus_val32 * val as opus_val32;
            } else {
                p = *(CELT_PVQ_U_ROW[_k as usize]).offset(_n as isize);
                q = *(CELT_PVQ_U_ROW[(_k + 1) as usize]).offset(_n as isize);
                if p <= _i && _i < q {
                    _i = (_i as u32).wrapping_sub(p) as u32 as u32;
                    let fresh5 = _y;
                    _y = _y.offset(1);
                    *fresh5 = 0;
                } else {
                    s = -((_i >= q) as i32);
                    _i = (_i as u32).wrapping_sub(q & s as u32) as u32 as u32;
                    k0 = _k;
                    loop {
                        _k -= 1;
                        p = *(CELT_PVQ_U_ROW[_k as usize]).offset(_n as isize);
                        if !(p > _i) {
                            break;
                        }
                    }
                    _i = (_i as u32).wrapping_sub(p) as u32 as u32;
                    val = (k0 - _k + s ^ s) as i16;
                    let fresh6 = _y;
                    _y = _y.offset(1);
                    *fresh6 = val as i32;
                    yy = yy + val as opus_val32 * val as opus_val32;
                }
            }
            _n -= 1;
        }
        p = (2 * _k + 1) as u32;
        s = -((_i >= p) as i32);
        _i = (_i as u32).wrapping_sub(p & s as u32) as u32 as u32;
        k0 = _k;
        _k = (_i.wrapping_add(1) >> 1) as i32;
        if _k != 0 {
            _i = (_i as u32)
                .wrapping_sub((2 * _k - 1) as u32)
                as u32 as u32;
        }
        val = (k0 - _k + s ^ s) as i16;
        let fresh7 = _y;
        _y = _y.offset(1);
        *fresh7 = val as i32;
        yy = yy + val as opus_val32 * val as opus_val32;
        s = -(_i as i32);
        val = (_k + s ^ s) as i16;
        *_y = val as i32;
        yy = yy + val as opus_val32 * val as opus_val32;
        return yy;
    }
        pub unsafe fn decode_pulses(
        mut _y: *mut i32,
        mut _n: i32,
        mut _k: i32,
        mut _dec: &mut ec_dec,
    ) -> opus_val32 {
        return cwrsi(
            _n,
            _k,
            ec_dec_uint(
                _dec,
                (*(CELT_PVQ_U_ROW[(if _n < _k { _n } else { _k }) as usize])
                    .offset((if _n > _k { _n } else { _k }) as isize))
                .wrapping_add(
                    *(CELT_PVQ_U_ROW[(if _n < _k + 1 {
                        _n
                    } else {
                        _k + 1
                    }) as usize])
                        .offset(
                            (if _n > _k + 1 {
                                _n
                            } else {
                                _k + 1
                            }) as isize,
                        ),
                ),
            ),
            _y,
        );
    }
    use super::arch_h::opus_val32;
    use super::entcode_h::{ec_dec, ec_enc};
    use super::entdec_c::ec_dec_uint;
    use super::entenc_c::ec_enc_uint;
    use super::stdlib_h::abs;
}
pub mod mathops_c {
        pub unsafe fn isqrt32(mut _val: u32) -> u32 {
        let mut b: u32 = 0;
        let mut g: u32 = 0;
        let mut bshift: i32 = 0;
        g = 0;
        bshift = ::core::mem::size_of::<u32>() as u64 as i32
            * 8
            - _val.leading_zeros() as i32
            - 1
            >> 1;
        b = (1 as u32) << bshift;
        loop {
            let mut t: u32 = 0;
            t = (g << 1).wrapping_add(b) << bshift;
            if t <= _val {
                g = g.wrapping_add(b);
                _val = (_val as u32).wrapping_sub(t) as u32 as u32;
            }
            b >>= 1;
            bshift -= 1;
            if !(bshift >= 0) {
                break;
            }
        }
        return g;
    }
}
pub mod rate_h {
    #[inline]
        pub unsafe fn get_pulses(mut i: i32) -> i32 {
        return if i < 8 {
            i
        } else {
            8 + (i & 7) << (i >> 3) - 1
        };
    }
}
pub use self::arch_h::opus_val32;
pub use self::cwrs_c::{
    cwrsi, decode_pulses, encode_pulses, get_required_bits, icwrs, log2_frac, CELT_PVQ_U_DATA,
    CELT_PVQ_U_ROW,
};
pub use self::entcode_c::ec_tell_frac;
pub use self::entcode_h::{celt_udiv, ec_ctx, ec_dec, ec_enc, ec_window};
pub use self::entdec_c::{
    ec_dec_bit_logp, ec_dec_bits, ec_dec_icdf, ec_dec_init, ec_dec_normalize, ec_dec_uint,
    ec_dec_update, ec_decode, ec_decode_bin, ec_read_byte, ec_read_byte_from_end,
};
pub use self::entenc_c::{
    ec_enc_bit_logp, ec_enc_bits, ec_enc_carry_out, ec_enc_done, ec_enc_icdf, ec_enc_init,
    ec_enc_normalize, ec_enc_patch_initial_bits, ec_enc_shrink, ec_enc_uint, ec_encode,
    ec_encode_bin, ec_write_byte, ec_write_byte_at_end,
};
pub use self::mathops_c::isqrt32;
pub use self::rate_h::get_pulses;
pub use self::stddef_h::size_t;

use self::stdio_h::{fprintf, printf, stderr};
use self::stdlib_h::abs;
pub use self::struct_FILE_h::{_IO_codecvt, _IO_lock_t, _IO_marker, _IO_wide_data, _IO_FILE};

pub use self::FILE_h::FILE;
use crate::externs::{memmove, memset};
static mut pn: [i32; 22] = [
    2,
    3,
    4,
    6,
    8,
    9,
    11,
    12,
    16,
    18,
    22,
    24,
    32,
    36,
    44,
    48,
    64,
    72,
    88,
    96,
    144,
    176,
];
static mut pkmax: [i32; 22] = [
    128,
    128,
    128,
    88,
    36,
    26,
    18,
    16,
    12,
    11,
    9,
    9,
    7,
    7,
    6,
    6,
    5,
    5,
    5,
    5,
    4,
    4,
];
unsafe fn main_0() -> i32 {
    let mut t: i32 = 0;
    let mut n: i32 = 0;
    t = 0;
    while t < 22 {
        let mut pseudo: i32 = 0;
        n = pn[t as usize];
        pseudo = 1;
        while pseudo < 41 {
            let mut k: i32 = 0;
            let mut inc: u32 = 0;
            let mut nc: u32 = 0;
            let mut i: u32 = 0;
            k = get_pulses(pseudo);
            if k > pkmax[t as usize] {
                break;
            }
            printf(
                b"Testing CWRS with N=%i, K=%i...\n\0" as *const u8 as *const i8,
                n,
                k,
            );
            nc = (*(CELT_PVQ_U_ROW[(if n < k { n } else { k }) as usize])
                .offset((if n > k { n } else { k }) as isize))
            .wrapping_add(
                *(CELT_PVQ_U_ROW[(if n < k + 1 {
                    n
                } else {
                    k + 1
                }) as usize])
                    .offset(
                        (if n > k + 1 {
                            n
                        } else {
                            k + 1
                        }) as isize,
                    ),
            );
            inc = nc.wrapping_div(20000);
            if inc < 1 {
                inc = 1;
            }
            i = 0;
            while i < nc {
                let mut y: [i32; 240] = [0; 240];
                let mut sy: i32 = 0;
                let mut v: u32 = 0;
                let mut ii: u32 = 0;
                let mut j: i32 = 0;
                cwrsi(n, k, i, y.as_mut_ptr());
                sy = 0;
                j = 0;
                while j < n {
                    sy += abs(y[j as usize]);
                    j += 1;
                }
                if sy != k {
                    fprintf(
                        stderr(),
                        b"N=%d Pulse count mismatch in cwrsi (%d!=%d).\n\0" as *const u8
                            as *const i8,
                        n,
                        sy,
                        k,
                    );
                    return 99;
                }
                ii = icwrs(n, y.as_mut_ptr());
                v = (*(CELT_PVQ_U_ROW[(if n < k { n } else { k }) as usize])
                    .offset((if n > k { n } else { k }) as isize))
                .wrapping_add(
                    *(CELT_PVQ_U_ROW[(if n < k + 1 {
                        n
                    } else {
                        k + 1
                    }) as usize])
                        .offset(
                            (if n > k + 1 {
                                n
                            } else {
                                k + 1
                            }) as isize,
                        ),
                );
                if ii != i {
                    fprintf(
                        stderr(),
                        b"Combination-index mismatch (%lu!=%lu).\n\0" as *const u8
                            as *const i8,
                        ii as i64,
                        i as i64,
                    );
                    return 1;
                }
                if v != nc {
                    fprintf(
                        stderr(),
                        b"Combination count mismatch (%lu!=%lu).\n\0" as *const u8
                            as *const i8,
                        v as i64,
                        nc as i64,
                    );
                    return 2;
                }
                i = (i as u32).wrapping_add(inc) as u32 as u32;
            }
            pseudo += 1;
        }
        t += 1;
    }
    return 0;
}
pub fn main() {
    unsafe { ::std::process::exit(main_0() as i32) }
}