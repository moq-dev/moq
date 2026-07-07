//! Android JNI bootstrap.
//!
//! Auto-initializes moq-native's platform TLS verifier when the JVM loads this
//! library, so Android apps verify against the OS trust store without any
//! Kotlin/Java setup. Best-effort: if the application `Context` can't be found
//! (e.g. loaded too early, or in a non-app process), moq-native falls back to
//! the bundled Mozilla roots.

use std::ffi::c_void;

use moq_native::jni::sys::{self, JNI_VERSION_1_6, jint};
use moq_native::jni::{Env, JavaVM, jni_sig, jni_str};

/// Called by the JVM on `System.loadLibrary("moq_ffi")`. The name is fixed by
/// the JNI spec, so it can't follow Rust's snake_case convention.
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "system" fn JNI_OnLoad(vm: *mut sys::JavaVM, _reserved: *mut c_void) -> jint {
	// SAFETY: per the JNI spec the JVM hands `JNI_OnLoad` a valid `JavaVM` pointer.
	let vm = unsafe { JavaVM::from_raw(vm) };
	if let Err(err) = init_platform_tls(&vm) {
		tracing::warn!(%err, "could not auto-initialize the Android platform TLS verifier; using bundled roots");
	}
	JNI_VERSION_1_6
}

/// Attach the loader thread and run the init. A Java exception thrown inside the
/// callback is caught and cleared by `attach_current_thread`, so it can't surface
/// as a `System.loadLibrary` failure or leak into the next JNI call; the
/// bundled-roots fallback already covers the init failure.
fn init_platform_tls(vm: &JavaVM) -> Result<(), Box<dyn std::error::Error>> {
	vm.attach_current_thread(discover_context_and_init)
}

/// Reflectively fetch the application `Context` and hand it to moq-native.
fn discover_context_and_init(env: &mut Env) -> Result<(), Box<dyn std::error::Error>> {
	// The app Context isn't passed to native code, so fetch it from
	// android.app.ActivityThread.currentApplication() (a long-stable internal API).
	// The `jni = ...` override points the compile-time encoders at moq-native's
	// re-export, since moq-ffi has no direct `jni` dependency of its own.
	let app = env
		.call_static_method(
			jni_str!(jni = moq_native::jni, "android/app/ActivityThread"),
			jni_str!(jni = moq_native::jni, "currentApplication"),
			jni_sig!(jni = moq_native::jni, "()Landroid/app/Application;"),
			&[],
		)?
		.l()?;

	if app.is_null() {
		return Err("ActivityThread.currentApplication() returned null".into());
	}

	moq_native::tls::init_android(env, app)?;
	Ok(())
}
