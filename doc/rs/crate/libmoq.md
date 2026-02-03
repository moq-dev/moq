---
title: libmoq
description: C bindings for MoQ
---

# libmoq

[![docs.rs](https://docs.rs/libmoq/badge.svg)](https://docs.rs/libmoq)

C bindings for `moq-lite` via FFI, enabling MoQ integration in C/C++ applications and other languages.

## Overview

`libmoq` provides:

- **C API** - Header files for C integration
- **FFI bindings** - Safe Rust-to-C interface
- **Build system integration** - CMake and pkg-config support

## Installation

### From Source

```bash
git clone https://github.com/moq-dev/moq
cd moq/rs/libmoq
cargo build --release
```

The library will be in `target/release/libmoq.a` (static) or `target/release/libmoq.so` (dynamic).

### Using Cargo

```bash
cargo install libmoq
```

## Usage

### C Header

```c
#include <moq.h>

int main() {
    // Initialize connection
    moq_connection_t* conn = moq_connect("https://relay.example.com/demo");
    if (!conn) {
        fprintf(stderr, "Failed to connect\n");
        return 1;
    }

    // Create broadcast
    moq_broadcast_t* broadcast = moq_broadcast_new("my-broadcast");

    // Create track
    moq_track_t* track = moq_track_new(broadcast, "chat");

    // Publish data
    moq_group_t* group = moq_group_append(track);
    moq_frame_write(group, "Hello, MoQ!", 11);
    moq_group_close(group);

    // Publish to relay
    moq_publish(conn, broadcast);

    // Cleanup
    moq_broadcast_free(broadcast);
    moq_connection_free(conn);

    return 0;
}
```

### Linking

With CMake:

```cmake
find_package(moq REQUIRED)
target_link_libraries(myapp moq)
```

With pkg-config:

```bash
gcc -o myapp myapp.c $(pkg-config --cflags --libs moq)
```

## API Reference

Full API documentation: [docs.rs/libmoq](https://docs.rs/libmoq)

## Use Cases

- **C/C++ applications** - Native integration without Rust toolchain
- **Language bindings** - Build bindings for Python, Go, etc.
- **Legacy systems** - Integrate MoQ into existing C codebases
- **Embedded systems** - Where Rust runtime isn't available

## Next Steps

- Use [moq-lite](/rs/crate/moq-lite) for Rust applications
- Deploy a [relay server](/app/relay/)
- Read the [Concepts guide](/concept/)
