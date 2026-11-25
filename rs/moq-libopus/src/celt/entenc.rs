#![forbid(unsafe_code)]

use crate::celt::entcode::{celt_udiv, ec_ctx, ec_window, EC_UINT_BITS, EC_WINDOW_SIZE};

pub type ec_enc<'a> = ec_ctx<'a>;

use crate::celt::entcode::{
    EC_CODE_BITS, EC_CODE_BOT, EC_CODE_SHIFT, EC_CODE_TOP, EC_SYM_BITS, EC_SYM_MAX,
};

use crate::silk::macros::EC_CLZ0;

fn ec_write_byte(this: &mut ec_enc, value: u32) -> i32 {
    #[cfg(feature = "ent-dump")]
    eprintln!("ec_write_byte(0x{:x} @ 0x{:x})", value, this.offs);
    if this.offs + this.end_offs >= this.storage {
        return -1;
    }

    this.buf[this.offs as usize] = value as u8;
    this.offs += 1;

    0
}

fn ec_write_byte_at_end(this: &mut ec_enc, value: u32) -> i32 {
    #[cfg(feature = "ent-dump")]
    eprintln!(
        "ec_write_byte_at_end(0x{:x} @ 0x{:x})",
        value, this.end_offs
    );
    if this.offs + this.end_offs >= this.storage {
        return -1;
    }

    this.end_offs += 1;
    this.buf[(this.storage - this.end_offs) as usize] = value as u8;

    0
}

fn ec_enc_carry_out(this: &mut ec_enc, c: i32) {
    if c as u32 != EC_SYM_MAX {
        let mut carry: i32 = 0;
        carry = c >> EC_SYM_BITS;
        if this.rem >= 0 {
            this.error |= ec_write_byte(this, (this.rem + carry) as u32);
        }
        if this.ext > 0 {
            let mut sym: u32 = 0;
            sym = EC_SYM_MAX.wrapping_add(carry as u32) & EC_SYM_MAX;
            loop {
                this.error |= ec_write_byte(this, sym);
                this.ext -= 1;
                if this.ext == 0 {
                    break;
                }
            }
        }
        this.rem = (c as u32 & EC_SYM_MAX) as i32;
    } else {
        this.ext += 1;
    };
}

#[inline]
fn ec_enc_normalize(this: &mut ec_enc) {
    while this.rng <= EC_CODE_BOT {
        ec_enc_carry_out(this, (this.val >> EC_CODE_SHIFT) as i32);
        this.val = this.val << EC_SYM_BITS & EC_CODE_TOP.wrapping_sub(1);
        this.rng <<= EC_SYM_BITS;
        this.nbits_total += EC_SYM_BITS;
    }
}

pub fn ec_enc_init(buf: &mut [u8]) -> ec_enc {
    #[cfg(feature = "ent-dump")]
    eprintln!("ec_enc_init()");
    ec_enc {
        storage: buf.len() as u32,
        buf,
        end_offs: 0,
        end_window: 0,
        nend_bits: 0,
        nbits_total: EC_CODE_BITS + 1,
        offs: 0,
        rng: EC_CODE_TOP,
        val: 0,
        ext: 0,
        rem: -1,
        error: 0,
    }
}

pub fn ec_encode(this: &mut ec_enc, mut _fl: u32, mut _fh: u32, mut _ft: u32) {
    #[cfg(feature = "ent-dump")]
    eprintln!("ec_encode({}, {}, {})", _fl, _fh, _ft);
    let mut r: u32 = 0;
    r = celt_udiv(this.rng, _ft);
    if _fl > 0 {
        this.val = this
            .val
            .wrapping_add((this.rng).wrapping_sub(r.wrapping_mul(_ft.wrapping_sub(_fl))));
        this.rng = r.wrapping_mul(_fh.wrapping_sub(_fl));
    } else {
        this.rng = this.rng.wrapping_sub(r.wrapping_mul(_ft.wrapping_sub(_fh)));
    }
    ec_enc_normalize(this);
}

pub fn ec_encode_bin(this: &mut ec_enc, mut _fl: u32, mut _fh: u32, mut _bits: u32) {
    #[cfg(feature = "ent-dump")]
    eprintln!("ec_encode_bin({}, {}, {})", _fl, _fh, _bits);
    let mut r: u32 = 0;
    r = this.rng >> _bits;
    if _fl > 0 {
        this.val = this.val.wrapping_add(
            this.rng
                .wrapping_sub(r.wrapping_mul((1_u32 << _bits).wrapping_sub(_fl))),
        );
        this.rng = r.wrapping_mul(_fh.wrapping_sub(_fl));
    } else {
        this.rng = this
            .rng
            .wrapping_sub(r.wrapping_mul((1_u32 << _bits).wrapping_sub(_fh)));
    }
    ec_enc_normalize(this);
}

pub fn ec_enc_bit_logp(this: &mut ec_enc, mut _val: i32, mut _logp: u32) {
    #[cfg(feature = "ent-dump")]
    eprintln!("ec_enc_bit_logp({}, {})", _val, _logp);
    let mut r: u32 = 0;
    let mut s: u32 = 0;
    let mut l: u32 = 0;
    r = this.rng;
    l = this.val;
    s = r >> _logp;
    r = r.wrapping_sub(s);
    if _val != 0 {
        this.val = l.wrapping_add(r);
    }
    this.rng = if _val != 0 { s } else { r };
    ec_enc_normalize(this);
}

pub fn ec_enc_icdf(this: &mut ec_enc, s: i32, icdf: &[u8], ftb: u32) {
    // we do a sub-slice here because the C code doesn't have an idea about the icdf length (it doesn't need to)
    #[cfg(feature = "ent-dump")]
    eprintln!("ec_enc_icdf({}, {:?}, {})", s, &icdf[..=(s as usize)], ftb);
    let mut r: u32 = 0;
    r = this.rng >> ftb;
    if s > 0 {
        this.val = this.val.wrapping_add(
            this.rng
                .wrapping_sub(r.wrapping_mul(icdf[s as usize - 1] as u32)),
        );
        this.rng = r.wrapping_mul((icdf[s as usize - 1] as i32 - icdf[s as usize] as i32) as u32);
    } else {
        this.rng = this
            .rng
            .wrapping_sub(r.wrapping_mul(icdf[s as usize] as u32));
    }
    ec_enc_normalize(this);
}

pub fn ec_enc_uint(mut _this: &mut ec_enc, mut _fl: u32, mut _ft: u32) {
    #[cfg(feature = "ent-dump")]
    eprintln!("ec_enc_uint({}, {})", _fl, _ft);
    let mut ft: u32 = 0;
    let mut fl: u32 = 0;
    let mut ftb: i32 = 0;
    assert!(_ft > 1);
    _ft = _ft.wrapping_sub(1);
    ftb = EC_CLZ0 - _ft.leading_zeros() as i32;
    if ftb > EC_UINT_BITS {
        ftb -= EC_UINT_BITS;
        ft = (_ft >> ftb).wrapping_add(1);
        fl = _fl >> ftb;
        ec_encode(_this, fl, fl.wrapping_add(1), ft);
        ec_enc_bits(_this, _fl & (1_u32 << ftb).wrapping_sub(1), ftb as u32);
    } else {
        ec_encode(_this, _fl, _fl.wrapping_add(1), _ft.wrapping_add(1));
    };
}

pub fn ec_enc_bits(this: &mut ec_enc, mut _fl: u32, mut _bits: u32) {
    #[cfg(feature = "ent-dump")]
    eprintln!("ec_enc_bits({}, {})", _fl, _bits);
    let mut window: ec_window = 0;
    let mut used: i32 = 0;
    window = this.end_window;
    used = this.nend_bits;
    assert!(_bits > 0);
    if (used as u32).wrapping_add(_bits) > EC_WINDOW_SIZE as u32 {
        loop {
            this.error |= ec_write_byte_at_end(this, window & EC_SYM_MAX);
            window >>= EC_SYM_BITS;
            used -= EC_SYM_BITS;
            if !(used >= EC_SYM_BITS) {
                break;
            }
        }
    }
    window |= _fl << used;
    used = (used as u32).wrapping_add(_bits) as i32;
    this.end_window = window;
    this.nend_bits = used;
    this.nbits_total = (this.nbits_total as u32).wrapping_add(_bits) as i32;
}

pub fn ec_enc_patch_initial_bits(this: &mut ec_enc, mut _val: u32, mut _nbits: u32) {
    #[cfg(feature = "ent-dump")]
    eprintln!("ec_enc_patch_initial_bits({}, {})", _val, _nbits);
    let mut shift: i32 = 0;
    let mut mask: u32 = 0;
    assert!(_nbits <= 8);
    shift = (EC_SYM_BITS as u32).wrapping_sub(_nbits) as i32;
    mask = ((((1) << _nbits) - 1) << shift) as u32;
    if this.offs > 0 {
        this.buf[0] = (this.buf[0] as u32 & !mask | _val << shift) as u8;
    } else if this.rem >= 0 {
        this.rem = (this.rem as u32 & !mask | _val << shift) as i32;
    } else if this.rng <= EC_CODE_TOP >> _nbits {
        this.val = this.val & !(mask << EC_CODE_SHIFT) | _val << (EC_CODE_SHIFT + shift);
    } else {
        this.error = -1;
    };
}

pub fn ec_enc_shrink(this: &mut ec_enc, new_size: u32) {
    #[cfg(feature = "ent-dump")]
    eprintln!("ec_enc_shrink({})", new_size);
    assert!((this.offs).wrapping_add(this.end_offs) <= new_size);

    this.buf.copy_within(
        (this.storage - this.end_offs) as usize..this.storage as usize,
        (new_size - this.end_offs) as usize,
    );
    this.storage = new_size;
}

pub fn ec_enc_done(this: &mut ec_enc) {
    let mut window: ec_window = 0;
    let mut used: i32 = 0;
    let mut msk: u32 = 0;
    let mut end: u32 = 0;
    let mut l: i32 = 0;
    /*We output the minimum number of bits that ensures that the symbols encoded
    thus far will be decoded correctly regardless of the bits that follow.*/
    l = EC_CODE_BITS - (EC_CLZ0 - (this.rng).leading_zeros() as i32);
    msk = EC_CODE_TOP.wrapping_sub(1) >> l;
    end = (this.val).wrapping_add(msk) & !msk;
    if end | msk >= (this.val).wrapping_add(this.rng) {
        l += 1;
        msk >>= 1;
        end = (this.val).wrapping_add(msk) & !msk;
    }
    while l > 0 {
        ec_enc_carry_out(this, (end >> EC_CODE_SHIFT) as i32);
        end = end << EC_SYM_BITS & EC_CODE_TOP.wrapping_sub(1);
        l -= EC_SYM_BITS;
    }
    /*If we have a buffered byte flush it into the output buffer.*/
    if this.rem >= 0 || this.ext > 0 {
        ec_enc_carry_out(this, 0);
    }
    /*If we have buffered extra bits, flush them as well.*/
    window = this.end_window;
    used = this.nend_bits;
    while used >= EC_SYM_BITS {
        this.error |= ec_write_byte_at_end(this, window & EC_SYM_MAX);
        window >>= EC_SYM_BITS;
        used -= EC_SYM_BITS;
    }
    /*Clear any excess space and add any remaining extra bits to the last byte.*/
    if this.error == 0 {
        this.buf[this.offs as usize..(this.storage - this.end_offs) as usize].fill(0);
        if used > 0 {
            /*If there's no range coder data at all, give up.*/
            if this.end_offs >= this.storage {
                this.error = -1;
            } else {
                /*If we've busted, don't add too many extra bits to the last byte; it
                would corrupt the range coder data, and that's more important.*/
                l = -l;
                if (this.offs).wrapping_add(this.end_offs) >= this.storage && l < used {
                    window &= (((1) << l) - 1) as u32;
                    this.error = -1;
                }
                let fresh1 = &mut this.buf[(this.storage - this.end_offs - 1) as usize];
                *fresh1 = (*fresh1 as i32 | window as u8 as i32) as u8;
            }
        }
    }

    #[cfg(feature = "ent-dump")]
    eprintln!(
        "ec_enc_done(): {}",
        hex::encode(&this.buf[..this.storage as usize])
    );
}
