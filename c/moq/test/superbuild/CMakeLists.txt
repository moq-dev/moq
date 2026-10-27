# A project that adds both c/moq and cpp/moq from source. `just c check` generates it with Ninja
# and dry-runs the build, which fails if both packages claim the same cargo outputs.
cmake_minimum_required(VERSION 3.21)
project(moq-superbuild LANGUAGES C CXX)

add_subdirectory(../.. c)
add_subdirectory(../../../../cpp/moq cpp)
