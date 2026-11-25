use crate::silk::float::energy_FLP::silk_energy_FLP;
use crate::silk::float::inner_product_FLP::silk_inner_product_FLP;

pub unsafe fn silk_corrVector_FLP(x: *const f32, t: *const f32, L: i32, Order: i32, Xt: *mut f32) {
    let mut lag: i32 = 0;
    let mut ptr1: *const f32 = 0 as *const f32;
    ptr1 = &*x.offset((Order - 1) as isize) as *const f32;
    lag = 0;
    while lag < Order {
        *Xt.offset(lag as isize) = silk_inner_product_FLP(ptr1, t, L) as f32;
        ptr1 = ptr1.offset(-1);
        lag += 1;
    }
}
pub unsafe fn silk_corrMatrix_FLP(x: *const f32, L: i32, Order: i32, XX: *mut f32) {
    let mut j: i32 = 0;
    let mut lag: i32 = 0;
    let mut energy: f64 = 0.;
    let mut ptr1: *const f32 = 0 as *const f32;
    let mut ptr2: *const f32 = 0 as *const f32;
    ptr1 = &*x.offset((Order - 1) as isize) as *const f32;
    energy = silk_energy_FLP(ptr1, L);
    *XX.offset((0 * Order + 0) as isize) = energy as f32;
    j = 1;
    while j < Order {
        energy += (*ptr1.offset(-j as isize) * *ptr1.offset(-j as isize)
            - *ptr1.offset((L - j) as isize) * *ptr1.offset((L - j) as isize))
            as f64;
        *XX.offset((j * Order + j) as isize) = energy as f32;
        j += 1;
    }
    ptr2 = &*x.offset((Order - 2) as isize) as *const f32;
    lag = 1;
    while lag < Order {
        energy = silk_inner_product_FLP(ptr1, ptr2, L);
        *XX.offset((lag * Order + 0) as isize) = energy as f32;
        *XX.offset((0 * Order + lag) as isize) = energy as f32;
        j = 1;
        while j < Order - lag {
            energy += (*ptr1.offset(-j as isize) * *ptr2.offset(-j as isize)
                - *ptr1.offset((L - j) as isize) * *ptr2.offset((L - j) as isize))
                as f64;
            *XX.offset(((lag + j) * Order + j) as isize) = energy as f32;
            *XX.offset((j * Order + (lag + j)) as isize) = energy as f32;
            j += 1;
        }
        ptr2 = ptr2.offset(-1);
        lag += 1;
    }
}
