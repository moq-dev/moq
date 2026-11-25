use crate::silk::float::inner_product_FLP::silk_inner_product_FLP;

pub unsafe fn silk_autocorrelation_FLP(
    results: *mut f32,
    inputData: *const f32,
    inputDataSize: i32,
    mut correlationCount: i32,
) {
    let mut i: i32 = 0;
    if correlationCount > inputDataSize {
        correlationCount = inputDataSize;
    }
    i = 0;
    while i < correlationCount {
        *results.offset(i as isize) =
            silk_inner_product_FLP(inputData, inputData.offset(i as isize), inputDataSize - i)
                as f32;
        i += 1;
    }
}
