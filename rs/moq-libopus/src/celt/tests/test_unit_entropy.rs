pub mod stddef_h {
        pub type size_t = u64;
}
pub mod time_t_h {
        pub type time_t = __time_t;
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
        pub unsafe fn ec_tell(mut _this: *mut ec_ctx) -> i32 {
        return (*_this).nbits_total
            - (::core::mem::size_of::<u32>() as u64 as i32
                * 8
                - ((*_this).rng).leading_zeros() as i32);
    }
    #[inline]
        pub unsafe fn celt_udiv(mut n: u32, mut d: u32) -> u32 {
        return n.wrapping_div(d);
    }
}
pub mod stdlib_h {
    #[inline]
        pub unsafe fn atoi(mut __nptr: *const i8) -> i32 {
        return strtol(
            __nptr,
            0 as *mut core::ffi::c_void as *mut *mut i8,
            10,
        ) as i32;
    }
    {
                pub fn strtol(
            _: *const i8,
            _: *mut *mut i8,
            _: i32,
        ) -> i64;
                pub fn rand() -> i32;
                pub fn srand(__seed: u32);
                pub fn malloc(_: u64) -> *mut core::ffi::c_void;
                pub fn free(_: *mut core::ffi::c_void);
                pub fn getenv(__name: *const i8) -> *mut i8;
    }
}
pub mod stdio_h {
    use super::FILE_h::FILE;
    {
                pub static mut stderr: *mut FILE;
                pub fn fprintf(_: *mut FILE, _: *const i8, _: ...) -> i32;
    }
}
pub mod mathcalls_h {
    {
                pub fn ldexp(_: f64, _: i32) -> f64;
                pub fn log(_: f64) -> f64;
    }
}
pub mod time_h {
    use super::time_t_h::time_t;
    {
                pub fn time(__timer: *mut time_t) -> time_t;
    }
}
pub mod entenc_c {
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
pub use self::entcode_c::ec_tell_frac;
pub use self::entcode_h::{celt_udiv, ec_ctx, ec_dec, ec_enc, ec_range_bytes, ec_tell, ec_window};
pub use self::entdec_c::{
    ec_dec_bit_logp, ec_dec_bits, ec_dec_icdf, ec_dec_init, ec_dec_normalize, ec_dec_uint,
    ec_dec_update, ec_decode, ec_decode_bin, ec_read_byte, ec_read_byte_from_end,
};
pub use self::entenc_c::{
    ec_enc_bit_logp, ec_enc_bits, ec_enc_carry_out, ec_enc_done, ec_enc_icdf, ec_enc_init,
    ec_enc_normalize, ec_enc_patch_initial_bits, ec_enc_shrink, ec_enc_uint, ec_encode,
    ec_encode_bin, ec_write_byte, ec_write_byte_at_end,
};
use self::mathcalls_h::{ldexp, log};
pub use self::stddef_h::size_t;

use self::stdio_h::{fprintf, stderr};
pub use self::stdlib_h::{atoi, free, getenv, malloc, rand, srand, strtol};
pub use self::struct_FILE_h::{_IO_codecvt, _IO_lock_t, _IO_marker, _IO_wide_data, _IO_FILE};
use self::time_h::time;
pub use self::time_t_h::time_t;

pub use self::FILE_h::FILE;
use crate::externs::{memmove, memset};
unsafe fn main_0(mut _argc: i32, mut _argv: *mut *mut i8) -> i32 {
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
    let mut nbits: i64 = 0;
    let mut nbits2: i64 = 0;
    let mut entropy: f64 = 0.;
    let mut ft: i32 = 0;
    let mut ftb: i32 = 0;
    let mut sz: i32 = 0;
    let mut i: i32 = 0;
    let mut ret: i32 = 0;
    let mut sym: u32 = 0;
    let mut seed: u32 = 0;
    let mut ptr: *mut u8 = 0 as *mut u8;
    let mut env_seed: *const i8 = 0 as *const i8;
    ret = 0;
    entropy = 0 as f64;
    if _argc > 2 {
        fprintf(
            stderr(),
            b"Usage: %s [<seed>]\n\0" as *const u8 as *const i8,
            *_argv.offset(0 as isize),
        );
        return 1;
    }
    env_seed = getenv(b"SEED\0" as *const u8 as *const i8);
    if _argc > 1 {
        seed = atoi(*_argv.offset(1 as isize)) as u32;
    } else if !env_seed.is_null() {
        seed = atoi(env_seed) as u32;
    } else {
        seed = time(0 as *mut time_t) as u32;
    }
    ptr = malloc(10000000) as *mut u8;
    ec_enc_init(&mut enc, ptr, 10000000);
    ft = 2;
    while ft < 1024 {
        i = 0;
        while i < ft {
            entropy += log(ft as f64) * 1.4426950408889634074f64;
            ec_enc_uint(&mut enc, i as u32, ft as u32);
            i += 1;
        }
        ft += 1;
    }
    ftb = 1;
    while ftb < 16 {
        i = 0;
        while i < (1) << ftb {
            entropy += ftb as f64;
            nbits = ec_tell(&mut enc) as i64;
            ec_enc_bits(&mut enc, i as u32, ftb as u32);
            nbits2 = ec_tell(&mut enc) as i64;
            if nbits2 - nbits != ftb as i64 {
                fprintf(
                    stderr(),
                    b"Used %li bits to encode %i bits directly.\n\0" as *const u8
                        as *const i8,
                    nbits2 - nbits,
                    ftb,
                );
                ret = -1;
            }
            i += 1;
        }
        ftb += 1;
    }
    nbits = ec_tell_frac(&mut enc) as i64;
    ec_enc_done(&mut enc);
    fprintf(
        stderr(),
        b"Encoded %0.2lf bits of entropy to %0.2lf bits (%0.3lf%% wasted).\n\0" as *const u8
            as *const i8,
        entropy,
        ldexp(nbits as f64, -(3)),
        100 as f64
            * (nbits as f64 - ldexp(entropy, 3))
            / nbits as f64,
    );
    fprintf(
        stderr(),
        b"Packed to %li bytes.\n\0" as *const u8 as *const i8,
        ec_range_bytes(&mut enc) as i64,
    );
    ec_dec_init(dec, ptr, 10000000);
    ft = 2;
    while ft < 1024 {
        i = 0;
        while i < ft {
            sym = ec_dec_uint(dec, ft as u32);
            if sym != i as u32 {
                fprintf(
                    stderr(),
                    b"Decoded %i instead of %i with ft of %i.\n\0" as *const u8
                        as *const i8,
                    sym,
                    i,
                    ft,
                );
                ret = -1;
            }
            i += 1;
        }
        ft += 1;
    }
    ftb = 1;
    while ftb < 16 {
        i = 0;
        while i < (1) << ftb {
            sym = ec_dec_bits(dec, ftb as u32);
            if sym != i as u32 {
                fprintf(
                    stderr(),
                    b"Decoded %i instead of %i with ftb of %i.\n\0" as *const u8
                        as *const i8,
                    sym,
                    i,
                    ftb,
                );
                ret = -1;
            }
            i += 1;
        }
        ftb += 1;
    }
    nbits2 = ec_tell_frac(dec) as i64;
    if nbits != nbits2 {
        fprintf(
            stderr(),
            b"Reported number of bits used was %0.2lf, should be %0.2lf.\n\0" as *const u8
                as *const i8,
            ldexp(nbits2 as f64, -(3)),
            ldexp(nbits as f64, -(3)),
        );
        ret = -1;
    }
    ec_enc_init(&mut enc, ptr, 2);
    ec_enc_bits(
        &mut enc,
        0x55,
        7,
    );
    ec_enc_uint(&mut enc, 1, 2);
    ec_enc_uint(&mut enc, 1, 3);
    ec_enc_uint(&mut enc, 1, 4);
    ec_enc_uint(&mut enc, 1, 5);
    ec_enc_uint(&mut enc, 2, 6);
    ec_enc_uint(&mut enc, 6, 7);
    ec_enc_done(&mut enc);
    ec_dec_init(dec, ptr, 2);
    if enc.error == 0
        || ec_dec_bits(dec, 7)
            != 0x5
        || ec_dec_uint(dec, 2) != 1
        || ec_dec_uint(dec, 3) != 1
        || ec_dec_uint(dec, 4) != 1
        || ec_dec_uint(dec, 5) != 1
        || ec_dec_uint(dec, 6) != 2
        || ec_dec_uint(dec, 7) != 6
    {
        fprintf(
            stderr(),
            b"Encoder bust overwrote range coder data with raw bits.\n\0" as *const u8
                as *const i8,
        );
        ret = -1;
    }
    srand(seed);
    fprintf(
        stderr(),
        b"Testing random streams... Random seed: %u (%.4X)\n\0" as *const u8 as *const i8,
        seed,
        rand() % 65536,
    );
    i = 0;
    while i < 409600 {
        let mut data: *mut u32 = 0 as *mut u32;
        let mut tell: *mut u32 = 0 as *mut u32;
        let mut tell_bits: u32 = 0;
        let mut j: i32 = 0;
        let mut zeros: i32 = 0;
        ft = (rand() as u32)
            .wrapping_div(
                ((2147483647
                    >> (rand() as u32).wrapping_rem(11 as u32))
                    as u32)
                    .wrapping_add(1 as u32),
            )
            .wrapping_add(10) as i32;
        sz =
            (rand() as u32).wrapping_div(
                ((2147483647
                    >> (rand() as u32).wrapping_rem(9 as u32))
                    as u32)
                    .wrapping_add(1 as u32),
            ) as i32;
        data = malloc(
            (sz as u64)
                .wrapping_mul(::core::mem::size_of::<u32>() as u64),
        ) as *mut u32;
        tell = malloc(
            ((sz + 1) as u64)
                .wrapping_mul(::core::mem::size_of::<u32>() as u64),
        ) as *mut u32;
        ec_enc_init(&mut enc, ptr, 10000);
        zeros = (rand() % 13 == 0) as i32;
        *tell.offset(0 as isize) = ec_tell_frac(&mut enc);
        j = 0;
        while j < sz {
            if zeros != 0 {
                *data.offset(j as isize) = 0;
            } else {
                *data.offset(j as isize) = (rand() % ft) as u32;
            }
            ec_enc_uint(&mut enc, *data.offset(j as isize), ft as u32);
            *tell.offset((j + 1) as isize) = ec_tell_frac(&mut enc);
            j += 1;
        }
        if rand() % 2 == 0 {
            while ec_tell(&mut enc) % 8 != 0 {
                ec_enc_uint(
                    &mut enc,
                    (rand() % 2) as u32,
                    2,
                );
            }
        }
        tell_bits = ec_tell(&mut enc) as u32;
        ec_enc_done(&mut enc);
        if tell_bits != ec_tell(&mut enc) as u32 {
            fprintf(
                stderr(),
                b"ec_tell() changed after ec_enc_done(): %i instead of %i (Random seed: %u)\n\0"
                    as *const u8 as *const i8,
                ec_tell(&mut enc),
                tell_bits,
                seed,
            );
            ret = -1;
        }
        if tell_bits
            .wrapping_add(7)
            .wrapping_div(8)
            < ec_range_bytes(&mut enc)
        {
            fprintf(
                stderr(),
                b"ec_tell() lied, there's %i bytes instead of %d (Random seed: %u)\n\0" as *const u8
                    as *const i8,
                ec_range_bytes(&mut enc),
                tell_bits
                    .wrapping_add(7)
                    .wrapping_div(8),
                seed,
            );
            ret = -1;
        }
        ec_dec_init(dec, ptr, 10000);
        if ec_tell_frac(dec) != *tell.offset(0 as isize) {
            fprintf(
                stderr(),
                b"Tell mismatch between encoder and decoder at symbol %i: %i instead of %i (Random seed: %u).\n\0"
                    as *const u8 as *const i8,
                0,
                ec_tell_frac(dec),
                *tell.offset(0 as isize),
                seed,
            );
        }
        j = 0;
        while j < sz {
            sym = ec_dec_uint(dec, ft as u32);
            if sym != *data.offset(j as isize) {
                fprintf(
                    stderr(),
                    b"Decoded %i instead of %i with ft of %i at position %i of %i (Random seed: %u).\n\0"
                        as *const u8 as *const i8,
                    sym,
                    *data.offset(j as isize),
                    ft,
                    j,
                    sz,
                    seed,
                );
                ret = -1;
            }
            if ec_tell_frac(dec) != *tell.offset((j + 1) as isize) {
                fprintf(
                    stderr(),
                    b"Tell mismatch between encoder and decoder at symbol %i: %i instead of %i (Random seed: %u).\n\0"
                        as *const u8 as *const i8,
                    j + 1,
                    ec_tell_frac(dec),
                    *tell.offset((j + 1) as isize),
                    seed,
                );
            }
            j += 1;
        }
        free(tell as *mut core::ffi::c_void);
        free(data as *mut core::ffi::c_void);
        i += 1;
    }
    i = 0;
    while i < 409600 {
        let mut logp1: *mut u32 = 0 as *mut u32;
        let mut data_0: *mut u32 = 0 as *mut u32;
        let mut tell_0: *mut u32 = 0 as *mut u32;
        let mut enc_method: *mut u32 = 0 as *mut u32;
        let mut j_0: i32 = 0;
        sz =
            (rand() as u32).wrapping_div(
                ((2147483647
                    >> (rand() as u32).wrapping_rem(9 as u32))
                    as u32)
                    .wrapping_add(1 as u32),
            ) as i32;
        logp1 = malloc(
            (sz as u64)
                .wrapping_mul(::core::mem::size_of::<u32>() as u64),
        ) as *mut u32;
        data_0 = malloc(
            (sz as u64)
                .wrapping_mul(::core::mem::size_of::<u32>() as u64),
        ) as *mut u32;
        tell_0 = malloc(
            ((sz + 1) as u64)
                .wrapping_mul(::core::mem::size_of::<u32>() as u64),
        ) as *mut u32;
        enc_method = malloc(
            (sz as u64)
                .wrapping_mul(::core::mem::size_of::<u32>() as u64),
        ) as *mut u32;
        ec_enc_init(&mut enc, ptr, 10000);
        *tell_0.offset(0 as isize) = ec_tell_frac(&mut enc);
        j_0 = 0;
        while j_0 < sz {
            *data_0.offset(j_0 as isize) = (rand()
                / ((2147483647 >> 1) + 1))
                as u32;
            *logp1.offset(j_0 as isize) =
                (rand() % 15 + 1) as u32;
            *enc_method.offset(j_0 as isize) = (rand()
                / ((2147483647 >> 2) + 1))
                as u32;
            match *enc_method.offset(j_0 as isize) {
                0 => {
                    ec_encode(
                        &mut enc,
                        (if *data_0.offset(j_0 as isize) != 0 {
                            ((1) << *logp1.offset(j_0 as isize)) - 1
                        } else {
                            0
                        }) as u32,
                        (((1) << *logp1.offset(j_0 as isize))
                            - (if *data_0.offset(j_0 as isize) != 0 {
                                0
                            } else {
                                1
                            })) as u32,
                        ((1) << *logp1.offset(j_0 as isize)) as u32,
                    );
                }
                1 => {
                    ec_encode_bin(
                        &mut enc,
                        (if *data_0.offset(j_0 as isize) != 0 {
                            ((1) << *logp1.offset(j_0 as isize)) - 1
                        } else {
                            0
                        }) as u32,
                        (((1) << *logp1.offset(j_0 as isize))
                            - (if *data_0.offset(j_0 as isize) != 0 {
                                0
                            } else {
                                1
                            })) as u32,
                        *logp1.offset(j_0 as isize),
                    );
                }
                2 => {
                    ec_enc_bit_logp(
                        &mut enc,
                        *data_0.offset(j_0 as isize) as i32,
                        *logp1.offset(j_0 as isize),
                    );
                }
                3 => {
                    let mut icdf: [u8; 2] = [0; 2];
                    icdf[0 as usize] = 1;
                    icdf[1 as usize] = 0;
                    ec_enc_icdf(
                        &mut enc,
                        *data_0.offset(j_0 as isize) as i32,
                        icdf.as_mut_ptr(),
                        *logp1.offset(j_0 as isize),
                    );
                }
                _ => {}
            }
            *tell_0.offset((j_0 + 1) as isize) = ec_tell_frac(&mut enc);
            j_0 += 1;
        }
        ec_enc_done(&mut enc);
        if (ec_tell(&mut enc) as u32)
            .wrapping_add(7 as u32)
            .wrapping_div(8 as u32)
            < ec_range_bytes(&mut enc)
        {
            fprintf(
                stderr(),
                b"tell() lied, there's %i bytes instead of %d (Random seed: %u)\n\0" as *const u8
                    as *const i8,
                ec_range_bytes(&mut enc),
                (ec_tell(&mut enc) + 7) / 8,
                seed,
            );
            ret = -1;
        }
        ec_dec_init(dec, ptr, 10000);
        if ec_tell_frac(dec) != *tell_0.offset(0 as isize) {
            fprintf(
                stderr(),
                b"Tell mismatch between encoder and decoder at symbol %i: %i instead of %i (Random seed: %u).\n\0"
                    as *const u8 as *const i8,
                0,
                ec_tell_frac(dec),
                *tell_0.offset(0 as isize),
                seed,
            );
        }
        j_0 = 0;
        while j_0 < sz {
            let mut fs: i32 = 0;
            let mut dec_method: i32 = 0;
            dec_method =
                rand() / ((2147483647 >> 2) + 1);
            match dec_method {
                0 => {
                    fs = ec_decode(
                        dec,
                        ((1) << *logp1.offset(j_0 as isize)) as u32,
                    ) as i32;
                    sym = (fs
                        >= ((1) << *logp1.offset(j_0 as isize)) - 1)
                        as i32 as u32;
                    ec_dec_update(
                        dec,
                        (if sym != 0 {
                            ((1) << *logp1.offset(j_0 as isize)) - 1
                        } else {
                            0
                        }) as u32,
                        (((1) << *logp1.offset(j_0 as isize))
                            - (if sym != 0 {
                                0
                            } else {
                                1
                            })) as u32,
                        ((1) << *logp1.offset(j_0 as isize)) as u32,
                    );
                }
                1 => {
                    fs = ec_decode_bin(dec, *logp1.offset(j_0 as isize)) as i32;
                    sym = (fs
                        >= ((1) << *logp1.offset(j_0 as isize)) - 1)
                        as i32 as u32;
                    ec_dec_update(
                        dec,
                        (if sym != 0 {
                            ((1) << *logp1.offset(j_0 as isize)) - 1
                        } else {
                            0
                        }) as u32,
                        (((1) << *logp1.offset(j_0 as isize))
                            - (if sym != 0 {
                                0
                            } else {
                                1
                            })) as u32,
                        ((1) << *logp1.offset(j_0 as isize)) as u32,
                    );
                }
                2 => {
                    sym = ec_dec_bit_logp(dec, *logp1.offset(j_0 as isize)) as u32;
                }
                3 => {
                    let mut icdf_0: [u8; 2] = [0; 2];
                    icdf_0[0 as usize] = 1;
                    icdf_0[1 as usize] = 0;
                    sym = ec_dec_icdf(dec, icdf_0.as_mut_ptr(), *logp1.offset(j_0 as isize))
                        as u32;
                }
                _ => {}
            }
            if sym != *data_0.offset(j_0 as isize) {
                fprintf(
                    stderr(),
                    b"Decoded %i instead of %i with logp1 of %i at position %i of %i (Random seed: %u).\n\0"
                        as *const u8 as *const i8,
                    sym,
                    *data_0.offset(j_0 as isize),
                    *logp1.offset(j_0 as isize),
                    j_0,
                    sz,
                    seed,
                );
                fprintf(
                    stderr(),
                    b"Encoding method: %i, decoding method: %i\n\0" as *const u8
                        as *const i8,
                    *enc_method.offset(j_0 as isize),
                    dec_method,
                );
                ret = -1;
            }
            if ec_tell_frac(dec) != *tell_0.offset((j_0 + 1) as isize) {
                fprintf(
                    stderr(),
                    b"Tell mismatch between encoder and decoder at symbol %i: %i instead of %i (Random seed: %u).\n\0"
                        as *const u8 as *const i8,
                    j_0 + 1,
                    ec_tell_frac(dec),
                    *tell_0.offset((j_0 + 1) as isize),
                    seed,
                );
            }
            j_0 += 1;
        }
        free(enc_method as *mut core::ffi::c_void);
        free(tell_0 as *mut core::ffi::c_void);
        free(data_0 as *mut core::ffi::c_void);
        free(logp1 as *mut core::ffi::c_void);
        i += 1;
    }
    ec_enc_init(&mut enc, ptr, 10000);
    ec_enc_bit_logp(&mut enc, 0, 1);
    ec_enc_bit_logp(&mut enc, 0, 1);
    ec_enc_bit_logp(&mut enc, 0, 1);
    ec_enc_bit_logp(&mut enc, 0, 1);
    ec_enc_bit_logp(&mut enc, 0, 2);
    ec_enc_patch_initial_bits(
        &mut enc,
        3,
        2,
    );
    if enc.error != 0 {
        fprintf(
            stderr(),
            b"patch_initial_bits failed\0" as *const u8 as *const i8,
        );
        ret = -1;
    }
    ec_enc_patch_initial_bits(
        &mut enc,
        0,
        5,
    );
    if enc.error == 0 {
        fprintf(
            stderr(),
            b"patch_initial_bits didn't fail when it should have\0" as *const u8
                as *const i8,
        );
        ret = -1;
    }
    ec_enc_done(&mut enc);
    if ec_range_bytes(&mut enc) != 1
        || *ptr.offset(0 as isize) as i32 != 192
    {
        fprintf(
            stderr(),
            b"Got %d when expecting 192 for patch_initial_bits\0" as *const u8
                as *const i8,
            *ptr.offset(0 as isize) as i32,
        );
        ret = -1;
    }
    ec_enc_init(&mut enc, ptr, 10000);
    ec_enc_bit_logp(&mut enc, 0, 1);
    ec_enc_bit_logp(&mut enc, 0, 1);
    ec_enc_bit_logp(&mut enc, 1, 6);
    ec_enc_bit_logp(&mut enc, 0, 2);
    ec_enc_patch_initial_bits(
        &mut enc,
        0,
        2,
    );
    if enc.error != 0 {
        fprintf(
            stderr(),
            b"patch_initial_bits failed\0" as *const u8 as *const i8,
        );
        ret = -1;
    }
    ec_enc_done(&mut enc);
    if ec_range_bytes(&mut enc) != 2
        || *ptr.offset(0 as isize) as i32 != 63
    {
        fprintf(
            stderr(),
            b"Got %d when expecting 63 for patch_initial_bits\0" as *const u8
                as *const i8,
            *ptr.offset(0 as isize) as i32,
        );
        ret = -1;
    }
    ec_enc_init(&mut enc, ptr, 2);
    ec_enc_bit_logp(&mut enc, 0, 2);
    i = 0;
    while i < 48 {
        ec_enc_bits(
            &mut enc,
            0,
            1,
        );
        i += 1;
    }
    ec_enc_done(&mut enc);
    if enc.error == 0 {
        fprintf(
            stderr(),
            b"Raw bits overfill didn't fail when it should have\0" as *const u8
                as *const i8,
        );
        ret = -1;
    }
    ec_enc_init(&mut enc, ptr, 2);
    i = 0;
    while i < 17 {
        ec_enc_bits(
            &mut enc,
            0,
            1,
        );
        i += 1;
    }
    ec_enc_done(&mut enc);
    if enc.error == 0 {
        fprintf(
            stderr(),
            b"17 raw bits encoded in two bytes\0" as *const u8 as *const i8,
        );
        ret = -1;
    }
    free(ptr as *mut core::ffi::c_void);
    return ret;
}
pub fn main() {
    let mut args: Vec<*mut i8> = Vec::new();
    for arg in ::std::env::args() {
        args.push(
            (::std::ffi::CString::new(arg))
                .expect("Failed to convert argument into CString.")
                .into_raw(),
        );
    }
    args.push(::core::ptr::null_mut());
    unsafe {
        ::std::process::exit(main_0(
            (args.len() - 1) as i32,
            args.as_mut_ptr() as *mut *mut i8,
        ) as i32)
    }
}
