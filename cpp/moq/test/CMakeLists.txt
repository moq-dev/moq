# A consumer of the installed package: `just cpp check` installs cpp/moq to a prefix, then
# builds this with find_package(moq-cpp) at the -DCMAKE_CXX_STANDARD it is given.
# -DPROBE_CXX_STANDARD raises only the probe's own target, leaving the bindings, which
# compile in this project, at CMAKE_CXX_STANDARD.
cmake_minimum_required(VERSION 3.21)
project(moq-probe LANGUAGES CXX)

if(NOT CMAKE_CXX_STANDARD)
    set(CMAKE_CXX_STANDARD 17)
endif()
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

# No exceptions and no RTTI, the way Unreal builds. Set before find_package so the bindings,
# which compile in this project, get the same flags.
if(MSVC)
    add_compile_options(/permissive- /EHs-c- /GR-)
    add_compile_definitions(_HAS_EXCEPTIONS=0)
else()
    add_compile_options(-Wall -Wextra -Werror -pedantic-errors -fno-exceptions -fno-rtti)
endif()

find_package(moq-cpp REQUIRED)

add_executable(probe probe.cpp)
target_link_libraries(probe PRIVATE moq::cpp)
if(PROBE_CXX_STANDARD)
    target_compile_features(probe PRIVATE cxx_std_${PROBE_CXX_STANDARD})
endif()
