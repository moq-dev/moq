// Inputs the samples in doc/lib/cpp/index.md leave undefined. `just cpp check` compiles
// every sample against the installed package with this included first.
#pragma once

#include <moq/moq.hpp>

#include <cstdint>
#include <memory>
#include <vector>

inline void report(const moq::Error &) {}

inline moq::expected<std::shared_ptr<moq::Session>> session;
inline moq::expected<std::shared_ptr<moq::MediaConsumer>> media;
inline std::vector<uint8_t> opus_init;
inline std::vector<uint8_t> packet;
inline std::vector<uint8_t> rgba;
