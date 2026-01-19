// Vertex shader

struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) tex_coords: vec2<f32>,
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) tex_coords: vec2<f32>,
}

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    output.clip_position = vec4<f32>(input.position, 0.0, 1.0);
    output.tex_coords = input.tex_coords;
    return output;
}

// Fragment shader

@group(0) @binding(0) var y_texture: texture_2d<f32>;
@group(0) @binding(1) var u_texture: texture_2d<f32>;
@group(0) @binding(2) var v_texture: texture_2d<f32>;
@group(0) @binding(3) var tex_sampler: sampler;

// YUV to RGB conversion matrix (BT.709)
const YUV_TO_RGB: mat3x3<f32> = mat3x3<f32>(
    vec3<f32>(1.0,  1.0,     1.0),
    vec3<f32>(0.0, -0.18732, 1.8556),
    vec3<f32>(1.5748, -0.46812, 0.0)
);

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    // Sample YUV planes
    let y = textureSample(y_texture, tex_sampler, input.tex_coords).r;
    let u = textureSample(u_texture, tex_sampler, input.tex_coords).r;
    let v = textureSample(v_texture, tex_sampler, input.tex_coords).r;

    // Convert from [0, 1] range to standard YUV range
    let yuv = vec3<f32>(
        y - 0.0625,         // Y: [16, 235] -> [0, 1]
        u - 0.5,            // U: [16, 240] -> [-0.5, 0.5]
        v - 0.5             // V: [16, 240] -> [-0.5, 0.5]
    );

    // Convert YUV to RGB using BT.709 matrix
    let rgb = YUV_TO_RGB * yuv;

    // Clamp to valid range
    let clamped_rgb = clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0));

    return vec4<f32>(clamped_rgb, 1.0);
}
