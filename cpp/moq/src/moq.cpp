// Compiles the generated bindings. Build it with the same C++ standard as the code that
// includes <moq/moq.hpp>; the MOQ_ABI symbol below turns a mismatch into a link error.
#define MOQ_IMPLEMENTATION
#include <moq/moq.hpp>

#include <moq/ffi/moq.cpp>
