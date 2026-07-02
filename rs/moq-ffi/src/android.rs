//! Android JNI bootstrap.
//!
//! Auto-initializes moq-native's platform TLS verifier when the JVM loads this
//! library, so Android apps verify against the OS trust store without any
//! Kotlin/Java setup. Best-effort: if the application `Context` can't be found
//! (e.g. loaded too early, or in a non-app process), moq-native falls back to
//! the bundled Mozilla roots.

use std::ffi::c_void;

use moq_native::jni::sys::{JNI_VERSION_1_6, jint};
use moq_native::jni::{JNIEnv, JavaVM};

/// Called by the JVM on `System.loadLibrary("moq_ffi")`. The name is fixed by
/// the JNI spec, so it can't follow Rust's snake_case convention.
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "system" fn JNI_OnLoad(vm: JavaVM, _reserved: *mut c_void) -> jint {
	if let Err(err) = init_platform_tls(&vm) {
		tracing::warn!(%err, "could not auto-initialize the Android platform TLS verifier; using bundled roots");
	}
	JNI_VERSION_1_6
}

/// Attach the loader thread, run the init, and clear any pending Java exception
/// a failed JNI call may have left behind.
fn init_platform_tls(vm: &JavaVM) -> Result<(), Box<dyn std::error::Error>> {
	let mut env = vm.attach_current_thread()?;
	let result = discover_context_and_init(&mut env);
	if result.is_err() {
		// Clear here (before JNI_OnLoad logs and returns) so a pending exception
		// can't surface as a System.loadLibrary failure or abort the next JNI
		// call; the bundled-roots fallback already covers the init failure.
		let _ = env.exception_clear();
	}
	result
}

/// Reflectively fetch the application `Context` and hand it to moq-native.
fn discover_context_and_init(env: &mut JNIEnv) -> Result<(), Box<dyn std::error::Error>> {
	// The app Context isn't passed to native code, so fetch it from
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

	moq_native::tls::init_android(env, app)?;
	Ok(())
}
