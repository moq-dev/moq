use crate::silk::bwexpander_32::silk_bwexpander_32;

pub mod typedef_h {
    pub const silk_int16_MIN: i32 = i16::MIN as i32;
    pub const silk_int16_MAX: i32 = i16::MAX as i32;
}

pub use self::typedef_h::{silk_int16_MAX, silk_int16_MIN};

pub unsafe fn silk_LPC_fit(a_QOUT: *mut i16, a_QIN: *mut i32, QOUT: i32, QIN: i32, d: i32) {
    let mut i: i32 = 0;
    let mut k: i32 = 0;
    let mut idx: i32 = 0;
    let mut maxabs: i32 = 0;
    let mut absval: i32 = 0;
    let mut chirp_Q16: i32 = 0;
    i = 0;
    while i < 10 {
        maxabs = 0;
        k = 0;
        while k < d {
            absval = if *a_QIN.offset(k as isize) > 0 {
                *a_QIN.offset(k as isize)
            } else {
                -*a_QIN.offset(k as isize)
            };
            if absval > maxabs {
                maxabs = absval;
                idx = k;
            }
            k += 1;
        }
        maxabs = if QIN - QOUT == 1 {
            (maxabs >> 1) + (maxabs & 1)
        } else {
            (maxabs >> QIN - QOUT - 1) + 1 >> 1
        };
        if !(maxabs > silk_int16_MAX) {
            break;
        }
        maxabs = if maxabs < 163838 { maxabs } else { 163838 };
        chirp_Q16 = (0.999f64 * ((1) << 16) as f64 + 0.5f64) as i32
            - (((maxabs - 0x7fff) as u32) << 14) as i32 / (maxabs * (idx + 1) >> 2);
        silk_bwexpander_32(a_QIN, d, chirp_Q16);
        i += 1;
    }
    if i == 10 {
        k = 0;
        while k < d {
            *a_QOUT.offset(k as isize) = (if (if QIN - QOUT == 1 {
                (*a_QIN.offset(k as isize) >> 1) + (*a_QIN.offset(k as isize) & 1)
            } else {
                (*a_QIN.offset(k as isize) >> QIN - QOUT - 1) + 1 >> 1
            }) > silk_int16_MAX
            {
                silk_int16_MAX
            } else if (if QIN - QOUT == 1 {
                (*a_QIN.offset(k as isize) >> 1) + (*a_QIN.offset(k as isize) & 1)
            } else {
                (*a_QIN.offset(k as isize) >> QIN - QOUT - 1) + 1 >> 1
            }) < silk_int16_MIN
            {
                silk_int16_MIN
            } else if QIN - QOUT == 1 {
                (*a_QIN.offset(k as isize) >> 1) + (*a_QIN.offset(k as isize) & 1)
            } else {
                (*a_QIN.offset(k as isize) >> QIN - QOUT - 1) + 1 >> 1
            }) as i16;
            *a_QIN.offset(k as isize) =
                ((*a_QOUT.offset(k as isize) as i32 as u32) << QIN - QOUT) as i32;
            k += 1;
        }
    } else {
        k = 0;
        while k < d {
            *a_QOUT.offset(k as isize) = (if QIN - QOUT == 1 {
                (*a_QIN.offset(k as isize) >> 1) + (*a_QIN.offset(k as isize) & 1)
            } else {
                (*a_QIN.offset(k as isize) >> QIN - QOUT - 1) + 1 >> 1
            }) as i16;
            k += 1;
        }
    };
}
