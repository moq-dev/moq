// YUV 4:2:0 -> RGB, for the two plane layouts the renderer feeds it: NV12 (luma
// plane + interleaved chroma plane) and I420 (three separate planes).
//
// Chroma is sampled with a linear filter, so the half-resolution planes are
// upsampled by the texture unit rather than in here. Output is gamma-encoded
// R'G'B', the same encoding the samples arrived in: this converts between color
// models, not between transfer functions.

struct Params {
	// Columns multiply Y, U and V respectively.
	matrix: mat3x3<f32>,
	// Subtracted from (Y, U, V) before the multiply: the black level and the
	// chroma midpoint.
	offset: vec4<f32>,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var plane0: texture_2d<f32>;
@group(0) @binding(3) var plane1: texture_2d<f32>;
// Unused by the NV12 entry point, which binds plane1 here as filler so one bind
// group layout serves both.
@group(0) @binding(4) var plane2: texture_2d<f32>;

struct Vertex {
	@builtin(position) position: vec4<f32>,
	@location(0) uv: vec2<f32>,
}

// A single oversized triangle covering the viewport. Cheaper than a quad (no
// index buffer, no diagonal seam) and it needs no vertex buffer at all.
@vertex
fn vertex(@builtin(vertex_index) index: u32) -> Vertex {
	var out: Vertex;
	let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
	out.uv = uv;
	out.position = vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
	return out;
}

fn convert(yuv: vec3<f32>) -> vec4<f32> {
	let rgb = params.matrix * (yuv - params.offset.xyz);
	return vec4<f32>(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}

@fragment
fn nv12(in: Vertex) -> @location(0) vec4<f32> {
	let y = textureSample(plane0, samp, in.uv).r;
	// One two-channel plane holds U and V interleaved.
	let uv = textureSample(plane1, samp, in.uv).rg;
	return convert(vec3<f32>(y, uv.r, uv.g));
}

@fragment
fn i420(in: Vertex) -> @location(0) vec4<f32> {
	let y = textureSample(plane0, samp, in.uv).r;
	let u = textureSample(plane1, samp, in.uv).r;
	let v = textureSample(plane2, samp, in.uv).r;
	return convert(vec3<f32>(y, u, v));
}
