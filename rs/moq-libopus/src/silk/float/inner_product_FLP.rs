pub unsafe fn silk_inner_product_FLP(data1: *const f32, data2: *const f32, dataSize: i32) -> f64 {
    let mut i: i32 = 0;
    let mut result: f64 = 0.;
    result = 0.0f64;
    i = 0;
    while i < dataSize - 3 {
        result += *data1.offset((i + 0) as isize) as f64 * *data2.offset((i + 0) as isize) as f64
            + *data1.offset((i + 1) as isize) as f64 * *data2.offset((i + 1) as isize) as f64
            + *data1.offset((i + 2) as isize) as f64 * *data2.offset((i + 2) as isize) as f64
            + *data1.offset((i + 3) as isize) as f64 * *data2.offset((i + 3) as isize) as f64;
        i += 4;
    }
    while i < dataSize {
        result += *data1.offset(i as isize) as f64 * *data2.offset(i as isize) as f64;
        i += 1;
    }
    return result;
}
