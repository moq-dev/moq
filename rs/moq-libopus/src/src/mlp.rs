#[derive(Copy, Clone)]
#[repr(C)]
pub struct DenseLayer {
    pub bias: *const i8,
    pub input_weights: *const i8,
    pub nb_inputs: i32,
    pub nb_neurons: i32,
    pub sigmoid: i32,
}
#[derive(Copy, Clone)]
#[repr(C)]
pub struct GRULayer {
    pub bias: *const i8,
    pub input_weights: *const i8,
    pub recurrent_weights: *const i8,
    pub nb_inputs: i32,
    pub nb_neurons: i32,
}
pub const WEIGHTS_SCALE: f32 = 1.0f32 / 128 as f32;

const tansig_table: [f32; 201] = [
    0.000000f32,
    0.039979f32,
    0.079830f32,
    0.119427f32,
    0.158649f32,
    0.197375f32,
    0.235496f32,
    0.272905f32,
    0.309507f32,
    0.345214f32,
    0.379949f32,
    0.413644f32,
    0.446244f32,
    0.477700f32,
    0.507977f32,
    0.537050f32,
    0.564900f32,
    0.591519f32,
    0.616909f32,
    0.641077f32,
    0.664037f32,
    0.685809f32,
    0.706419f32,
    0.725897f32,
    0.744277f32,
    0.761594f32,
    0.777888f32,
    0.793199f32,
    0.807569f32,
    0.821040f32,
    0.833655f32,
    0.845456f32,
    0.856485f32,
    0.866784f32,
    0.876393f32,
    0.885352f32,
    0.893698f32,
    0.901468f32,
    0.908698f32,
    0.915420f32,
    0.921669f32,
    0.927473f32,
    0.932862f32,
    0.937863f32,
    0.942503f32,
    0.946806f32,
    0.950795f32,
    0.954492f32,
    0.957917f32,
    0.961090f32,
    0.964028f32,
    0.966747f32,
    0.969265f32,
    0.971594f32,
    0.973749f32,
    0.975743f32,
    0.977587f32,
    0.979293f32,
    0.980869f32,
    0.982327f32,
    0.983675f32,
    0.984921f32,
    0.986072f32,
    0.987136f32,
    0.988119f32,
    0.989027f32,
    0.989867f32,
    0.990642f32,
    0.991359f32,
    0.992020f32,
    0.992631f32,
    0.993196f32,
    0.993718f32,
    0.994199f32,
    0.994644f32,
    0.995055f32,
    0.995434f32,
    0.995784f32,
    0.996108f32,
    0.996407f32,
    0.996682f32,
    0.996937f32,
    0.997172f32,
    0.997389f32,
    0.997590f32,
    0.997775f32,
    0.997946f32,
    0.998104f32,
    0.998249f32,
    0.998384f32,
    0.998508f32,
    0.998623f32,
    0.998728f32,
    0.998826f32,
    0.998916f32,
    0.999000f32,
    0.999076f32,
    0.999147f32,
    0.999213f32,
    0.999273f32,
    0.999329f32,
    0.999381f32,
    0.999428f32,
    0.999472f32,
    0.999513f32,
    0.999550f32,
    0.999585f32,
    0.999617f32,
    0.999646f32,
    0.999673f32,
    0.999699f32,
    0.999722f32,
    0.999743f32,
    0.999763f32,
    0.999781f32,
    0.999798f32,
    0.999813f32,
    0.999828f32,
    0.999841f32,
    0.999853f32,
    0.999865f32,
    0.999875f32,
    0.999885f32,
    0.999893f32,
    0.999902f32,
    0.999909f32,
    0.999916f32,
    0.999923f32,
    0.999929f32,
    0.999934f32,
    0.999939f32,
    0.999944f32,
    0.999948f32,
    0.999952f32,
    0.999956f32,
    0.999959f32,
    0.999962f32,
    0.999965f32,
    0.999968f32,
    0.999970f32,
    0.999973f32,
    0.999975f32,
    0.999977f32,
    0.999978f32,
    0.999980f32,
    0.999982f32,
    0.999983f32,
    0.999984f32,
    0.999986f32,
    0.999987f32,
    0.999988f32,
    0.999989f32,
    0.999990f32,
    0.999990f32,
    0.999991f32,
    0.999992f32,
    0.999992f32,
    0.999993f32,
    0.999994f32,
    0.999994f32,
    0.999994f32,
    0.999995f32,
    0.999995f32,
    0.999996f32,
    0.999996f32,
    0.999996f32,
    0.999997f32,
    0.999997f32,
    0.999997f32,
    0.999997f32,
    0.999997f32,
    0.999998f32,
    0.999998f32,
    0.999998f32,
    0.999998f32,
    0.999998f32,
    0.999998f32,
    0.999999f32,
    0.999999f32,
    0.999999f32,
    0.999999f32,
    0.999999f32,
    0.999999f32,
    0.999999f32,
    0.999999f32,
    0.999999f32,
    0.999999f32,
    0.999999f32,
    0.999999f32,
    0.999999f32,
    1.000000f32,
    1.000000f32,
    1.000000f32,
    1.000000f32,
    1.000000f32,
    1.000000f32,
    1.000000f32,
    1.000000f32,
    1.000000f32,
    1.000000f32,
    1.000000f32,
];

#[inline]
unsafe fn tansig_approx(mut x: f32) -> f32 {
    let mut i: i32 = 0;
    let mut y: f32 = 0.;
    let mut dy: f32 = 0.;
    let mut sign: f32 = 1 as f32;
    if !(x < 8 as f32) {
        return 1 as f32;
    }
    if !(x > -(8) as f32) {
        return -1 as f32;
    }
    if x != x {
        return 0 as f32;
    }
    if x < 0 as f32 {
        x = -x;
        sign = -1 as f32;
    }
    i = (0.5f32 + 25.0 * x).floor() as i32;
    x -= 0.04f32 * i as f32;
    y = tansig_table[i as usize];
    dy = 1 as f32 - y * y;
    y = y + x * dy * (1 as f32 - y * x);
    return sign * y;
}
#[inline]
unsafe fn sigmoid_approx(x: f32) -> f32 {
    return 0.5f32 + 0.5f32 * tansig_approx(0.5f32 * x);
}
unsafe fn gemm_accum(
    out: *mut f32,
    weights: *const i8,
    rows: i32,
    cols: i32,
    col_stride: i32,
    x: *const f32,
) {
    let mut i: i32 = 0;
    let mut j: i32 = 0;
    i = 0;
    while i < rows {
        j = 0;
        while j < cols {
            *out.offset(i as isize) += *weights.offset((j * col_stride + i) as isize) as i32 as f32
                * *x.offset(j as isize);
            j += 1;
        }
        i += 1;
    }
}
pub unsafe fn compute_dense(layer: *const DenseLayer, output: *mut f32, input: *const f32) {
    let mut i: i32 = 0;
    let mut N: i32 = 0;
    let mut M: i32 = 0;
    let mut stride: i32 = 0;
    M = (*layer).nb_inputs;
    N = (*layer).nb_neurons;
    stride = N;
    i = 0;
    while i < N {
        *output.offset(i as isize) = *((*layer).bias).offset(i as isize) as f32;
        i += 1;
    }
    gemm_accum(output, (*layer).input_weights, N, M, stride, input);
    i = 0;
    while i < N {
        *output.offset(i as isize) *= WEIGHTS_SCALE;
        i += 1;
    }
    if (*layer).sigmoid != 0 {
        i = 0;
        while i < N {
            *output.offset(i as isize) = sigmoid_approx(*output.offset(i as isize));
            i += 1;
        }
    } else {
        i = 0;
        while i < N {
            *output.offset(i as isize) = tansig_approx(*output.offset(i as isize));
            i += 1;
        }
    };
}
pub unsafe fn compute_gru(gru: *const GRULayer, state: *mut f32, input: *const f32) {
    let mut i: i32 = 0;
    let mut N: i32 = 0;
    let mut M: i32 = 0;
    let mut stride: i32 = 0;
    let mut tmp: [f32; 32] = [0.; 32];
    let mut z: [f32; 32] = [0.; 32];
    let mut r: [f32; 32] = [0.; 32];
    let mut h: [f32; 32] = [0.; 32];
    M = (*gru).nb_inputs;
    N = (*gru).nb_neurons;
    stride = 3 * N;
    i = 0;
    while i < N {
        z[i as usize] = *((*gru).bias).offset(i as isize) as f32;
        i += 1;
    }
    gemm_accum(z.as_mut_ptr(), (*gru).input_weights, N, M, stride, input);
    gemm_accum(
        z.as_mut_ptr(),
        (*gru).recurrent_weights,
        N,
        N,
        stride,
        state,
    );
    i = 0;
    while i < N {
        z[i as usize] = sigmoid_approx(WEIGHTS_SCALE * z[i as usize]);
        i += 1;
    }
    i = 0;
    while i < N {
        r[i as usize] = *((*gru).bias).offset((N + i) as isize) as f32;
        i += 1;
    }
    gemm_accum(
        r.as_mut_ptr(),
        &*((*gru).input_weights).offset(N as isize),
        N,
        M,
        stride,
        input,
    );
    gemm_accum(
        r.as_mut_ptr(),
        &*((*gru).recurrent_weights).offset(N as isize),
        N,
        N,
        stride,
        state,
    );
    i = 0;
    while i < N {
        r[i as usize] = sigmoid_approx(WEIGHTS_SCALE * r[i as usize]);
        i += 1;
    }
    i = 0;
    while i < N {
        h[i as usize] = *((*gru).bias).offset((2 * N + i) as isize) as f32;
        i += 1;
    }
    i = 0;
    while i < N {
        tmp[i as usize] = *state.offset(i as isize) * r[i as usize];
        i += 1;
    }
    gemm_accum(
        h.as_mut_ptr(),
        &*((*gru).input_weights).offset((2 * N) as isize),
        N,
        M,
        stride,
        input,
    );
    gemm_accum(
        h.as_mut_ptr(),
        &*((*gru).recurrent_weights).offset((2 * N) as isize),
        N,
        N,
        stride,
        tmp.as_mut_ptr(),
    );
    i = 0;
    while i < N {
        h[i as usize] = z[i as usize] * *state.offset(i as isize)
            + (1 as f32 - z[i as usize]) * tansig_approx(WEIGHTS_SCALE * h[i as usize]);
        i += 1;
    }
    i = 0;
    while i < N {
        *state.offset(i as isize) = h[i as usize];
        i += 1;
    }
}
