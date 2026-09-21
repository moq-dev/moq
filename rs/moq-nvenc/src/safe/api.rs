//! Fallible loading for the private NVENC function table.

use core::ffi::{c_int, c_void};
use std::sync::OnceLock;

use super::result::EncodeError;
use crate::sys::nvEncodeAPI::{
	GUID, NVENCAPI_MAJOR_VERSION, NVENCAPI_MINOR_VERSION, NVENCSTATUS, NV_ENCODE_API_FUNCTION_LIST,
	NV_ENCODE_API_FUNCTION_LIST_VER, NV_ENC_BUFFER_FORMAT, NV_ENC_CAPS_PARAM, NV_ENC_CREATE_BITSTREAM_BUFFER,
	NV_ENC_CREATE_INPUT_BUFFER, NV_ENC_CREATE_MV_BUFFER, NV_ENC_CUSTREAM_PTR, NV_ENC_EVENT_PARAMS,
	NV_ENC_INITIALIZE_PARAMS, NV_ENC_INPUT_PTR, NV_ENC_LOCK_BITSTREAM, NV_ENC_LOCK_INPUT_BUFFER,
	NV_ENC_LOOKAHEAD_PIC_PARAMS, NV_ENC_MAP_INPUT_RESOURCE, NV_ENC_MEONLY_PARAMS, NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS,
	NV_ENC_OUTPUT_PTR, NV_ENC_PIC_PARAMS, NV_ENC_PRESET_CONFIG, NV_ENC_RECONFIGURE_PARAMS, NV_ENC_REGISTERED_PTR,
	NV_ENC_REGISTER_RESOURCE, NV_ENC_RESTORE_ENCODER_STATE_PARAMS, NV_ENC_SEQUENCE_PARAM_PAYLOAD, NV_ENC_STAT,
	NV_ENC_TUNING_INFO,
};

#[cfg(target_os = "linux")]
const CANDIDATES: &[&str] = &["libnvidia-encode.so.1", "libnvidia-encode.so"];
#[cfg(target_os = "windows")]
const CANDIDATES: &[&str] = &["nvEncodeAPI64.dll", "nvEncodeAPI.dll"];
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
const CANDIDATES: &[&str] = &[];

/// An error while loading the NVENC driver API.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum LoadError {
	/// None of the platform's NVIDIA encode libraries could be loaded.
	#[error("NVIDIA encode library unavailable ({reason})")]
	Library {
		/// The attempted libraries and loader failure.
		reason: String,
	},

	/// A required entry point was absent.
	#[error("NVENC entry point {name} unavailable ({reason})")]
	Symbol {
		/// The required entry point.
		name: &'static str,
		/// The loader or driver-table failure.
		reason: String,
	},

	/// The installed driver implements an older NVENC API.
	#[error(
		"NVIDIA driver supports NVENC {supported_major}.{supported_minor}, but {required_major}.{required_minor} is required"
	)]
	UnsupportedVersion {
		/// Required major version.
		required_major: u32,
		/// Required minor version.
		required_minor: u32,
		/// Driver-supported major version.
		supported_major: u32,
		/// Driver-supported minor version.
		supported_minor: u32,
	},

	/// The NVIDIA loader rejected an initialization call.
	#[error("NVENC loader call failed: {0}")]
	Api(#[from] EncodeError),
}

// Function type aliases to shorten later definitions.
type OpenEncodeSession = unsafe extern "C" fn(*mut c_void, u32, *mut *mut c_void) -> NVENCSTATUS;
type GetEncodeGUIDCount = unsafe extern "C" fn(*mut c_void, *mut u32) -> NVENCSTATUS;
type GetEncodeGUIDs = unsafe extern "C" fn(*mut c_void, *mut GUID, u32, *mut u32) -> NVENCSTATUS;
type GetInputFormatCount = unsafe extern "C" fn(*mut c_void, GUID, *mut u32) -> NVENCSTATUS;
type GetInputFormats = unsafe extern "C" fn(*mut c_void, GUID, *mut NV_ENC_BUFFER_FORMAT, u32, *mut u32) -> NVENCSTATUS;
type GetEncodeCaps = unsafe extern "C" fn(*mut c_void, GUID, *mut NV_ENC_CAPS_PARAM, *mut c_int) -> NVENCSTATUS;
type GetEncodePresetCount = unsafe extern "C" fn(*mut c_void, GUID, *mut u32) -> NVENCSTATUS;
type GetEncodePresetGUIDs = unsafe extern "C" fn(*mut c_void, GUID, *mut GUID, u32, *mut u32) -> NVENCSTATUS;
type GetEncodeProfileGUIDCount = GetEncodePresetCount;
type GetEncodeProfileGUIDs = GetEncodePresetGUIDs;
type GetEncodePresetConfig = unsafe extern "C" fn(*mut c_void, GUID, GUID, *mut NV_ENC_PRESET_CONFIG) -> NVENCSTATUS;
type GetEncodePresetConfigEx =
	unsafe extern "C" fn(*mut c_void, GUID, GUID, NV_ENC_TUNING_INFO, *mut NV_ENC_PRESET_CONFIG) -> NVENCSTATUS;
type InitializeEncoder = unsafe extern "C" fn(*mut c_void, *mut NV_ENC_INITIALIZE_PARAMS) -> NVENCSTATUS;
type CreateInputBuffer = unsafe extern "C" fn(*mut c_void, *mut NV_ENC_CREATE_INPUT_BUFFER) -> NVENCSTATUS;
type DestroyInputBuffer = unsafe extern "C" fn(*mut c_void, NV_ENC_INPUT_PTR) -> NVENCSTATUS;
type CreateBitstreamBuffer = unsafe extern "C" fn(*mut c_void, *mut NV_ENC_CREATE_BITSTREAM_BUFFER) -> NVENCSTATUS;
type DestroyBitstreamBuffer = unsafe extern "C" fn(*mut c_void, NV_ENC_OUTPUT_PTR) -> NVENCSTATUS;
type EncodePicture = unsafe extern "C" fn(*mut c_void, *mut NV_ENC_PIC_PARAMS) -> NVENCSTATUS;
type LockBitstream = unsafe extern "C" fn(*mut c_void, *mut NV_ENC_LOCK_BITSTREAM) -> NVENCSTATUS;
type UnlockBitstream = unsafe extern "C" fn(*mut c_void, NV_ENC_OUTPUT_PTR) -> NVENCSTATUS;
type LockInputBuffer = unsafe extern "C" fn(*mut c_void, *mut NV_ENC_LOCK_INPUT_BUFFER) -> NVENCSTATUS;
type UnlockInputBuffer = unsafe extern "C" fn(*mut c_void, NV_ENC_INPUT_PTR) -> NVENCSTATUS;
type GetEncodeStats = unsafe extern "C" fn(*mut c_void, *mut NV_ENC_STAT) -> NVENCSTATUS;
type GetSequenceParams = unsafe extern "C" fn(*mut c_void, *mut NV_ENC_SEQUENCE_PARAM_PAYLOAD) -> NVENCSTATUS;
type RegisterAsyncEvent = unsafe extern "C" fn(*mut c_void, *mut NV_ENC_EVENT_PARAMS) -> NVENCSTATUS;
type UnregisterAsyncEvent = unsafe extern "C" fn(*mut c_void, *mut NV_ENC_EVENT_PARAMS) -> NVENCSTATUS;
type MapInputResource = unsafe extern "C" fn(*mut c_void, *mut NV_ENC_MAP_INPUT_RESOURCE) -> NVENCSTATUS;
type UnmapInputResource = unsafe extern "C" fn(*mut c_void, NV_ENC_INPUT_PTR) -> NVENCSTATUS;
type DestroyEncoder = unsafe extern "C" fn(encoder: *mut c_void) -> NVENCSTATUS;
type InvalidateRefFrames = unsafe extern "C" fn(*mut c_void, u64) -> NVENCSTATUS;
type OpenEncodeSessionEx =
	unsafe extern "C" fn(*mut NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS, *mut *mut c_void) -> NVENCSTATUS;
type RegisterResource = unsafe extern "C" fn(*mut c_void, *mut NV_ENC_REGISTER_RESOURCE) -> NVENCSTATUS;
type UnregisterResource = unsafe extern "C" fn(*mut c_void, NV_ENC_REGISTERED_PTR) -> NVENCSTATUS;
type ReconfigureEncoder = unsafe extern "C" fn(*mut c_void, *mut NV_ENC_RECONFIGURE_PARAMS) -> NVENCSTATUS;
type CreateMVBuffer = unsafe extern "C" fn(*mut c_void, *mut NV_ENC_CREATE_MV_BUFFER) -> NVENCSTATUS;
type DestroyMVBuffer = unsafe extern "C" fn(*mut c_void, NV_ENC_OUTPUT_PTR) -> NVENCSTATUS;
type RunMotionEstimationOnly = unsafe extern "C" fn(*mut c_void, *mut NV_ENC_MEONLY_PARAMS) -> NVENCSTATUS;
type GetLastErrorString = unsafe extern "C" fn(encoder: *mut c_void) -> *const ::core::ffi::c_char;
type SetIOCudaStreams = unsafe extern "C" fn(*mut c_void, NV_ENC_CUSTREAM_PTR, NV_ENC_CUSTREAM_PTR) -> NVENCSTATUS;
type GetSequenceParamEx =
	unsafe extern "C" fn(*mut c_void, *mut NV_ENC_INITIALIZE_PARAMS, *mut NV_ENC_SEQUENCE_PARAM_PAYLOAD) -> NVENCSTATUS;
type RestoreEncoderState = unsafe extern "C" fn(*mut c_void, *mut NV_ENC_RESTORE_ENCODER_STATE_PARAMS) -> NVENCSTATUS;
type LookaheadPicture = unsafe extern "C" fn(*mut c_void, *mut NV_ENC_LOOKAHEAD_PIC_PARAMS) -> NVENCSTATUS;
type GetMaxVersion = unsafe extern "C" fn(*mut u32) -> NVENCSTATUS;
type CreateInstance = unsafe extern "C" fn(*mut NV_ENCODE_API_FUNCTION_LIST) -> NVENCSTATUS;

#[derive(Clone, Copy)]
struct EntryPoints {
	get_max_version: GetMaxVersion,
	create_instance: CreateInstance,
}

trait Loader {
	fn entry_points(&self) -> Result<EntryPoints, LoadError>;
}

struct DynamicLoader;

impl Loader for DynamicLoader {
	fn entry_points(&self) -> Result<EntryPoints, LoadError> {
		// SAFETY: loading the NVIDIA driver library runs its initializers, which
		// is sound for driver libraries. The handle is intentionally leaked so
		// every resolved function pointer remains valid for the process lifetime.
		unsafe {
			let mut failures = Vec::new();
			let library = CANDIDATES
				.iter()
				.find_map(|name| match libloading::Library::new(*name) {
					Ok(library) => Some(library),
					Err(error) => {
						failures.push(format!("{name}: {error}"));
						None
					}
				});
			let library = library.ok_or_else(|| LoadError::Library {
				reason: if failures.is_empty() {
					"no library exists for this platform".to_owned()
				} else {
					failures.join(", ")
				},
			})?;
			let library: &'static libloading::Library = Box::leak(Box::new(library));

			unsafe fn symbol<T: Copy>(
				library: &'static libloading::Library,
				name: &'static str,
				bytes: &'static [u8],
			) -> Result<T, LoadError> {
				let symbol: libloading::Symbol<T> =
					unsafe { library.get(bytes) }.map_err(|error| LoadError::Symbol {
						name,
						reason: error.to_string(),
					})?;
				Ok(*symbol)
			}

			Ok(EntryPoints {
				get_max_version: symbol(
					library,
					"NvEncodeAPIGetMaxSupportedVersion",
					b"NvEncodeAPIGetMaxSupportedVersion\0",
				)?,
				create_instance: symbol(library, "NvEncodeAPICreateInstance", b"NvEncodeAPICreateInstance\0")?,
			})
		}
	}
}

/// The private `NvEncodeAPI` function table.
#[allow(dead_code, missing_docs)]
#[derive(Debug, Clone)]
pub(crate) struct EncodeAPI {
	#[doc(alias = "NvEncOpenEncodeSession")]
	pub open_encode_session: OpenEncodeSession,
	#[doc(alias = "NvEncOpenEncodeSessionEx")]
	pub open_encode_session_ex: OpenEncodeSessionEx,
	#[doc(alias = "NvEncInitializeEncoder")]
	pub initialize_encoder: InitializeEncoder,
	#[doc(alias = "NvEncReconfigureEncoder")]
	pub reconfigure_encoder: ReconfigureEncoder,
	#[doc(alias = "NvEncDestroyEncoder")]
	pub destroy_encoder: DestroyEncoder,
	#[doc(alias = "NvEncGetEncodeGuidCount")]
	pub get_encode_guid_count: GetEncodeGUIDCount,
	#[doc(alias = "NvEncGetEncodeGUIDs")]
	pub get_encode_guids: GetEncodeGUIDs,
	#[doc(alias = "NvEncGetEncodeProfileGuidCount")]
	pub get_encode_profile_guid_count: GetEncodeProfileGUIDCount,
	#[doc(alias = "NvEncGetEncodeProfileGUIDs")]
	pub get_encode_profile_guids: GetEncodeProfileGUIDs,
	#[doc(alias = "NvEncGetInputFormatCount")]
	pub get_input_format_count: GetInputFormatCount,
	#[doc(alias = "NvEncGetInputFormats")]
	pub get_input_formats: GetInputFormats,
	#[doc(alias = "NvEncGetEncodePresetCount")]
	pub get_encode_preset_count: GetEncodePresetCount,
	#[doc(alias = "NvEncGetEncodePresetGUIDs")]
	pub get_encode_preset_guids: GetEncodePresetGUIDs,
	#[doc(alias = "NvEncGetEncodePresetConfig")]
	pub get_encode_preset_config: GetEncodePresetConfig,
	#[doc(alias = "NvEncGetEncodePresetConfigEx")]
	pub get_encode_preset_config_ex: GetEncodePresetConfigEx,
	#[doc(alias = "NvEncGetEncodeCaps")]
	pub get_encode_caps: GetEncodeCaps,
	#[doc(alias = "NvEncCreateInputBuffer")]
	pub create_input_buffer: CreateInputBuffer,
	#[doc(alias = "NvEncDestroyInputBuffer")]
	pub destroy_input_buffer: DestroyInputBuffer,
	#[doc(alias = "NvLockInputBuffer")]
	pub lock_input_buffer: LockInputBuffer,
	#[doc(alias = "NvUnlockInputBuffer")]
	pub unlock_input_buffer: UnlockInputBuffer,
	#[doc(alias = "NvEncCreateBitstreamBuffer")]
	pub create_bitstream_buffer: CreateBitstreamBuffer,
	#[doc(alias = "NvEncDestroyBitstreamBuffer")]
	pub destroy_bitstream_buffer: DestroyBitstreamBuffer,
	#[doc(alias = "NvEncLockBitstream")]
	pub lock_bitstream: LockBitstream,
	#[doc(alias = "NvEncUnlockBitstream")]
	pub unlock_bitstream: UnlockBitstream,
	#[doc(alias = "NvEncMapInputResource")]
	pub map_input_resource: MapInputResource,
	#[doc(alias = "NvEncUnmapInputResource")]
	pub unmap_input_resource: UnmapInputResource,
	#[doc(alias = "NvEncRegisterResource")]
	pub register_resource: RegisterResource,
	#[doc(alias = "NvEncUnregisterResource")]
	pub unregister_resource: UnregisterResource,
	#[doc(alias = "NvEncCreateMVBuffer")]
	pub create_mv_buffer: CreateMVBuffer,
	#[doc(alias = "NvEncDestroyMVBuffer")]
	pub destroy_mv_buffer: DestroyMVBuffer,
	#[doc(alias = "NvEncEncodePicture")]
	pub encode_picture: EncodePicture,
	#[doc(alias = "NvEncGetEncodeStats")]
	pub get_encode_stats: GetEncodeStats,
	#[doc(alias = "NvEncGetSequenceParams")]
	pub get_sequence_params: GetSequenceParams,
	#[doc(alias = "NvEncGetSequenceParamEx")]
	pub get_sequence_param_ex: GetSequenceParamEx,
	#[doc(alias = "NvEncRegisterAsyncEvent")]
	pub register_async_event: RegisterAsyncEvent,
	#[doc(alias = "NvEncUnregisterAsyncEvent")]
	pub unregister_async_event: UnregisterAsyncEvent,
	#[doc(alias = "NvEncInvalidateRefFrames")]
	pub invalidate_ref_frames: InvalidateRefFrames,
	#[doc(alias = "NvEncRunMotionEstimationOnly")]
	pub run_motion_estimation_only: RunMotionEstimationOnly,
	#[doc(alias = "NvEncGetLastErrorString")]
	pub get_last_error_string: GetLastErrorString,
	#[doc(alias = "NvEncSetIOCudaStreams")]
	pub set_io_cuda_streams: SetIOCudaStreams,
	#[doc(alias = "NvEncRestoreEncoderState")]
	pub restore_encoder_state: RestoreEncoderState,
	#[doc(alias = "NvEncLookaheadPicture")]
	pub lookahead_picture: LookaheadPicture,
}

pub(crate) fn get() -> Result<&'static EncodeAPI, LoadError> {
	static API: OnceLock<Result<EncodeAPI, LoadError>> = OnceLock::new();
	API.get_or_init(|| EncodeAPI::load_with(&DynamicLoader))
		.as_ref()
		.map_err(Clone::clone)
}

impl EncodeAPI {
	fn load_with(loader: &dyn Loader) -> Result<Self, LoadError> {
		let entry_points = loader.entry_points()?;
		let mut version = 0;
		unsafe { (entry_points.get_max_version)(&mut version) }.result_without_string()?;
		let supported = (version >> 4, version & 0b1111);
		let required = (NVENCAPI_MAJOR_VERSION, NVENCAPI_MINOR_VERSION);
		if supported < required {
			return Err(LoadError::UnsupportedVersion {
				required_major: required.0,
				required_minor: required.1,
				supported_major: supported.0,
				supported_minor: supported.1,
			});
		}

		let mut function_list = NV_ENCODE_API_FUNCTION_LIST {
			version: NV_ENCODE_API_FUNCTION_LIST_VER,
			..Default::default()
		};
		unsafe { (entry_points.create_instance)(&mut function_list) }.result_without_string()?;

		Self::from_function_list(function_list)
	}

	fn from_function_list(function_list: NV_ENCODE_API_FUNCTION_LIST) -> Result<Self, LoadError> {
		macro_rules! required {
			($field:ident) => {
				function_list.$field.ok_or_else(|| LoadError::Symbol {
					name: stringify!($field),
					reason: "driver returned an incomplete function table".to_owned(),
				})?
			};
		}

		Ok(Self {
			open_encode_session: required!(nvEncOpenEncodeSession),
			open_encode_session_ex: required!(nvEncOpenEncodeSessionEx),
			initialize_encoder: required!(nvEncInitializeEncoder),
			reconfigure_encoder: required!(nvEncReconfigureEncoder),
			destroy_encoder: required!(nvEncDestroyEncoder),
			get_encode_guid_count: required!(nvEncGetEncodeGUIDCount),
			get_encode_guids: required!(nvEncGetEncodeGUIDs),
			get_encode_profile_guid_count: required!(nvEncGetEncodeProfileGUIDCount),
			get_encode_profile_guids: required!(nvEncGetEncodeProfileGUIDs),
			get_input_format_count: required!(nvEncGetInputFormatCount),
			get_input_formats: required!(nvEncGetInputFormats),
			get_encode_preset_count: required!(nvEncGetEncodePresetCount),
			get_encode_preset_guids: required!(nvEncGetEncodePresetGUIDs),
			get_encode_preset_config: required!(nvEncGetEncodePresetConfig),
			get_encode_preset_config_ex: required!(nvEncGetEncodePresetConfigEx),
			get_encode_caps: required!(nvEncGetEncodeCaps),
			create_input_buffer: required!(nvEncCreateInputBuffer),
			destroy_input_buffer: required!(nvEncDestroyInputBuffer),
			lock_input_buffer: required!(nvEncLockInputBuffer),
			unlock_input_buffer: required!(nvEncUnlockInputBuffer),
			create_bitstream_buffer: required!(nvEncCreateBitstreamBuffer),
			destroy_bitstream_buffer: required!(nvEncDestroyBitstreamBuffer),
			lock_bitstream: required!(nvEncLockBitstream),
			unlock_bitstream: required!(nvEncUnlockBitstream),
			map_input_resource: required!(nvEncMapInputResource),
			unmap_input_resource: required!(nvEncUnmapInputResource),
			register_resource: required!(nvEncRegisterResource),
			unregister_resource: required!(nvEncUnregisterResource),
			create_mv_buffer: required!(nvEncCreateMVBuffer),
			destroy_mv_buffer: required!(nvEncDestroyMVBuffer),
			encode_picture: required!(nvEncEncodePicture),
			get_encode_stats: required!(nvEncGetEncodeStats),
			get_sequence_params: required!(nvEncGetSequenceParams),
			get_sequence_param_ex: required!(nvEncGetSequenceParamEx),
			register_async_event: required!(nvEncRegisterAsyncEvent),
			unregister_async_event: required!(nvEncUnregisterAsyncEvent),
			invalidate_ref_frames: required!(nvEncInvalidateRefFrames),
			run_motion_estimation_only: required!(nvEncRunMotionEstimationOnly),
			get_last_error_string: required!(nvEncGetLastErrorString),
			set_io_cuda_streams: required!(nvEncSetIOCudaStreams),
			restore_encoder_state: required!(nvEncRestoreEncoderState),
			lookahead_picture: required!(nvEncLookaheadPicture),
		})
	}
}

#[cfg(test)]
mod tests {
	use std::mem::{size_of, transmute_copy};

	use super::*;

	#[derive(Clone)]
	struct TestLoader(Result<EntryPoints, LoadError>);

	impl Loader for TestLoader {
		fn entry_points(&self) -> Result<EntryPoints, LoadError> {
			self.0.clone()
		}
	}

	unsafe extern "C" fn unused_function() {}

	unsafe extern "C" fn create_complete(functions: *mut NV_ENCODE_API_FUNCTION_LIST) -> NVENCSTATUS {
		unsafe { *functions = complete_function_list() };
		NVENCSTATUS::NV_ENC_SUCCESS
	}

	unsafe extern "C" fn create_unexpected(_functions: *mut NV_ENCODE_API_FUNCTION_LIST) -> NVENCSTATUS {
		panic!("create_instance must not be called after an unsupported version");
	}

	fn fake_function<T: Copy>() -> T {
		assert_eq!(size_of::<T>(), size_of::<unsafe extern "C" fn()>());
		// SAFETY: every function pointer has the same representation on the
		// supported targets. Tests only verify table construction and never call
		// these deliberately signature-erased pointers.
		unsafe { transmute_copy(&(unused_function as unsafe extern "C" fn())) }
	}

	fn complete_function_list() -> NV_ENCODE_API_FUNCTION_LIST {
		let mut functions = NV_ENCODE_API_FUNCTION_LIST {
			version: NV_ENCODE_API_FUNCTION_LIST_VER,
			..Default::default()
		};
		macro_rules! provide {
			($($field:ident),+ $(,)?) => {
				$(functions.$field = Some(fake_function());)+
			};
		}
		provide!(
			nvEncOpenEncodeSession,
			nvEncOpenEncodeSessionEx,
			nvEncInitializeEncoder,
			nvEncReconfigureEncoder,
			nvEncDestroyEncoder,
			nvEncGetEncodeGUIDCount,
			nvEncGetEncodeGUIDs,
			nvEncGetEncodeProfileGUIDCount,
			nvEncGetEncodeProfileGUIDs,
			nvEncGetInputFormatCount,
			nvEncGetInputFormats,
			nvEncGetEncodePresetCount,
			nvEncGetEncodePresetGUIDs,
			nvEncGetEncodePresetConfig,
			nvEncGetEncodePresetConfigEx,
			nvEncGetEncodeCaps,
			nvEncCreateInputBuffer,
			nvEncDestroyInputBuffer,
			nvEncLockInputBuffer,
			nvEncUnlockInputBuffer,
			nvEncCreateBitstreamBuffer,
			nvEncDestroyBitstreamBuffer,
			nvEncLockBitstream,
			nvEncUnlockBitstream,
			nvEncMapInputResource,
			nvEncUnmapInputResource,
			nvEncRegisterResource,
			nvEncUnregisterResource,
			nvEncCreateMVBuffer,
			nvEncDestroyMVBuffer,
			nvEncEncodePicture,
			nvEncGetEncodeStats,
			nvEncGetSequenceParams,
			nvEncGetSequenceParamEx,
			nvEncRegisterAsyncEvent,
			nvEncUnregisterAsyncEvent,
			nvEncInvalidateRefFrames,
			nvEncRunMotionEstimationOnly,
			nvEncGetLastErrorString,
			nvEncSetIOCudaStreams,
			nvEncRestoreEncoderState,
			nvEncLookaheadPicture,
		);
		functions
	}

	unsafe extern "C" fn current_version(version: *mut u32) -> NVENCSTATUS {
		unsafe { *version = (NVENCAPI_MAJOR_VERSION << 4) | NVENCAPI_MINOR_VERSION };
		NVENCSTATUS::NV_ENC_SUCCESS
	}

	unsafe extern "C" fn old_version(version: *mut u32) -> NVENCSTATUS {
		unsafe { *version = (NVENCAPI_MAJOR_VERSION - 1) << 4 };
		NVENCSTATUS::NV_ENC_SUCCESS
	}

	unsafe extern "C" fn create_incomplete(_functions: *mut NV_ENCODE_API_FUNCTION_LIST) -> NVENCSTATUS {
		NVENCSTATUS::NV_ENC_SUCCESS
	}

	#[test]
	fn missing_library_is_an_error() {
		let error = EncodeAPI::load_with(&TestLoader(Err(LoadError::Library {
			reason: "not installed".to_owned(),
		})))
		.unwrap_err();
		assert!(matches!(error, LoadError::Library { reason } if reason == "not installed"));
	}

	#[test]
	fn missing_bootstrap_symbol_is_an_error() {
		let error = EncodeAPI::load_with(&TestLoader(Err(LoadError::Symbol {
			name: "NvEncodeAPICreateInstance",
			reason: "not exported".to_owned(),
		})))
		.unwrap_err();
		assert!(matches!(
			error,
			LoadError::Symbol { name: "NvEncodeAPICreateInstance", reason } if reason == "not exported"
		));
	}

	#[test]
	fn old_driver_is_rejected_before_table_creation() {
		let error = EncodeAPI::load_with(&TestLoader(Ok(EntryPoints {
			get_max_version: old_version,
			create_instance: create_unexpected,
		})))
		.unwrap_err();
		assert!(matches!(error, LoadError::UnsupportedVersion { .. }));
	}

	#[test]
	fn incomplete_driver_table_is_an_error() {
		let error = EncodeAPI::load_with(&TestLoader(Ok(EntryPoints {
			get_max_version: current_version,
			create_instance: create_incomplete,
		})))
		.unwrap_err();
		assert!(matches!(
			error,
			LoadError::Symbol {
				name: "nvEncOpenEncodeSession",
				..
			}
		));
	}

	#[test]
	fn complete_driver_table_loads() {
		EncodeAPI::load_with(&TestLoader(Ok(EntryPoints {
			get_max_version: current_version,
			create_instance: create_complete,
		})))
		.unwrap();
	}
}
