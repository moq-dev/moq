# libmoq

C bindings for Media over QUIC.

## Building

### Build the Rust library and generate C headers

```bash
cargo build --release
```

This will:
- Build the static library (`libmoq.a` on Unix-like systems, `moq.lib` on Windows)
- Generate the C header file at `target/include/moq.h`
- Generate the pkg-config file at `target/moq.pc`

There's also a [CMakeLists.txt](CMakeLists.txt) file that can be used to import/build the library.

## API

The library exposes the following C functions, see [lib.rs](src/lib.rs) for full details:

```c
int32_t moq_log_level(const char *level);

int32_t moq_session_connect(const char *url, void (*callback)(void *user_data, int32_t code), void *user_data);
int32_t moq_session_close(int32_t id);

int32_t moq_broadcast_create(void);
int32_t moq_broadcast_close(int32_t id);
int32_t moq_broadcast_publish(int32_t id, int32_t session, const char *path);

int32_t moq_track_create(int32_t broadcast, const char *format);
int32_t moq_track_close(int32_t id);
int32_t moq_track_init(int32_t id, const uint8_t *extra, uintptr_t extra_size);
int32_t moq_track_write(int32_t id, const uint8_t *data, uintptr_t data_size, uint64_t pts);
```
