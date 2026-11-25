pub mod stddef_h {
        pub type size_t = u64;
}
pub mod stdio_h {
    use super::FILE_h::FILE;
    {
                pub static mut stderr: *mut FILE;
                pub fn fprintf(_: *mut FILE, _: *const i8, _: ...) -> i32;
    }
}
pub use self::stddef_h::size_t;

use self::stdio_h::{fprintf, stderr};
pub use self::struct_FILE_h::{_IO_codecvt, _IO_lock_t, _IO_marker, _IO_wide_data, _IO_FILE};

pub use self::FILE_h::FILE;
unsafe fn main_0() -> i32 {
    let mut i: i16 = 1;
    i = ((i as i32) << 14) as i16;
    if i as i32 >> 14 != 1 {
        fprintf(
            stderr(),
            b"i16 isn't 16 bits\n\0" as *const u8 as *const i8,
        );
        return 1;
    }
    if (::core::mem::size_of::<i16>() as u64)
        .wrapping_mul(2)
        != ::core::mem::size_of::<i32>() as u64
    {
        fprintf(
            stderr(),
            b"16*2 != 32\n\0" as *const u8 as *const i8,
        );
        return 1;
    }
    return 0;
}
pub fn main() {
    unsafe { ::std::process::exit(main_0() as i32) }
}
