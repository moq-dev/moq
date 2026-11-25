pub mod stddef_h {
        pub type size_t = u64;
}
pub mod arch_h {
        pub type opus_val16 = f32;
        pub type opus_val32 = f32;
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
                pub fn exp(_: f64) -> f64;
                pub fn log(_: f64) -> f64;
                pub fn sqrt(_: f64) -> f64;
                pub fn fabs(_: f64) -> f64;
    }
}
use self::arch_h::{opus_val16, opus_val32};
use self::bands_h::{bitexact_cos, bitexact_log2tan};
use self::mathcalls_h::{exp, fabs, log, sqrt};
pub use self::stddef_h::size_t;

use self::stdio_h::{fprintf, stderr};
pub use self::struct_FILE_h::{_IO_codecvt, _IO_lock_t, _IO_marker, _IO_wide_data, _IO_FILE};

pub use self::FILE_h::FILE;
pub static mut ret: i32 = 0;
pub unsafe fn testdiv() {
    let mut i: i32 = 0;
    i = 1;
    while i <= 327670 {
        let mut prod: f64 = 0.;
        let mut val: opus_val32 = 0.;
        val = 1.0f32 / i as f32;
        prod = (val * i as f32) as f64;
        if fabs(prod - 1 as f64) > 0.00025f64 {
            fprintf(
                stderr(),
                b"div failed: 1/%d=%f (product = %f)\n\0" as *const u8 as *const i8,
                i,
                val as f64,
                prod,
            );
            ret = 1;
        }
        i += 1;
    }
}
pub unsafe fn testsqrt() {
    let mut i: i32 = 0;
    i = 1;
    while i <= 1000000000 {
        let mut ratio: f64 = 0.;
        let mut val: opus_val16 = 0.;
        val = sqrt(i as f64) as f32;
        ratio = val as f64 / sqrt(i as f64);
        if fabs(ratio - 1 as f64) > 0.0005f64
            && fabs(val as f64 - sqrt(i as f64))
                > 2 as f64
        {
            fprintf(
                stderr(),
                b"sqrt failed: sqrt(%d)=%f (ratio = %f)\n\0" as *const u8 as *const i8,
                i,
                val as f64,
                ratio,
            );
            ret = 1;
        }
        i += i >> 10;
        i += 1;
    }
}
pub unsafe fn testbitexactcos() {
    let mut i: i32 = 0;
    let mut min_d: i32 = 0;
    let mut max_d: i32 = 0;
    let mut last: i32 = 0;
    let mut chk: i32 = 0;
    max_d = 0;
    chk = max_d;
    min_d = 32767;
    last = min_d;
    i = 64;
    while i <= 16320 {
        let mut d: i32 = 0;
        let mut q: i32 = bitexact_cos(i as i16) as i32;
        chk ^= q * i;
        d = last - q;
        if d > max_d {
            max_d = d;
        }
        if d < min_d {
            min_d = d;
        }
        last = q;
        i += 1;
    }
    if chk != 89408644
        || max_d != 5
        || min_d != 0
        || bitexact_cos(64) as i32 != 32767
        || bitexact_cos(16320) as i32 != 200
        || bitexact_cos(8192) as i32 != 23171
    {
        fprintf(
            stderr(),
            b"bitexact_cos failed\n\0" as *const u8 as *const i8,
        );
        ret = 1;
    }
}
pub unsafe fn testbitexactlog2tan() {
    let mut i: i32 = 0;
    let mut fail: i32 = 0;
    let mut min_d: i32 = 0;
    let mut max_d: i32 = 0;
    let mut last: i32 = 0;
    let mut chk: i32 = 0;
    max_d = 0;
    chk = max_d;
    fail = chk;
    min_d = 15059;
    last = min_d;
    i = 64;
    while i < 8193 {
        let mut d: i32 = 0;
        let mut mid: i32 = bitexact_cos(i as i16) as i32;
        let mut side: i32 = bitexact_cos((16384 - i) as i16) as i32;
        let mut q: i32 = bitexact_log2tan(mid, side);
        chk ^= q * i;
        d = last - q;
        if q != -1 * bitexact_log2tan(side, mid) {
            fail = 1;
        }
        if d > max_d {
            max_d = d;
        }
        if d < min_d {
            min_d = d;
        }
        last = q;
        i += 1;
    }
    if chk != 15821257
        || max_d != 61
        || min_d != -(2)
        || fail != 0
        || bitexact_log2tan(32767, 200) != 15059
        || bitexact_log2tan(30274, 12540) != 2611
        || bitexact_log2tan(23171, 23171) != 0
    {
        fprintf(
            stderr(),
            b"bitexact_log2tan failed\n\0" as *const u8 as *const i8,
        );
        ret = 1;
    }
}
pub unsafe fn testlog2() {
    let mut x: f32 = 0.;
    x = 0.001f64 as f32;
    while (x as f64) < 1677700.0f64 {
        let mut error: f32 = fabs(
            1.442695040888963387f64 * log(x as f64)
                - (1.442695040888963387f64 * log(x as f64)) as f32
                    as f64,
        ) as f32;
        if error as f64 > 0.0009f64 {
            fprintf(
                stderr(),
                b"celt_log2 failed: fabs((1.442695040888963387*log(x))-celt_log2(x))>0.001 (x = %f, error = %f)\n\0"
                    as *const u8 as *const i8,
                x as f64,
                error as f64,
            );
            ret = 1;
        }
        x = (x as f64 + x as f64 / 8.0f64) as f32;
    }
}
pub unsafe fn testexp2() {
    let mut x: f32 = 0.;
    x = -11.0f64 as f32;
    while (x as f64) < 24.0f64 {
        let mut error: f32 = fabs(
            x as f64
                - 1.442695040888963387f64
                    * log(
                        exp(0.6931471805599453094f64 * x as f64) as f32
                            as f64,
                    ),
        ) as f32;
        if error as f64 > 0.0002f64 {
            fprintf(
                stderr(),
                b"celt_exp2 failed: fabs(x-(1.442695040888963387*log(celt_exp2(x))))>0.0005 (x = %f, error = %f)\n\0"
                    as *const u8 as *const i8,
                x as f64,
                error as f64,
            );
            ret = 1;
        }
        x = (x as f64 + 0.0007f64) as f32;
    }
}
pub unsafe fn testexp2log2() {
    let mut x: f32 = 0.;
    x = -11.0f64 as f32;
    while (x as f64) < 24.0f64 {
        let mut error: f32 = fabs(
            (x - (1.442695040888963387f64
                * log(
                    exp(0.6931471805599453094f64 * x as f64) as f32
                        as f64,
                )) as f32) as f64,
        ) as f32;
        if error as f64 > 0.001f64 {
            fprintf(
                stderr(),
                b"celt_log2/celt_exp2 failed: fabs(x-(celt_log2(celt_exp2(x))))>0.001 (x = %f, error = %f)\n\0"
                    as *const u8 as *const i8,
                x as f64,
                error as f64,
            );
            ret = 1;
        }
        x = (x as f64 + 0.0007f64) as f32;
    }
}
unsafe fn main_0() -> i32 {
    testbitexactcos();
    testbitexactlog2tan();
    testdiv();
    testsqrt();
    testlog2();
    testexp2();
    testexp2log2();
    return ret;
}
pub fn main() {
    unsafe { ::std::process::exit(main_0() as i32) }
}
