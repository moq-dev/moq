//! Zero-copy `CMSampleBuffer` -> [`Frame::PixelBuffer`] extraction, shared by the
//! AVFoundation (camera) and ScreenCaptureKit (screen) backends.

use objc2_core_foundation::CFRetained;
use objc2_core_media::CMSampleBuffer;
use objc2_core_video::{CVImageBuffer, CVPixelBuffer, CVPixelBufferGetHeight, CVPixelBufferGetWidth};

use crate::frame::Frame;
use crate::frame::macos::PixelBuffer;

/// Extract the `CVPixelBuffer` from a sample buffer as a zero-copy surface.
pub(super) fn surface_frame(sample_buffer: &CMSampleBuffer) -> Option<Frame> {
	let image: CFRetained<CVImageBuffer> = unsafe { sample_buffer.image_buffer() }?;
	// CVImageBufferRef and CVPixelBufferRef are the same object for video; the
	// retain carries over with the reinterpret.
	let pixel: CFRetained<CVPixelBuffer> = unsafe { CFRetained::from_raw(CFRetained::into_raw(image).cast()) };
	let width = CVPixelBufferGetWidth(&pixel) as u32;
	let height = CVPixelBufferGetHeight(&pixel) as u32;
	Some(Frame::PixelBuffer(PixelBuffer::new(pixel, width, height)))
}
