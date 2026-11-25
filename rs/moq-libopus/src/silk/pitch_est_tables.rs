pub const PE_NB_CBKS_STAGE3_MIN: i32 = 16;
pub const PE_NB_CBKS_STAGE3_MID: i32 = 24;
pub const PE_NB_CBKS_STAGE3_MAX: i32 = 34;
pub const PE_NB_CBKS_STAGE3_10MS: i32 = 12;
pub const PE_NB_STAGE3_LAGS: i32 = 5;
pub const PE_NB_CBKS_STAGE2_EXT: i32 = 11;
pub const PE_NB_CBKS_STAGE2_10MS: i32 = 3;
pub const PE_MAX_NB_SUBFR: i32 = 4;
pub const PE_SUBFR_LENGTH_MS: i32 = 5;
pub const PE_LTP_MEM_LENGTH_MS: i32 = 4 * PE_SUBFR_LENGTH_MS;
pub const PE_MAX_LAG_MS: i32 = 18;
pub const PE_MIN_LAG_MS: i32 = 2;
pub const PE_NB_CBKS_STAGE2: i32 = 3;

pub const SILK_PE_MIN_COMPLEX: i32 = 0;
pub const SILK_PE_MID_COMPLEX: i32 = 1;
pub const SILK_PE_MAX_COMPLEX: i32 = 2;

pub const PE_SHORTLAG_BIAS: f32 = 0.2f32;
pub const PE_PREVLAG_BIAS: f32 = 0.2f32;
pub const PE_FLATCONTOUR_BIAS: f32 = 0.05f32;

pub static silk_CB_lags_stage2_10_ms: [[i8; 3]; 2] = [[0, 1, 0], [0, 0, 1]];
pub static silk_CB_lags_stage3_10_ms: [[i8; 12]; 2] = [
    [0, 0, 1, -1, 1, -1, 2, -2, 2, -2, 3, -3],
    [0, 1, 0, 1, -1, 2, -1, 2, -2, 3, -2, 3],
];
pub static silk_Lag_range_stage3_10_ms: [[i8; 2]; 2] = [[-3, 7], [-2, 7]];
pub static silk_CB_lags_stage2: [[i8; 11]; 4] = [
    [0, 2, -1, -1, -1, 0, 0, 1, 1, 0, 1],
    [0, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0],
    [0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0],
    [0, -1, 2, 1, 0, 1, 1, 0, 0, -1, -1],
];
pub static silk_CB_lags_stage3: [[i8; 34]; 4] = [
    [
        0, 0, 1, -1, 0, 1, -1, 0, -1, 1, -2, 2, -2, -2, 2, -3, 2, 3, -3, -4, 3, -4, 4, 4, -5, 5,
        -6, -5, 6, -7, 6, 5, 8, -9,
    ],
    [
        0, 0, 1, 0, 0, 0, 0, 0, 0, 0, -1, 1, 0, 0, 1, -1, 0, 1, -1, -1, 1, -1, 2, 1, -1, 2, -2, -2,
        2, -2, 2, 2, 3, -3,
    ],
    [
        0, 1, 0, 0, 0, 0, 0, 0, 1, 0, 1, 0, 0, 1, -1, 1, 0, 0, 2, 1, -1, 2, -1, -1, 2, -1, 2, 2,
        -1, 3, -2, -2, -2, 3,
    ],
    [
        0, 1, 0, 0, 1, 0, 1, -1, 2, -1, 2, -1, 2, 3, -2, 3, -2, -2, 4, 4, -3, 5, -3, -4, 6, -4, 6,
        5, -5, 8, -6, -5, -7, 9,
    ],
];
pub static silk_Lag_range_stage3: [[[i8; 2]; 4]; 3] = [
    [[-5, 8], [-1, 6], [-1, 6], [-4, 10]],
    [[-6, 10], [-2, 6], [-1, 6], [-5, 10]],
    [[-9, 12], [-3, 7], [-2, 7], [-7, 13]],
];
pub static silk_nb_cbk_searchs_stage3: [i8; 3] = [
    PE_NB_CBKS_STAGE3_MIN as i8,
    PE_NB_CBKS_STAGE3_MID as i8,
    PE_NB_CBKS_STAGE3_MAX as i8,
];
