//! Android JNI bootstrap.
//!
//! Auto-initializes moq-native's platform TLS verifier when the JVM loads this
//! library, so Android apps verify against the OS trust store without any
//! Kotlin/Java setup. Best-effort: if the application `Context` can't be found
//! (e.g. loaded too early, or in a non-app process), moq-native falls back to
//! the bundled Mozilla roots.

use std::ffi::c_void;

use moq_native::jni::JavaVM;
use moq_native::jni::sys::{JNI_VERSION_1_6, jint};

/// Called by the JVM on `System.loadLibrary("moq_ffi")`. The name is fixed by
/// the JNI spec, so it can't follow Rust's snake_case convention.
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "system" fn JNI_OnLoad(vm: JavaVM, _reserved: *mut c_void) -> jint {
	if let Err(err) = init_platform_tls(&vm) {
		tracing::warn!(%err, "could not auto-initialize the Android platform TLS verifier; using bundled roots");

		// A failed JNI call may have left a Java exception pending. Returning it
		// to System.loadLibrary would surface as a load failure, so clear it; the
		// bundled-roots fallback already covers the init failure.
		if let Ok(mut env) = vm.attach_current_thread() {
			let _ = env.exception_clear();
		}
	}
	JNI_VERSION_1_6
}

/// Discover the application `Context` and hand it to moq-native.
fn init_platform_tls(vm: &JavaVM) -> Result<(), Box<dyn std::error::Error>> {
	let mut env = vm.attach_current_thread()?;

	// The app Context isn't passed to native code, so fetch it reflectively from
	// android.app.ActivityThread.currentApplication() (a long-stable internal API).
	let app = env
		.call_static_method(
			"android/app/ActivityThread",
			"currentApplication",
			"()Landroid/app/Application;",
			&[],
		)?
		.l()?;

	if app.is_null() {
		return Err("ActivityThread.currentApplication() returned null".into());
	}

	moq_native::tls::init_android(&mut env, app)?;
	Ok(())
}
