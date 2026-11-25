use crate::celt::entdec::{ec_dec, ec_dec_update, ec_decode_bin};
use crate::celt::entenc::{ec_enc, ec_encode_bin};

pub const LAPLACE_LOG_MINP: i32 = 0;
pub const LAPLACE_MINP: i32 = (1) << LAPLACE_LOG_MINP;
pub const LAPLACE_NMIN: i32 = 16;
unsafe fn ec_laplace_get_freq1(fs0: u32, decay: i32) -> u32 {
    let mut ft: u32 = 0;
    ft = ((32768 - LAPLACE_MINP * (2 * LAPLACE_NMIN)) as u32).wrapping_sub(fs0);
    return ft.wrapping_mul((16384 - decay) as u32) >> 15;
}
pub unsafe fn ec_laplace_encode(enc: &mut ec_enc, value: *mut i32, mut fs: u32, decay: i32) {
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
            fl = fl.wrapping_add(fs.wrapping_add((2 * LAPLACE_MINP) as u32));
            fs = fs.wrapping_mul(decay as u32) >> 15;
            i += 1;
        }
        if fs == 0 {
            let mut di: i32 = 0;
            let mut ndi_max: i32 = 0;
            ndi_max = (32768_u32
                .wrapping_sub(fl)
                .wrapping_add(LAPLACE_MINP as u32)
                .wrapping_sub(1)
                >> LAPLACE_LOG_MINP) as i32;
            ndi_max = ndi_max - s >> 1;
            di = if val - i < ndi_max - 1 {
                val - i
            } else {
                ndi_max - 1
            };
            fl = fl.wrapping_add(((2 * di + 1 + s) * LAPLACE_MINP) as u32);
            fs = if (((1) << 0) as u32) < 32768_u32.wrapping_sub(fl) {
                ((1) << 0) as u32
            } else {
                32768_u32.wrapping_sub(fl)
            };
            *value = i + di + s ^ s;
        } else {
            fs = fs.wrapping_add(LAPLACE_MINP as u32);
            fl = fl.wrapping_add(fs & !s as u32);
        }
        assert!(fl.wrapping_add(fs) <= 32768);
        assert!(fs > 0);
    }
    ec_encode_bin(enc, fl, fl.wrapping_add(fs), 15);
}
pub unsafe fn ec_laplace_decode(dec: &mut ec_dec, mut fs: u32, decay: i32) -> i32 {
    let mut val: i32 = 0;
    let mut fl: u32 = 0;
    let mut fm: u32 = 0;
    fm = ec_decode_bin(dec, 15);
    fl = 0;
    if fm >= fs {
        val += 1;
        fl = fs;
        fs = (ec_laplace_get_freq1(fs, decay)).wrapping_add(LAPLACE_MINP as u32);
        while fs > LAPLACE_MINP as u32 && fm >= fl.wrapping_add(2_u32.wrapping_mul(fs)) {
            fs = fs.wrapping_mul(2);
            fl = fl.wrapping_add(fs);
            fs = fs
                .wrapping_sub((2 * LAPLACE_MINP) as u32)
                .wrapping_mul(decay as u32)
                >> 15;
            fs = fs.wrapping_add(LAPLACE_MINP as u32);
            val += 1;
        }
        if fs <= LAPLACE_MINP as u32 {
            let mut di: i32 = 0;
            di = (fm.wrapping_sub(fl) >> LAPLACE_LOG_MINP + 1) as i32;
            val += di;
            fl = fl.wrapping_add((2 * di * LAPLACE_MINP) as u32);
        }
        if fm < fl.wrapping_add(fs) {
            val = -val;
        } else {
            fl = fl.wrapping_add(fs);
        }
    }
    assert!(fl < 32768);
    assert!(fs > 0);
    assert!(fl <= fm);
    assert!(fm < fl.wrapping_add(fs).min(32768));
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
