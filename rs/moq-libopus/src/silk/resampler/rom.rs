pub const RESAMPLER_DOWN_ORDER_FIR2: usize = 36;
pub const RESAMPLER_DOWN_ORDER_FIR1: usize = 24;
pub const RESAMPLER_DOWN_ORDER_FIR0: usize = 18;
pub const RESAMPLER_ORDER_FIR_12: usize = 8;

/* Tables for 2x downsampler */
pub const silk_resampler_down2_0: i16 = 9872;
pub const silk_resampler_down2_1: i16 = (39809 - 65536) as i16;

/* Tables for 2x upsampler, high quality */
pub const silk_resampler_up2_hq_0: [i16; 3] = [1746, 14986, (39083 - 65536) as i16];
pub const silk_resampler_up2_hq_1: [i16; 3] = [6854, 25769, (55542 - 65536) as i16];

/* Matlab code for the notch filter coefficients: */
/* B = [1, 0.147, 1];  A = [1, 0.107, 0.89]; G = 0.93; freqz(G * B, A, 2^14, 16e3); axis([0, 8000, -10, 1]) */
/* fprintf('\t%6d, %6d, %6d, %6d\n', round(B(2)*2^16), round(-A(2)*2^16), round((1-A(3))*2^16), round(G*2^15)) */
/* const opus_int16 silk_resampler_up2_hq_notch[ 4 ] = { 9634,  -7012,   7209,  30474 }; */

/* Tables with IIR and FIR coefficients for fractional downsamplers (123 Words) */

pub static silk_Resampler_3_4_COEFS: [i16; 29] = [
    -20694, -13867, -49, 64, 17, -157, 353, -496, 163, 11047, 22205, -39, 6, 91, -170, 186, 23,
    -896, 6336, 19928, -19, -36, 102, -89, -24, 328, -951, 2568, 15909,
];
pub static silk_Resampler_2_3_COEFS: [i16; 20] = [
    -14457, -14019, 64, 128, -122, 36, 310, -768, 584, 9267, 17733, 12, 128, 18, -142, 288, -117,
    -865, 4123, 14459,
];
pub static silk_Resampler_1_2_COEFS: [i16; 14] = [
    616, -14323, -10, 39, 58, -46, -84, 120, 184, -315, -541, 1284, 5380, 9024,
];
pub static silk_Resampler_1_3_COEFS: [i16; 20] = [
    16102, -15162, -13, 0, 20, 26, 5, -31, -43, -4, 65, 90, 7, -157, -248, -44, 593, 1583, 2612,
    3271,
];
pub static silk_Resampler_1_4_COEFS: [i16; 20] = [
    22500, -15099, 3, -14, -20, -15, 2, 25, 37, 25, -16, -71, -107, -79, 50, 292, 623, 982, 1288,
    1464,
];
pub static silk_Resampler_1_6_COEFS: [i16; 20] = [
    27540, -15257, 17, 12, 8, 1, -10, -22, -30, -32, -22, 3, 44, 100, 168, 243, 317, 381, 429, 455,
];
pub static silk_Resampler_2_3_COEFS_LQ: [i16; 6] = [-2797, -6507, 4697, 10739, 1567, 8276];

/* Table with interplation fractions of 1/24, 3/24, ..., 23/24 */
pub static silk_resampler_frac_FIR_12: [[i16; 4]; 12] = [
    [189, -600, 617, 30567],
    [117, -159, -1070, 29704],
    [52, 221, -2392, 28276],
    [-4, 529, -3350, 26341],
    [-48, 758, -3956, 23973],
    [-80, 905, -4235, 21254],
    [-99, 972, -4222, 18278],
    [-107, 967, -3957, 15143],
    [-103, 896, -3487, 11950],
    [-91, 773, -2865, 8798],
    [-71, 611, -2143, 5784],
    [-46, 425, -1375, 2996],
];
