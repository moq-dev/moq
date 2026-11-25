#![forbid(unsafe_code)]

use crate::celt::entcode::{celt_udiv, ec_ctx, ec_window, EC_UINT_BITS, EC_WINDOW_SIZE};
use crate::silk::macros::EC_CLZ0;

pub type ec_dec<'a> = ec_ctx<'a>;

use crate::celt::entcode::{
    EC_CODE_BITS, EC_CODE_BOT, EC_CODE_EXTRA, EC_CODE_TOP, EC_SYM_BITS, EC_SYM_MAX,
};

fn ec_read_byte(this: &mut ec_dec) -> i32 {
    if this.offs < this.storage {
        let res = this.buf[this.offs as usize] as i32;

        this.offs += 1;

        res
    } else {
        0
    }
}

fn ec_read_byte_from_end(this: &mut ec_dec) -> i32 {
    if this.end_offs < this.storage {
        this.end_offs += 1;

        this.buf[(this.storage - this.end_offs) as usize] as i32
    } else {
        0
    }
}

fn ec_dec_normalize(this: &mut ec_dec) {
    while this.rng <= EC_CODE_BOT {
        let mut sym: i32 = 0;
        this.nbits_total += EC_SYM_BITS;
        this.rng <<= EC_SYM_BITS;
        sym = this.rem;
        this.rem = ec_read_byte(this);
        sym = (sym << EC_SYM_BITS | this.rem) >> (EC_SYM_BITS - EC_CODE_EXTRA);
        this.val = (this.val << EC_SYM_BITS).wrapping_add(EC_SYM_MAX & !sym as u32)
            & EC_CODE_TOP.wrapping_sub(1);
    }
}

pub fn ec_dec_init(buf: &mut [u8]) -> ec_dec {
    let mut this = ec_dec {
        storage: buf.len() as u32,
        buf,
        end_offs: 0,
        end_window: 0,
        nend_bits: 0,
        nbits_total: EC_CODE_BITS + 1 - (EC_CODE_BITS - EC_CODE_EXTRA) / EC_SYM_BITS * EC_SYM_BITS,
        offs: 0,
        rng: 1 << EC_CODE_EXTRA,
        val: 0,
        ext: 0,
        rem: 0,
        error: 0,
    };

    let rem = ec_read_byte(&mut this);
    this.rem = rem;
    this.val = (this.rng)
        .wrapping_sub(1)
        .wrapping_sub((this.rem >> (EC_SYM_BITS - EC_CODE_EXTRA)) as u32);

    ec_dec_normalize(&mut this);

    this
}

pub fn ec_decode(this: &mut ec_dec, mut _ft: u32) -> u32 {
    let mut s: u32 = 0;
    this.ext = celt_udiv(this.rng, _ft);
    s = (this.val).wrapping_div(this.ext);

    _ft.wrapping_sub(s.wrapping_add(1).wrapping_add(
        _ft.wrapping_sub(s.wrapping_add(1)) & -((_ft < s.wrapping_add(1)) as i32) as u32,
    ))
}

pub fn ec_decode_bin(this: &mut ec_dec, mut _bits: u32) -> u32 {
    let mut s: u32 = 0;
    this.ext = this.rng >> _bits;
    s = (this.val).wrapping_div(this.ext);

    (1_u32 << _bits).wrapping_sub(s.wrapping_add(1).wrapping_add(
        (1_u32 << _bits).wrapping_sub(s.wrapping_add(1))
            & -((1_u32 << _bits < s.wrapping_add(1)) as i32) as u32,
    ))
}

pub fn ec_dec_update(mut _this: &mut ec_dec, mut _fl: u32, mut _fh: u32, mut _ft: u32) {
    let mut s: u32 = 0;
    s = (_this.ext).wrapping_mul(_ft.wrapping_sub(_fh));
    _this.val = _this.val.wrapping_sub(s);
    _this.rng = if _fl > 0 {
        (_this.ext).wrapping_mul(_fh.wrapping_sub(_fl))
    } else {
        (_this.rng).wrapping_sub(s)
    };
    ec_dec_normalize(_this);
}

pub fn ec_dec_bit_logp(mut _this: &mut ec_dec, mut _logp: u32) -> i32 {
    let mut r: u32 = 0;
    let mut d: u32 = 0;
    let mut s: u32 = 0;
    let mut ret: i32 = 0;
    r = _this.rng;
    d = _this.val;
    s = r >> _logp;
    ret = (d < s) as i32;
    if ret == 0 {
        _this.val = d.wrapping_sub(s);
    }
    _this.rng = if ret != 0 { s } else { r.wrapping_sub(s) };
    ec_dec_normalize(_this);

    ret
}

pub fn ec_dec_icdf(mut _this: &mut ec_dec, icdf: &[u8], mut _ftb: u32) -> i32 {
    let mut r: u32 = 0;
    let mut d: u32 = 0;
    let mut s: u32 = 0;
    let mut t: u32 = 0;
    let mut ret: i32 = 0;
    s = _this.rng;
    d = _this.val;
    r = s >> _ftb;
    ret = -1;
    loop {
        t = s;
        ret += 1;
        s = r.wrapping_mul(icdf[ret as usize] as u32);
        if !(d < s) {
            break;
        }
    }
    _this.val = d.wrapping_sub(s);
    _this.rng = t.wrapping_sub(s);
    ec_dec_normalize(_this);

    ret
}

pub fn ec_dec_uint(mut _this: &mut ec_dec, mut _ft: u32) -> u32 {
    let mut ft: u32 = 0;
    let mut s: u32 = 0;
    let mut ftb: i32 = 0;
    assert!(_ft > 1);
    _ft = _ft.wrapping_sub(1);
    ftb = EC_CLZ0 - _ft.leading_zeros() as i32;
    if ftb > EC_UINT_BITS {
        let mut t: u32 = 0;
        ftb -= EC_UINT_BITS;
        ft = (_ft >> ftb).wrapping_add(1);
        s = ec_decode(_this, ft);
        ec_dec_update(_this, s, s.wrapping_add(1), ft);
        t = s << ftb | ec_dec_bits(_this, ftb as u32);
        if t <= _ft {
            return t;
        }
        _this.error = 1;
        _ft
    } else {
        _ft = _ft.wrapping_add(1);
        s = ec_decode(_this, _ft);
        ec_dec_update(_this, s, s.wrapping_add(1), _ft);
        s
    }
}

pub fn ec_dec_bits(mut _this: &mut ec_dec, mut _bits: u32) -> u32 {
    let mut window: ec_window = 0;
    let mut available: i32 = 0;
    let mut ret: u32 = 0;
    window = _this.end_window;
    available = _this.nend_bits;
    if (available as u32) < _bits {
        loop {
            window |= (ec_read_byte_from_end(_this) as ec_window) << available;
            available += EC_SYM_BITS;
            if !(available <= EC_WINDOW_SIZE - EC_SYM_BITS) {
                break;
            }
        }
    }
    ret = window & (1_u32 << _bits).wrapping_sub(1);
    window >>= _bits;
    available = (available as u32).wrapping_sub(_bits) as i32;
    _this.end_window = window;
    _this.nend_bits = available;
    _this.nbits_total = (_this.nbits_total as u32).wrapping_add(_bits) as i32;

    ret
}
