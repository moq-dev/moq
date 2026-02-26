# libmoq

C bindings for Media over QUIC. UniFFI bindings are also available for Android (Kotlin) and iOS (Swift).

## Building

### C bindings

```bash
cargo build --release
```

This will:

- Build the static library (`libmoq.a` on Unix-like systems, `moq.lib` on Windows)
- Generate the C header file at `target/include/moq.h`
- Generate the pkg-config file at `target/moq.pc`

There's also a [CMakeLists.txt](CMakeLists.txt) file that can be used to import/build the library.

### UniFFI bindings (Android/iOS)

The `uniffi-api` feature enables the UniFFI surface instead of the C API. First build the library:

```bash
cargo build --release --features uniffi-api
```

Then generate the language bindings:

```bash
cargo run --features uniffi-api \
  --bin uniffi-bindgen \
  generate \
  --library ../../target/release/libmoq.dylib \
  --language kotlin \
  --out-dir out/

cargo run --features uniffi-api \
  --bin uniffi-bindgen \
  generate \
  --library ../../target/release/libmoq.a \
  --language swift \
  --out-dir out/
```

#### Build the Android shared library

To build a shared library for Android (e.g. for use with JNI), install [`cargo-ndk`](https://github.com/bbqsrc/cargo-ndk) and the Android NDK, then run:

```bash
cargo install cargo-ndk
rustup target add aarch64-linux-android
```

Then build for the `arm64-v8a` ABI:

```bash
cargo ndk -t arm64-v8a -o ./jniLibs build --release --features uniffi-api
```

This outputs `libmoq.so` under `./jniLibs/arm64-v8a/`, ready to be bundled into an Android project.

To target additional ABIs (e.g. `x86_64`, `armeabi-v7a`), add more `-t` flags:

```bash
cargo ndk -t arm64-v8a -t x86_64 -o ./jniLibs build --release --features uniffi-api
```

#### Build the iOS XCFramework

To build a static XCFramework for iOS, add the required targets:

```bash
rustup target add aarch64-apple-ios          # real device (arm64)
rustup target add aarch64-apple-ios-sim      # simulator on Apple Silicon
```

Then build for each target:

```bash
cargo build --release --features uniffi-api --target aarch64-apple-ios
cargo build --release --features uniffi-api --target aarch64-apple-ios-sim
```

Then bundle both slices into an XCFramework:

```bash
xcodebuild -create-xcframework \
  -library ../../target/aarch64-apple-ios/release/libmoq.a \
  -library ../../target/aarch64-apple-ios-sim/release/libmoq.a \
  -output libmoq.xcframework
```

This outputs `libmoq.xcframework`, ready to be added to an Xcode project. iOS apps link against it statically at build time — no dynamic loading occurs at runtime.

## C API

The library exposes the following C functions, see [api.rs](src/api.rs) for full details:

```c
// Logging
int32_t moq_log_level(const char *level, uintptr_t level_len);

// Session
int32_t moq_session_connect(const char *url, uintptr_t url_len, uint32_t origin_publish, uint32_t origin_consume, void (*on_status)(void *user_data, int32_t code), void *user_data);
int32_t moq_session_close(uint32_t session);

// Origin
int32_t moq_origin_create(void);
int32_t moq_origin_close(uint32_t origin);
int32_t moq_origin_publish(uint32_t origin, const char *path, uintptr_t path_len, uint32_t broadcast);
int32_t moq_origin_consume(uint32_t origin, const char *path, uintptr_t path_len);
int32_t moq_origin_announced(uint32_t origin, void (*on_announce)(void *user_data, int32_t announced), void *user_data);
int32_t moq_origin_announced_info(uint32_t announced, moq_announced *dst);
int32_t moq_origin_announced_close(uint32_t announced);

// Publishing
int32_t moq_publish_create(void);
int32_t moq_publish_close(uint32_t broadcast);
int32_t moq_publish_media_ordered(uint32_t broadcast, const char *format, uintptr_t format_len, const uint8_t *init, uintptr_t init_size);
int32_t moq_publish_media_close(uint32_t media);
int32_t moq_publish_media_frame(uint32_t media, const uint8_t *payload, uintptr_t payload_size, uint64_t timestamp_us);

// Consuming
int32_t moq_consume_close(uint32_t consume);

// Consuming: Catalog
int32_t moq_consume_catalog(uint32_t broadcast, void (*on_catalog)(void *user_data, int32_t catalog), void *user_data);
int32_t moq_consume_catalog_close(uint32_t catalog);
int32_t moq_consume_video_config(uint32_t catalog, uint32_t index, moq_video_config *dst);
int32_t moq_consume_audio_config(uint32_t catalog, uint32_t index, moq_audio_config *dst);

// Consuming: Video
int32_t moq_consume_video_ordered(uint32_t broadcast, uint32_t index, uint64_t max_latency_ms, void (*on_frame)(void *user_data, int32_t frame), void *user_data);
int32_t moq_consume_video_close(uint32_t track);

// Consuming: Audio
int32_t moq_consume_audio_ordered(uint32_t broadcast, uint32_t index, uint64_t max_latency_ms, void (*on_frame)(void *user_data, int32_t frame), void *user_data);
int32_t moq_consume_audio_close(uint32_t track);

// Consuming: Frames
int32_t moq_consume_frame_chunk(uint32_t frame, uint32_t index, moq_frame *dst);
int32_t moq_consume_frame_close(uint32_t frame);
```
