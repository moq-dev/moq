// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Wrappers and helpers around `VABuffer`s.

mod enc_misc;
mod h264;
mod hevc;
mod proc_pipeline;

pub use enc_misc::*;
pub use h264::*;
pub use hevc::*;
pub use proc_pipeline::*;

use std::rc::Rc;

use tracing::error;

use crate::bindings;
use crate::va_check;
use crate::Context;
use crate::VaError;

/// Wrapper type representing a buffer created with `vaCreateBuffer`.
pub struct Buffer {
	context: Rc<Context>,
	id: bindings::VABufferID,
}

impl Buffer {
	/// Creates a new buffer by wrapping a `vaCreateBuffer` call. This is just a helper for
	/// [`Context::create_buffer`].
	pub(crate) fn new(context: Rc<Context>, mut type_: BufferType) -> Result<Self, VaError> {
		let mut buffer_id = 0;

		/* we send all slices parameters as a single array in H264 */
		let nb_elements = match type_ {
			BufferType::SliceParameter(SliceParameter::H264(ref mut params)) => params.inner_mut().len(),
			_ => 1,
		};

		let (ptr, size) = match type_ {
			BufferType::PictureParameter(ref mut picture_param) => match picture_param {
				PictureParameter::H264(ref mut wrapper) => (
					wrapper.inner_mut() as *mut _ as *mut std::ffi::c_void,
					std::mem::size_of_val(wrapper.inner_mut()),
				),
				PictureParameter::HEVC(ref mut wrapper) => (
					wrapper.inner_mut() as *mut _ as *mut std::ffi::c_void,
					std::mem::size_of_val(wrapper.inner_mut()),
				),
				PictureParameter::HEVCRext(ref mut wrapper) => (
					wrapper.inner_mut() as *mut _ as *mut std::ffi::c_void,
					std::mem::size_of_val(wrapper.inner_mut()),
				),
				PictureParameter::HEVCScc(ref mut wrapper) => (
					wrapper.inner_mut() as *mut _ as *mut std::ffi::c_void,
					std::mem::size_of_val(wrapper.inner_mut()),
				),
			},

			BufferType::SliceParameter(ref mut slice_param) => match slice_param {
				SliceParameter::H264(ref mut wrapper) => (
					wrapper.inner_mut().as_mut_ptr() as *mut std::ffi::c_void,
					std::mem::size_of::<bindings::VASliceParameterBufferH264>(),
				),
				SliceParameter::HEVC(ref mut wrapper) => (
					wrapper.inner_mut() as *mut _ as *mut std::ffi::c_void,
					std::mem::size_of_val(wrapper.inner_mut()),
				),
				SliceParameter::HEVCRext(ref mut wrapper) => (
					wrapper.inner_mut() as *mut _ as *mut std::ffi::c_void,
					std::mem::size_of_val(wrapper.inner_mut()),
				),
			},

			BufferType::IQMatrix(ref mut iq_matrix) => match iq_matrix {
				IQMatrix::H264(ref mut wrapper) => (
					wrapper.inner_mut() as *mut _ as *mut std::ffi::c_void,
					std::mem::size_of_val(wrapper.inner_mut()),
				),
				IQMatrix::HEVC(ref mut wrapper) => (
					wrapper.inner_mut() as *mut _ as *mut std::ffi::c_void,
					std::mem::size_of_val(wrapper.inner_mut()),
				),
			},

			BufferType::SliceData(ref mut data) => (data.as_mut_ptr() as *mut std::ffi::c_void, data.len()),

			BufferType::EncSequenceParameter(ref mut seq_param) => match seq_param {
				EncSequenceParameter::H264(ref mut wrapper) => (
					wrapper.inner_mut() as *mut _ as *mut std::ffi::c_void,
					std::mem::size_of_val(wrapper.inner_mut()),
				),
				EncSequenceParameter::HEVC(ref mut wrapper) => (
					wrapper.inner_mut() as *mut _ as *mut std::ffi::c_void,
					std::mem::size_of_val(wrapper.inner_mut()),
				),
			},

			BufferType::EncPictureParameter(ref mut picture_param) => match picture_param {
				EncPictureParameter::H264(ref mut wrapper) => (
					wrapper.inner_mut() as *mut _ as *mut std::ffi::c_void,
					std::mem::size_of_val(wrapper.inner_mut()),
				),
				EncPictureParameter::HEVC(ref mut wrapper) => (
					wrapper.inner_mut() as *mut _ as *mut std::ffi::c_void,
					std::mem::size_of_val(wrapper.inner_mut()),
				),
			},

			BufferType::EncSliceParameter(ref mut slice_param) => match slice_param {
				EncSliceParameter::H264(ref mut wrapper) => (
					wrapper.inner_mut() as *mut _ as *mut std::ffi::c_void,
					std::mem::size_of_val(wrapper.inner_mut()),
				),
				EncSliceParameter::HEVC(ref mut wrapper) => (
					wrapper.inner_mut() as *mut _ as *mut std::ffi::c_void,
					std::mem::size_of_val(wrapper.inner_mut()),
				),
			},

			BufferType::EncMacroblockParameterBuffer(ref mut mb_param) => match mb_param {
				EncMacroblockParameterBuffer::H264(ref mut wrapper) => (
					wrapper.inner_mut() as *mut _ as *mut std::ffi::c_void,
					std::mem::size_of_val(wrapper.inner_mut()),
				),
			},

			BufferType::EncCodedBuffer(size) => (std::ptr::null_mut(), size),

			BufferType::EncMiscParameter(ref mut enc_misc_param) => match enc_misc_param {
				EncMiscParameter::FrameRate(ref mut wrapper) => (
					wrapper.inner_mut() as *mut _ as *mut std::ffi::c_void,
					std::mem::size_of_val(wrapper.inner_mut()),
				),
				EncMiscParameter::RateControl(ref mut wrapper) => (
					wrapper.inner_mut() as *mut _ as *mut std::ffi::c_void,
					std::mem::size_of_val(wrapper.inner_mut()),
				),
				EncMiscParameter::MaxSliceSize(ref mut wrapper) => (
					wrapper.inner_mut() as *mut _ as *mut std::ffi::c_void,
					std::mem::size_of_val(wrapper.inner_mut()),
				),
				EncMiscParameter::MaxFrameSize(ref mut wrapper) => (
					wrapper.inner_mut() as *mut _ as *mut std::ffi::c_void,
					std::mem::size_of_val(wrapper.inner_mut()),
				),
				EncMiscParameter::SkipFrame(ref mut wrapper) => (
					wrapper.inner_mut() as *mut _ as *mut std::ffi::c_void,
					std::mem::size_of_val(wrapper.inner_mut()),
				),
				EncMiscParameter::HRD(ref mut wrapper) => (
					wrapper.inner_mut() as *mut _ as *mut std::ffi::c_void,
					std::mem::size_of_val(wrapper.inner_mut()),
				),
				EncMiscParameter::QualityLevel(ref mut wrapper) => (
					wrapper.inner_mut() as *mut _ as *mut std::ffi::c_void,
					std::mem::size_of_val(wrapper.inner_mut()),
				),
				EncMiscParameter::Quantization(ref mut wrapper) => (
					wrapper.inner_mut() as *mut _ as *mut std::ffi::c_void,
					std::mem::size_of_val(wrapper.inner_mut()),
				),
			},
			BufferType::ProcPipelineParameter(ref mut proc_pipeline_param) => (
				proc_pipeline_param.inner_mut() as *mut _ as *mut std::ffi::c_void,
				std::mem::size_of_val(proc_pipeline_param.inner_mut()),
			),
			BufferType::EncPackedHeaderParameter(ref mut wrapper) => (
				wrapper.inner_mut() as *mut _ as *mut std::ffi::c_void,
				std::mem::size_of_val(wrapper.inner_mut()),
			),

			BufferType::EncPackedHeaderData(ref data) => (data.as_ptr() as *mut std::ffi::c_void, data.len()),
		};

		// Safe because `self` represents a valid `VAContext`. `ptr` and `size` are also ensured to
		// be correct, as `ptr` is just a cast to `*c_void` from a Rust struct, and `size` is
		// computed from `std::mem::size_of_val`.
		va_check(unsafe {
			bindings::va().vaCreateBuffer(
				context.display().handle(),
				context.id(),
				type_.inner(),
				size as u32,
				nb_elements as u32,
				ptr,
				&mut buffer_id,
			)
		})?;

		Ok(Self { context, id: buffer_id })
	}

	/// Convenience function to return a `VABufferID` vector from a slice of `Buffer`s in order to
	/// easily interface with the C API where a buffer array might be needed.
	pub fn as_id_vec(buffers: &[Self]) -> Vec<bindings::VABufferID> {
		buffers.iter().map(|buffer| buffer.id).collect()
	}
}

impl Drop for Buffer {
	fn drop(&mut self) {
		// Safe because `self` represents a valid buffer, created with
		// vaCreateBuffers.
		let status = va_check(unsafe { bindings::va().vaDestroyBuffer(self.context.display().handle(), self.id) });

		if status.is_err() {
			error!("vaDestroyBuffer failed: {}", status.unwrap_err());
		}
	}
}

/// Abstraction over `VABufferType`s.
pub enum BufferType {
	/// Abstraction over `VAPictureParameterBufferType`. Needed for H264, HEVC.
	PictureParameter(PictureParameter),
	/// Abstraction over `VASliceParameterBufferType`. Needed for H264, HEVC.
	SliceParameter(SliceParameter),
	/// Abstraction over `VAIQMatrixBufferType`. Needed for H264, HEVC.
	IQMatrix(IQMatrix),
	/// Abstraction over `VASliceDataBufferType`. Needed for H264, HEVC.
	SliceData(Vec<u8>),
	/// Abstraction over `VAEncSequenceParameterBufferType`. Needed for H264, HEVC.
	EncSequenceParameter(EncSequenceParameter),
	/// Abstraction over `VAEncPictureParameterBufferType`. Needed for H264, HEVC.
	EncPictureParameter(EncPictureParameter),
	/// Abstraction over `VAEncSliceParameterBufferType`. Needed for H264, HEVC.
	EncSliceParameter(EncSliceParameter),
	/// Abstraction over `VAEncMacroblockMapBufferType`. Needed for H264.
	EncMacroblockParameterBuffer(EncMacroblockParameterBuffer),
	/// Abstraction over `VAEncCodedBufferType`. Needed for H264, HEVC.
	EncCodedBuffer(usize),
	/// Abstraction over `VAEncMiscParameterBuffer`.
	EncMiscParameter(EncMiscParameter),
	/// Abstraction over `VAProcPipelineParameterBuffer`.
	ProcPipelineParameter(proc_pipeline::ProcPipelineParameterBuffer),
	/// Abstraction over `VAEncPackedHeaderParameterBufferType`.
	EncPackedHeaderParameter(EncPackedHeaderParameter),
	/// Abstraction over `VAEncPackedHeaderDataBufferType`.
	EncPackedHeaderData(Vec<u8>),
}

impl BufferType {
	/// Returns the inner FFI buffer type.
	pub(crate) fn inner(&self) -> bindings::VABufferType::Type {
		match self {
			BufferType::PictureParameter(_) => bindings::VABufferType::VAPictureParameterBufferType,
			BufferType::SliceParameter(_) => bindings::VABufferType::VASliceParameterBufferType,
			BufferType::IQMatrix(_) => bindings::VABufferType::VAIQMatrixBufferType,
			BufferType::SliceData { .. } => bindings::VABufferType::VASliceDataBufferType,

			BufferType::EncSequenceParameter(_) => bindings::VABufferType::VAEncSequenceParameterBufferType,

			BufferType::EncPictureParameter(_) => bindings::VABufferType::VAEncPictureParameterBufferType,

			BufferType::EncSliceParameter(_) => bindings::VABufferType::VAEncSliceParameterBufferType,

			BufferType::EncMacroblockParameterBuffer(_) => bindings::VABufferType::VAEncMacroblockMapBufferType,

			BufferType::EncCodedBuffer(_) => bindings::VABufferType::VAEncCodedBufferType,

			BufferType::EncMiscParameter(_) => bindings::VABufferType::VAEncMiscParameterBufferType,

			BufferType::ProcPipelineParameter(_) => bindings::VABufferType::VAProcPipelineParameterBufferType,

			BufferType::EncPackedHeaderParameter(_) => bindings::VABufferType::VAEncPackedHeaderParameterBufferType,

			BufferType::EncPackedHeaderData(_) => bindings::VABufferType::VAEncPackedHeaderDataBufferType,
		}
	}
}

/// Abstraction over the `PictureParameterBuffer` types we support.
pub enum PictureParameter {
	/// Wrapper over VAPictureParameterBufferH264.
	H264(h264::PictureParameterBufferH264),
	/// Wrapper over VAPictureParameterBufferHEVC
	HEVC(hevc::PictureParameterBufferHEVC),
	/// Wrapper over VAPictureParameterBufferHEVCRext
	HEVCRext(hevc::PictureParameterBufferHEVCRext),
	/// Wrapper over VAPictureParameterBufferHEVCScc
	HEVCScc(hevc::PictureParameterBufferHEVCScc),
}

/// Abstraction over the `SliceParameterBuffer` types we support
pub enum SliceParameter {
	/// Wrapper over VASliceParameterBufferH264
	H264(h264::SliceParameterBufferH264),
	/// Wrapper over VASliceParameterBufferHEVC
	HEVC(hevc::SliceParameterBufferHEVC),
	/// Wrapper over VASliceParameterBufferHEVCRext
	HEVCRext(hevc::SliceParameterBufferHEVCRext),
}

/// Abstraction over the `IQMatrixBuffer` types we support.
pub enum IQMatrix {
	/// Abstraction over `VAIQMatrixBufferH264`
	H264(h264::IQMatrixBufferH264),
	/// Abstraction over `VAIQMatrixBufferHEVC`
	HEVC(hevc::IQMatrixBufferHEVC),
}

/// Abstraction over the `EncSequenceParameter` types we support.
pub enum EncSequenceParameter {
	/// Abstraction over `VAEncSequenceParameterBufferH264`
	H264(h264::EncSequenceParameterBufferH264),
	/// Abstraction over `VAEncSequenceParameterBufferHEVC`
	HEVC(hevc::EncSequenceParameterBufferHEVC),
}

/// Abstraction over the `EncPictureParameter` types we support.
pub enum EncPictureParameter {
	/// Abstraction over `VAEncPictureParameterBufferH264`
	H264(h264::EncPictureParameterBufferH264),
	/// Abstraction over `VAEncPictureParameterBufferHEVC`
	HEVC(hevc::EncPictureParameterBufferHEVC),
}

/// Abstraction over the `EncSliceParameter` types we support.
pub enum EncSliceParameter {
	/// Abstraction over `VAEncSliceParameterBufferH264`
	H264(h264::EncSliceParameterBufferH264),
	/// Abstraction over `VAEncSliceParameterBufferHEVC`
	HEVC(hevc::EncSliceParameterBufferHEVC),
}

/// Abstraction over the `EncMacroblockParameterBuffer` types we support.
pub enum EncMacroblockParameterBuffer {
	/// Abstraction over `VAEncMacroblockParameterBufferH264`
	H264(h264::EncMacroblockParameterBufferH264),
}

/// Wrapper type representing a buffer created with `vaCreateBuffer` with VAEncCodedBufferType.
pub struct EncCodedBuffer(Buffer);

impl EncCodedBuffer {
	pub(crate) fn new(context: Rc<Context>, size: usize) -> Result<Self, VaError> {
		Ok(Self(Buffer::new(context, BufferType::EncCodedBuffer(size))?))
	}

	/// Convenience function to return buffer's `VABufferID`.
	pub fn id(&self) -> bindings::VABufferID {
		self.0.id
	}
}

/// Helper to access a single segment of mapped coded buffer
pub struct MappedCodedSegment<'s> {
	pub bit_offset: u32,
	pub status: u32,
	pub buf: &'s [u8],
}

/// Helper to access segments of mapped coded buffer
pub struct MappedCodedBuffer<'p> {
	segments: Vec<MappedCodedSegment<'p>>,
	buffer: &'p EncCodedBuffer,
}

impl<'p> MappedCodedBuffer<'p> {
	/// Map a 'VAEncCodedBufferType' buffer.
	pub fn new(buffer: &'p EncCodedBuffer) -> Result<Self, VaError> {
		let mut addr = std::ptr::null_mut();
		let mut segments = Vec::new();

		va_check(unsafe { bindings::va().vaMapBuffer(buffer.0.context.display().handle(), buffer.id(), &mut addr) })?;

		while !addr.is_null() {
			let segment: &bindings::VACodedBufferSegment = unsafe { &*(addr as *const bindings::VACodedBufferSegment) };

			let size = segment.size;
			let buf = segment.buf;

			let buf = unsafe { std::slice::from_raw_parts(buf as *mut u8, size as usize) };

			segments.push(MappedCodedSegment {
				bit_offset: segment.bit_offset,
				status: segment.status,
				buf,
			});

			addr = segment.next;
		}

		Ok(Self { segments, buffer })
	}

	/// Returns the iterator over segments
	pub fn iter(&self) -> impl Iterator<Item = &MappedCodedSegment<'p>> {
		self.segments.iter()
	}

	/// Returns the segments of mapped coded buffers.
	pub fn segments(&self) -> &Vec<MappedCodedSegment<'p>> {
		&self.segments
	}
}

impl<'p> Drop for MappedCodedBuffer<'p> {
	fn drop(&mut self) {
		let status = va_check(unsafe {
			bindings::va().vaUnmapBuffer(self.buffer.0.context.display().handle(), self.buffer.id())
		});

		if status.is_err() {
			error!("vaUnmapBuffer failed: {}", status.unwrap_err());
		}
	}
}

/// Abstraction over the `EncMiscParameterBuffer` types we support.
pub enum EncMiscParameter {
	/// Wrapper over `VAEncMiscParameterBuffer` with `VAEncMiscParameterFrameRate`.
	FrameRate(EncMiscParameterFrameRate),
	/// Wrapper over `VAEncMiscParameterBuffer` with `VAEncMiscParameterRateControl`.
	RateControl(EncMiscParameterRateControl),
	/// Wrapper over `VAEncMiscParameterBuffer` with `VAEncMiscParameterMaxSliceSize`.
	MaxSliceSize(EncMiscParameterMaxSliceSize),
	/// Wrapper over `VAEncMiscParameterBuffer` with `VAEncMiscParameterBufferMaxFrameSize`.
	MaxFrameSize(EncMiscParameterBufferMaxFrameSize),
	/// Wrapper over `VAEncMiscParameterBuffer` with `VAEncMiscParameterSkipFrame`.
	SkipFrame(EncMiscParameterSkipFrame),
	/// Wrapper over `VAEncMiscParameterBuffer` with `VAEncMiscParameterHRD`.
	HRD(EncMiscParameterHRD),
	/// Wrapper over `VAEncMiscParameterBuffer` with `VAEncMiscParameterBufferQualityLevel`.
	QualityLevel(EncMiscParameterBufferQualityLevel),
	/// Wrapper over `VAEncMiscParameterBuffer` with `VAEncMiscParameterQuantization`.
	Quantization(EncMiscParameterQuantization),
}

/// Abstraction over the `VAEncPackedHeaderType` enum values we support.
#[repr(u32)]
pub enum EncPackedHeaderType {
	/// Sequence header
	Sequence = bindings::VAEncPackedHeaderType::VAEncPackedHeaderSequence,
	/// Picture header
	Picture = bindings::VAEncPackedHeaderType::VAEncPackedHeaderPicture,
	/// Slice header
	Slice = bindings::VAEncPackedHeaderType::VAEncPackedHeaderSlice,
	/// Raw data
	RawData = bindings::VAEncPackedHeaderType::VAEncPackedHeaderRawData,
}

/// Abstraction over `EncPackedHeaderParameterBuffer` types we support
pub struct EncPackedHeaderParameter(Box<bindings::VAEncPackedHeaderParameterBuffer>);

impl EncPackedHeaderParameter {
	/// Creates a new `EncPackedHeaderParameter` from the given `VAEncPackedHeaderParameterBuffer`.
	pub fn new(type_: EncPackedHeaderType, length_in_bits: u32, has_emulation: bool) -> Self {
		Self(Box::new(bindings::VAEncPackedHeaderParameterBuffer {
			type_: type_ as _,
			bit_length: length_in_bits,
			has_emulation_bytes: has_emulation as u8,
			..Default::default()
		}))
	}

	/// Returns a mutable reference to the inner `VAEncPackedHeaderParameterBuffer`.
	pub fn inner_mut(&mut self) -> &mut bindings::VAEncPackedHeaderParameterBuffer {
		&mut self.0
	}
}
