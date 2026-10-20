//! A host that installed its own tracing subscriber can still load the plugin.
//!
//! Its own test binary, since the global subscriber is process-wide.

#[test]
fn registers_under_a_host_subscriber() {
	tracing::subscriber::set_global_default(tracing_subscriber::FmtSubscriber::new())
		.expect("no subscriber is installed yet");

	gst::init().unwrap();
	gstmoq::plugin_register_static().expect("register moq plugin");
	assert!(gst::ElementFactory::find("moqsink").is_some());
}
