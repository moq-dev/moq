use crate::silk::define::{OFFSET_UVH_Q10, OFFSET_UVL_Q10, OFFSET_VH_Q10, OFFSET_VL_Q10};

pub static silk_stereo_pred_quant_Q13: [i16; 16] = [
    -13732, -10050, -8266, -7526, -6500, -5000, -2950, -820, 820, 2950, 5000, 6500, 7526, 8266,
    10050, 13732,
];
pub static silk_stereo_pred_joint_iCDF: [u8; 25] = [
    249, 247, 246, 245, 244, 234, 210, 202, 201, 200, 197, 174, 82, 59, 56, 55, 54, 46, 22, 12, 11,
    10, 9, 7, 0,
];
pub static silk_stereo_only_code_mid_iCDF: [u8; 2] = [64, 0];
static silk_LBRR_flags_2_iCDF: [u8; 3] = [203, 150, 0];
static silk_LBRR_flags_3_iCDF: [u8; 7] = [215, 195, 166, 125, 110, 82, 0];
pub static silk_LBRR_flags_iCDF_ptr: [&[u8]; 2] =
    [&silk_LBRR_flags_2_iCDF, &silk_LBRR_flags_3_iCDF];
pub static silk_lsb_iCDF: [u8; 2] = [120, 0];
pub static silk_LTPscale_iCDF: [u8; 3] = [128, 64, 0];
pub static silk_type_offset_VAD_iCDF: [u8; 4] = [232, 158, 10, 0];
pub static silk_type_offset_no_VAD_iCDF: [u8; 2] = [230, 0];
pub static silk_NLSF_interpolation_factor_iCDF: [u8; 5] = [243, 221, 192, 181, 0];
pub static silk_Quantization_Offsets_Q10: [[i16; 2]; 2] = [
    [OFFSET_UVL_Q10 as i16, OFFSET_UVH_Q10 as i16],
    [OFFSET_VL_Q10 as i16, OFFSET_VH_Q10 as i16],
];
pub static silk_LTPScales_table_Q14: [i16; 3] = [15565, 12288, 8192];
pub static silk_uniform3_iCDF: [u8; 3] = [171, 85, 0];
pub static silk_uniform4_iCDF: [u8; 4] = [192, 128, 64, 0];
pub static silk_uniform5_iCDF: [u8; 5] = [205, 154, 102, 51, 0];
pub static silk_uniform6_iCDF: [u8; 6] = [213, 171, 128, 85, 43, 0];
pub static silk_uniform8_iCDF: [u8; 8] = [224, 192, 160, 128, 96, 64, 32, 0];
pub static silk_NLSF_EXT_iCDF: [u8; 7] = [100, 40, 16, 7, 3, 1, 0];
pub static silk_Transition_LP_B_Q28: [[i32; 3]; 5] = [
    [250767114, 501534038, 250767114],
    [209867381, 419732057, 209867381],
    [170987846, 341967853, 170987846],
    [131531482, 263046905, 131531482],
    [89306658, 178584282, 89306658],
];
pub static silk_Transition_LP_A_Q28: [[i32; 2]; 5] = [
    [506393414, 239854379],
    [411067935, 169683996],
    [306733530, 116694253],
    [185807084, 77959395],
    [35497197, 57401098],
];
