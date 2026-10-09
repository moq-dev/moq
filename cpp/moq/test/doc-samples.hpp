// Inputs the samples in doc/lib/cpp/index.md leave undefined, and the ok() helper its
// Example section shows. `just cpp check` compiles every sample against the installed
// package with this included first.
#pragma once

#include <moq/moq.hpp>

#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <memory>
#include <vector>

template <typename T>
T ok(moq::expected<T> result) {
    if (!result) {
        std::fprintf(stderr, "moq: %s\n", result.error().to_string().c_str());
        std::abort();
    }
    return std::move(*result);
}

inline void ok(moq::expected<void> result) {
    if (!result) {
        std::fprintf(stderr, "moq: %s\n", result.error().to_string().c_str());
        std::abort();
    }
}

inline std::shared_ptr<moq::Session> session;
inline std::shared_ptr<moq::MediaContainerConsumer> media;
inline std::vector<uint8_t> opus_init;
inline std::vector<uint8_t> packet;
inline std::vector<uint8_t> rgba;
