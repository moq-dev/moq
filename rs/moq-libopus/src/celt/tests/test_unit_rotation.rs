pub mod stddef_h {
        pub type size_t = u64;
}
pub mod arch_h {
        pub type opus_val16 = f32;
        pub type celt_norm = f32;
}
pub mod stdio_h {
    use super::FILE_h::FILE;
    {
                pub static mut stderr: *mut FILE;
                pub fn fprintf(_: *mut FILE, _: *const i8, _: ...) -> i32;
                pub fn printf(_: *const i8, _: ...) -> i32;
    }
}
pub mod stdlib_h {
    {
                pub fn rand() -> i32;
    }
}
pub mod mathcalls_h {
    {
                pub fn log10(_: f64) -> f64;
    }
}
pub mod vq_h {
    use super::arch_h::celt_norm;
    {
                pub fn exp_rotation(
            X: *mut celt_norm,
            len: i32,
            dir: i32,
            stride: i32,
            K: i32,
            spread: i32,
        );
    }
}
pub use self::arch_h::{celt_norm, opus_val16};
use self::mathcalls_h::log10;
pub use self::stddef_h::size_t;
use self::stdio_h::{fprintf, printf, stderr};
use self::stdlib_h::rand;
pub use self::struct_FILE_h::{_IO_codecvt, _IO_lock_t, _IO_marker, _IO_wide_data, _IO_FILE};

use self::vq_h::exp_rotation;
pub use self::FILE_h::FILE;
pub static mut ret: i32 = 0;
pub unsafe fn test_rotation(mut N: i32, mut K: i32) {
    let mut i: i32 = 0;
    let mut err: f64 = 0 as f64;
    let mut ener: f64 = 0 as f64;
    let mut snr: f64 = 0.;
    let mut snr0: f64 = 0.;
    let mut x0: [opus_val16; 100] = [0.; 100];
    let mut x1: [opus_val16; 100] = [0.; 100];
    i = 0;
    while i < N {
        x0[i as usize] = (rand() % 32767 - 16384) as opus_val16;
        x1[i as usize] = x0[i as usize];
        i += 1;
    }
    exp_rotation(
        x1.as_mut_ptr(),
        N,
        1,
        1,
        K,
        2,
    );
    i = 0;
    while i < N {
        err += (x0[i as usize] as f64 - x1[i as usize] as f64)
            * (x0[i as usize] as f64 - x1[i as usize] as f64);
        ener += x0[i as usize] as f64 * x0[i as usize] as f64;
        i += 1;
    }
    snr0 = 20 as f64 * log10(ener / err);
    ener = 0 as f64;
    err = ener;
    exp_rotation(
        x1.as_mut_ptr(),
        N,
        -1,
        1,
        K,
        2,
    );
    i = 0;
    while i < N {
        err += (x0[i as usize] as f64 - x1[i as usize] as f64)
            * (x0[i as usize] as f64 - x1[i as usize] as f64);
        ener += x0[i as usize] as f64 * x0[i as usize] as f64;
        i += 1;
    }
    snr = 20 as f64 * log10(ener / err);
    printf(
        b"SNR for size %d (%d pulses) is %f (was %f without inverse)\n\0" as *const u8
            as *const i8,
        N,
        K,
        snr,
        snr0,
    );
    if snr < 60 as f64 || snr0 > 20 as f64 {
        fprintf(stderr(), b"FAIL!\n\0" as *const u8 as *const i8);
        ret = 1;
    }
}
unsafe fn main_0() -> i32 {
    test_rotation(15, 3);
    test_rotation(23, 5);
    test_rotation(50, 3);
    test_rotation(80, 1);
    return ret;
}
pub fn main() {
    unsafe { ::std::process::exit(main_0() as i32) }
}
