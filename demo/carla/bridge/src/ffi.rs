//! The C ABI the CARLA plugin calls, declared in `include/carla_bridge.h`.
//!
//! One bridge per process, created by the first source that names a socket.
//! Every entry point reports failure through its return value and
//! [`carla_bridge_error`]; a panic is caught at the boundary and reported the
//! same way rather than unwinding into Unreal.

use std::cell::RefCell;
use std::ffi::{CStr, CString, c_char, c_int};
use std::os::fd::{FromRawFd, OwnedFd};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use parking_lot::Mutex;

use crate::{Bridge, Device, Format, Size, Source};

static BRIDGE: Mutex<Option<Bridge>> = Mutex::new(None);

thread_local! {
    static ERROR: RefCell<CString> = RefCell::new(CString::default());
}

/// Opaque to C.
pub struct CarlaBridgeSource(Source);

fn guard<T>(failed: T, body: impl FnOnce() -> Result<T>) -> T {
    let error = match catch_unwind(AssertUnwindSafe(body)) {
        Ok(Ok(value)) => return value,
        Ok(Err(err)) => format!("{err:#}"),
        Err(panic) => {
            let message = panic
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_default();
            format!("CARLA bridge panicked: {message}")
        }
    };
    tracing::error!(%error, "CARLA bridge call failed");
    let error = CString::new(error.replace('\0', " ")).expect("NULs replaced");
    ERROR.with(|slot| *slot.borrow_mut() = error);
    failed
}

/// # Safety
/// `value` is NULL or a NUL-terminated string.
unsafe fn text<'a>(value: *const c_char, what: &str) -> Result<&'a str> {
    if value.is_null() {
        bail!("{what} is NULL");
    }
    // SAFETY: the caller passes a NUL-terminated string.
    unsafe { CStr::from_ptr(value) }
        .to_str()
        .with_context(|| format!("{what} is not UTF-8"))
}

/// # Safety
/// `source` is NULL or a live pointer from `carla_bridge_source_open`.
unsafe fn source<'a>(source: *const CarlaBridgeSource) -> Result<&'a Source> {
    // SAFETY: the caller passes a live source.
    unsafe { source.as_ref() }
        .map(|source| &source.0)
        .ok_or_else(|| anyhow!("source is NULL"))
}

/// Register a camera; see `include/carla_bridge.h`.
///
/// # Safety
/// `socket` and `stream` are NUL-terminated strings, and `device_uuid` and
/// `driver_uuid` each point at 16 bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn carla_bridge_source_open(
    socket: *const c_char,
    stream: *const c_char,
    device_uuid: *const u8,
    driver_uuid: *const u8,
    width: u32,
    height: u32,
    channels: u32,
    slots: u32,
) -> *mut CarlaBridgeSource {
    guard(std::ptr::null_mut(), || {
        // SAFETY: forwarded from the caller.
        let socket = unsafe { text(socket, "socket") }?;
        // SAFETY: forwarded from the caller.
        let stream = unsafe { text(stream, "stream") }?;
        if device_uuid.is_null() || driver_uuid.is_null() {
            bail!("device or driver UUID is NULL");
        }
        // SAFETY: the caller passes 16 readable bytes for each.
        let device = Device {
            device_uuid: unsafe { *device_uuid.cast::<[u8; 16]>() },
            driver_uuid: unsafe { *driver_uuid.cast::<[u8; 16]>() },
            render_node: None,
        };
        let format = match channels {
            0 => Format::Rgba8,
            1 => Format::Bgra8,
            other => bail!("unknown channel order {other}"),
        };

        let mut bridge = BRIDGE.lock();
        let bridge = match &mut *bridge {
            Some(bridge) if bridge.path() == Path::new(socket) => bridge,
            Some(bridge) => bail!("the bridge already listens on {}", bridge.path().display()),
            None => bridge.insert(Bridge::listen(socket)?),
        };
        let source = bridge.source(
            stream,
            device,
            Size::new(width, height),
            format,
            slots as usize,
        )?;
        Ok(Box::into_raw(Box::new(CarlaBridgeSource(source))))
    })
}

/// # Safety
/// `source` is a live source; the descriptors are owned by the caller and
/// consumed by this call whatever it returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn carla_bridge_source_import(
    source_: *mut CarlaBridgeSource,
    slot: u32,
    memory_fd: c_int,
    timeline_fd: c_int,
    allocation_size: u64,
    memory_type: u32,
) -> c_int {
    // Owned first, so an early error still closes them.
    // SAFETY: the caller transfers ownership of both descriptors.
    let memory = (memory_fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(memory_fd) });
    // SAFETY: as above.
    let timeline = (timeline_fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(timeline_fd) });
    guard(-1, || {
        let (Some(memory), Some(timeline)) = (memory, timeline) else {
            bail!("invalid file descriptor");
        };
        // SAFETY: forwarded from the caller.
        unsafe { source(source_) }?.import(
            slot as usize,
            memory,
            timeline,
            allocation_size,
            memory_type,
        )?;
        Ok(0)
    })
}

/// # Safety
/// `source` is a live source.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn carla_bridge_source_wanted(source_: *const CarlaBridgeSource) -> bool {
    // SAFETY: forwarded from the caller.
    guard(false, || Ok(unsafe { source(source_) }?.wanted()))
}

/// # Safety
/// `source` is a live source and `slot`/`ready` are writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn carla_bridge_source_acquire(
    source_: *mut CarlaBridgeSource,
    slot: *mut u32,
    ready: *mut u64,
) -> c_int {
    guard(-1, || {
        if slot.is_null() || ready.is_null() {
            bail!("output pointer is NULL");
        }
        // SAFETY: forwarded from the caller.
        let Some((index, value)) = unsafe { source(source_) }?.acquire()? else {
            return Ok(0);
        };
        // SAFETY: the caller passes writable outputs.
        unsafe {
            *slot = index as u32;
            *ready = value;
        }
        Ok(1)
    })
}

/// # Safety
/// `source` is a live source.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn carla_bridge_source_submit(
    source_: *mut CarlaBridgeSource,
    slot: u32,
    frame: u64,
    timestamp_us: u64,
) -> c_int {
    guard(-1, || {
        // SAFETY: forwarded from the caller.
        unsafe { source(source_) }?.submit(slot as usize, frame, timestamp_us)?;
        Ok(0)
    })
}

/// # Safety
/// `source` is a live source.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn carla_bridge_source_cancel(
    source_: *mut CarlaBridgeSource,
    slot: u32,
) -> c_int {
    guard(-1, || {
        // SAFETY: forwarded from the caller.
        unsafe { source(source_) }?.cancel(slot as usize)?;
        Ok(0)
    })
}

/// Report that the producer can no longer feed a camera; see
/// `include/carla_bridge.h`.
///
/// # Safety
/// `source` is a live source and `message` a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn carla_bridge_source_fail(
    source_: *mut CarlaBridgeSource,
    message: *const c_char,
) -> c_int {
    guard(-1, || {
        // SAFETY: forwarded from the caller.
        let message = unsafe { text(message, "message") }?;
        // SAFETY: forwarded from the caller.
        unsafe { source(source_) }?.fail(message);
        Ok(0)
    })
}

/// # Safety
/// `source` is NULL or a live source, which this call frees.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn carla_bridge_source_close(source_: *mut CarlaBridgeSource) {
    guard((), || {
        if !source_.is_null() {
            // SAFETY: the caller hands back the pointer `open` returned, once.
            drop(unsafe { Box::from_raw(source_) });
        }
        Ok(())
    })
}

/// The calling thread's last error, valid until its next bridge call.
#[unsafe(no_mangle)]
pub extern "C" fn carla_bridge_error() -> *const c_char {
    ERROR.with(|slot| slot.borrow().as_ptr())
}
