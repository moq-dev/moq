# A consumer of the installed package: `just cpp check` installs cpp/moq to a prefix, then
# builds this with find_package(moq-cpp) at C++17 and C++23, the standard given as
# -DCMAKE_CXX_STANDARD or as -DPROBE_CXX_STANDARD, which sets it in the project and also
# requires it on the app's own target, the way a consumer following doc/lib/cpp would.
cmake_minimum_required(VERSION 3.21)
project(moq-probe LANGUAGES CXX)

if(PROBE_CXX_STANDARD)
    set(CMAKE_CXX_STANDARD ${PROBE_CXX_STANDARD})
endif()
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
