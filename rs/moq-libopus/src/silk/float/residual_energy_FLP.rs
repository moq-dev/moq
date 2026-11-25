use crate::silk::define::MAX_NB_SUBFR;
use crate::silk::float::energy_FLP::silk_energy_FLP;
use crate::silk::float::LPC_analysis_filter_FLP::silk_LPC_analysis_filter_FLP;

pub unsafe fn silk_residual_energy_FLP(
    nrgs: *mut f32,
    x: *const f32,
    a: *mut [f32; 16],
    gains: *const f32,
    subfr_length: i32,
    nb_subfr: i32,
    LPC_order: i32,
) {
    let mut shift: i32 = 0;
    let mut LPC_res_ptr: *mut f32 = 0 as *mut f32;
    let mut LPC_res: [f32; 192] = [0.; 192];
    LPC_res_ptr = LPC_res.as_mut_ptr().offset(LPC_order as isize);
    shift = LPC_order + subfr_length;
    silk_LPC_analysis_filter_FLP(
        LPC_res.as_mut_ptr(),
        (*a.offset(0 as isize)).as_mut_ptr() as *const f32,
        x.offset((0 * shift) as isize),
        2 * shift,
        LPC_order,
    );
    *nrgs.offset(0 as isize) = ((*gains.offset(0 as isize) * *gains.offset(0 as isize)) as f64
        * silk_energy_FLP(LPC_res_ptr.offset((0 * shift) as isize), subfr_length))
        as f32;
    *nrgs.offset(1 as isize) = ((*gains.offset(1 as isize) * *gains.offset(1 as isize)) as f64
        * silk_energy_FLP(LPC_res_ptr.offset((1 * shift) as isize), subfr_length))
        as f32;
    if nb_subfr == MAX_NB_SUBFR {
        silk_LPC_analysis_filter_FLP(
            LPC_res.as_mut_ptr(),
            (*a.offset(1 as isize)).as_mut_ptr() as *const f32,
            x.offset((2 * shift) as isize),
            2 * shift,
            LPC_order,
        );
        *nrgs.offset(2 as isize) = ((*gains.offset(2 as isize) * *gains.offset(2 as isize)) as f64
            * silk_energy_FLP(LPC_res_ptr.offset((0 * shift) as isize), subfr_length))
            as f32;
        *nrgs.offset(3 as isize) = ((*gains.offset(3 as isize) * *gains.offset(3 as isize)) as f64
            * silk_energy_FLP(LPC_res_ptr.offset((1 * shift) as isize), subfr_length))
            as f32;
    }
}
