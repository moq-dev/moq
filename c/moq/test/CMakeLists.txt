# A consumer of the installed package: `just c check` installs c/moq to a prefix, then builds
# this with find_package(moq-c). C only, so a C++ toolchain is not what makes it link.
cmake_minimum_required(VERSION 3.21)
project(moq-probe LANGUAGES C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

if(MSVC)
    add_compile_options(/W4 /WX)
else()
    add_compile_options(-Wall -Wextra -Werror -pedantic-errors)
endif()

find_package(moq-c REQUIRED)

add_executable(probe probe.c)
target_link_libraries(probe PRIVATE moq::c)
