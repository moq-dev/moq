pub unsafe fn silk_scale_copy_vector_FLP(
    data_out: *mut f32,
    data_in: *const f32,
    gain: f32,
    dataSize: i32,
) {
    let mut i: i32 = 0;
    let mut dataSize4: i32 = 0;
    dataSize4 = dataSize & 0xfffc;
    i = 0;
    while i < dataSize4 {
        *data_out.offset((i + 0) as isize) = gain * *data_in.offset((i + 0) as isize);
        *data_out.offset((i + 1) as isize) = gain * *data_in.offset((i + 1) as isize);
        *data_out.offset((i + 2) as isize) = gain * *data_in.offset((i + 2) as isize);
        *data_out.offset((i + 3) as isize) = gain * *data_in.offset((i + 3) as isize);
        i += 4;
    }
    while i < dataSize {
        *data_out.offset(i as isize) = gain * *data_in.offset(i as isize);
        i += 1;
    }
}
