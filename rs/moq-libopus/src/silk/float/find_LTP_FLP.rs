use crate::silk::define::LTP_ORDER;
use crate::silk::float::corrMatrix_FLP::{silk_corrMatrix_FLP, silk_corrVector_FLP};
use crate::silk::float::energy_FLP::silk_energy_FLP;
use crate::silk::float::scale_vector_FLP::silk_scale_vector_FLP;

pub unsafe fn silk_find_LTP_FLP(
    XX: *mut f32,
    xX: *mut f32,
    mut r_ptr: *const f32,
    lag: *const i32,
    subfr_length: i32,
    nb_subfr: i32,
) {
    let mut k: i32 = 0;
    let mut xX_ptr: *mut f32 = 0 as *mut f32;
    let mut XX_ptr: *mut f32 = 0 as *mut f32;
    let mut lag_ptr: *const f32 = 0 as *const f32;
    let mut xx: f32 = 0.;
    let mut temp: f32 = 0.;
    xX_ptr = xX;
    XX_ptr = XX;
    k = 0;
    while k < nb_subfr {
        lag_ptr = r_ptr.offset(-((*lag.offset(k as isize) + LTP_ORDER / 2) as isize));
        silk_corrMatrix_FLP(lag_ptr, subfr_length, LTP_ORDER, XX_ptr);
        silk_corrVector_FLP(lag_ptr, r_ptr, subfr_length, LTP_ORDER, xX_ptr);
        xx = silk_energy_FLP(r_ptr, subfr_length + LTP_ORDER) as f32;
        temp = 1.0f32
            / (if xx
                > 0.03f32 * 0.5f32 * (*XX_ptr.offset(0 as isize) + *XX_ptr.offset(24 as isize))
                    + 1.0f32
            {
                xx
            } else {
                0.03f32 * 0.5f32 * (*XX_ptr.offset(0 as isize) + *XX_ptr.offset(24 as isize))
                    + 1.0f32
            });
        silk_scale_vector_FLP(XX_ptr, temp, LTP_ORDER * LTP_ORDER);
        silk_scale_vector_FLP(xX_ptr, temp, LTP_ORDER);
        r_ptr = r_ptr.offset(subfr_length as isize);
        XX_ptr = XX_ptr.offset((LTP_ORDER * LTP_ORDER) as isize);
        xX_ptr = xX_ptr.offset(LTP_ORDER as isize);
        k += 1;
    }
}
