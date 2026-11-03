# Builds the C smoke subscriber against the prebuilt libmoq using the CMake
# package config it ships (lib/cmake/moq/moq-config.cmake). This is the CMake
# consumer path: find_package locates the moq::moq imported target, which
# carries the include dir, the static archive, and the per-platform system
# libraries. A broken config (wrong paths, missing target or system libs) fails
# configure/build here -- breakage the hand-rolled cc build can't surface.
#
# smoke.sh points CMAKE_PREFIX_PATH at the extracted tarball root, so this is
# never built standalone; it's driven by c_build_cmake.
cmake_minimum_required(VERSION 3.16)
project(moq_c_smoke C)

find_package(moq REQUIRED CONFIG)

add_executable(smoke subscribe.c)
target_link_libraries(smoke PRIVATE moq::moq)
