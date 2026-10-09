fn main() {
	// The CoreVideo/VideoToolbox codec, frame, and render paths, which iOS shares
	// with macOS. Capture stays `target_os = "macos"` (ScreenCaptureKit).
	cfg_aliases::cfg_aliases! {
		apple: { any(target_os = "macos", target_os = "ios") },
	}
}
