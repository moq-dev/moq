use crate::sys::v4l2_ext_control;

// We need to carry our own copy of this struct, because the `which` field used to be called
// `ctrl_class` and Linux now has both fields in a union. While the change is transparent as far as
// C (and thus Linux) is concerned, it causes us trouble with bindgen since we cannot directly
// access union members.
//
// See https://github.com/raymanfx/libv4l-rs/pull/40#issuecomment-885169894 for additional details.
#[repr(C)]
pub(crate) struct v4l2_ext_controls {
	pub which: u32,
	pub count: u32,
	pub error_idx: u32,
	pub request_fd: i32,
	pub reserved: u32,
	pub controls: *mut v4l2_ext_control,
}
