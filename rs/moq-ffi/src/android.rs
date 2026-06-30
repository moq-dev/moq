use jni::EnvUnowned;
use jni::errors::ThrowRuntimeExAndDefault;
use jni::objects::{JClass, JObject};

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_moq_PlatformTLS_initializeNative<'local>(
	mut env: EnvUnowned<'local>,
	_class: JClass<'local>,
	context: JObject<'local>,
) {
	env.with_env(|env| -> jni::errors::Result<()> {
		rustls_platform_verifier::android::init_with_env(env, context)?;
		Ok(())
	})
	.resolve::<ThrowRuntimeExAndDefault>();
}
