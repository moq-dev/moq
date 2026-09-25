# The C++ interop client, built against the installed cpp/moq package the way an external
# project would: interop.sh installs cpp/moq to a prefix and points CMAKE_PREFIX_PATH at it.
cmake_minimum_required(VERSION 3.21)
project(cpp-interop LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

find_package(moq REQUIRED)

add_executable(cpp-interop main.cpp)
target_link_libraries(cpp-interop PRIVATE moq::moq)
