pub mod stddef_h {
        pub type size_t = u64;
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
                pub fn srand(__seed: u32);
    }
}
pub mod cpu_support_h {
    #[inline]
        pub unsafe fn opus_select_arch() -> i32 {
        return 0;
    }
}
pub mod mathcalls_h {
    {
                pub fn fabs(_: f64) -> f64;
    }
}
use self::cpu_support_h::opus_select_arch;
pub use self::stddef_h::size_t;

use self::stdio_h::{fprintf, printf, stderr};
use self::stdlib_h::{rand, srand};

pub use self::FILE_h::FILE;
use self::SigProc_FIX_h::silk_LPC_inverse_pred_gain_c;
pub unsafe fn check_stability(A_Q12: *mut i16, order: i32) -> i32 {
    let mut i: i32 = 0;
    let mut j: i32 = 0;
    let mut sum_a: i32 = 0;
    let mut sum_abs_a: i32 = 0;
    sum_abs_a = 0;
    sum_a = sum_abs_a;
    j = 0;
    while j < order {
        sum_a += *A_Q12.offset(j as isize) as i32;
        sum_abs_a += if *A_Q12.offset(j as isize) as i32 > 0 {
            *A_Q12.offset(j as isize) as i32
        } else {
            -(*A_Q12.offset(j as isize) as i32)
        };
        j += 1;
    }
    if sum_a >= 4096 {
        return 0;
    }
    if sum_abs_a < 4096 {
        return 1;
    }
    let mut y: [f64; 24] = [
        0 as f64,
        0.,
        0.,
        0.,
        0.,
        0.,
        0.,
        0.,
        0.,
        0.,
        0.,
        0.,
        0.,
        0.,
        0.,
        0.,
        0.,
        0.,
        0.,
        0.,
        0.,
        0.,
        0.,
        0.,
    ];
    y[0 as usize] = 1 as f64;
    i = 0;
    while i < 10000 {
        let mut sum: f64 = 0 as f64;
        j = 0;
        while j < order {
            sum += y[j as usize] * *A_Q12.offset(j as isize) as i32 as f64;
            j += 1;
        }
        j = order - 1;
        while j > 0 {
            y[j as usize] = y[(j - 1) as usize];
            j -= 1;
        }
        y[0 as usize] = sum * (1.0f64 / 4096 as f64);
        if !(y[0 as usize] < 10000 as f64
            && y[0 as usize] > -(10000) as f64)
        {
            return 0;
        }
        if i & 0x7 == 0 {
            let mut amp: f64 = 0 as f64;
            j = 0;
            while j < order {
                amp += y[j as usize].abs();
                j += 1;
            }
            if amp < 0.00001f64 {
                return 1;
            }
        }
        i += 1;
    }
    return 1;
}
unsafe fn main_0() -> i32 {
    let _arch: i32 = opus_select_arch();
    let loop_num: i32 = 10000;
    let mut count: i32 = 0;
    srand(0);
    printf(
        b"Testing silk_LPC_inverse_pred_gain() optimization ...\n\0" as *const u8
            as *const i8,
    );
    count = 0;
    while count < loop_num {
        let mut i: u32 = 0;
        let mut order: i32 = 0;
        let mut shift: u32 = 0;
        let mut A_Q12: [i16; 24] = [0; 24];
        let mut gain: i32 = 0;
        order = 2;
        while order <= 24 {
            shift = 0;
            while shift < 16 {
                i = 0;
                while i < 24 {
                    A_Q12[i as usize] = (rand() as i16 as i32 >> shift) as i16;
                    i = i.wrapping_add(1);
                }
                gain = silk_LPC_inverse_pred_gain_c(A_Q12.as_mut_ptr(), order);
                if gain != 0 && check_stability(A_Q12.as_mut_ptr(), order) == 0 {
                    fprintf(
                        stderr(),
                        b"**Loop %4d failed!**\n\0" as *const u8 as *const i8,
                        count,
                    );
                    return 1;
                }
                shift = shift.wrapping_add(1);
            }
            order += 2;
        }
        if count % 500 == 0 {
            printf(
                b"Loop %4d passed\n\0" as *const u8 as *const i8,
                count,
            );
        }
        count += 1;
    }
    printf(
        b"silk_LPC_inverse_pred_gain() optimization passed\n\0" as *const u8 as *const i8,
    );
    return 0;
}
pub fn main() {
    unsafe { ::std::process::exit(main_0() as i32) }
}
