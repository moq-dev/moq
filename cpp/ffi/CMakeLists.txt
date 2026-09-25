# Compiles the generated moq-ffi bindings and the probe that exercises them.
# `just cpp check` generates the bindings and drives this; it is not a package.
cmake_minimum_required(VERSION 3.16)
project(moq-ffi-probe LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

# The cargo output directory holding the moq-ffi cdylib.
set(MOQ_FFI_LIB_DIR "" CACHE PATH "Directory containing the moq-ffi shared library")
if(NOT MOQ_FFI_LIB_DIR)
    message(FATAL_ERROR "Set MOQ_FFI_LIB_DIR to the cargo output directory")
endif()

set(GENERATED ${CMAKE_CURRENT_SOURCE_DIR}/generated)
find_package(Threads REQUIRED)

add_executable(probe probe.cpp ${GENERATED}/moq.cpp)
target_include_directories(probe PRIVATE ${GENERATED})

if(MSVC)
    # No exceptions and no RTTI, the way Unreal builds.
    target_compile_options(probe PRIVATE /permissive- /EHs-c- /GR-)
    target_compile_definitions(probe PRIVATE _HAS_EXCEPTIONS=0)
    target_link_libraries(probe PRIVATE ${MOQ_FFI_LIB_DIR}/moq_ffi.dll.lib)
    add_custom_command(TARGET probe POST_BUILD
        COMMAND ${CMAKE_COMMAND} -E copy_if_different ${MOQ_FFI_LIB_DIR}/moq_ffi.dll $<TARGET_FILE_DIR:probe>)
else()
    target_compile_options(probe PRIVATE -Wall -Wextra -Werror -pedantic-errors -fno-exceptions -fno-rtti)
    find_library(MOQ_FFI moq_ffi PATHS ${MOQ_FFI_LIB_DIR} NO_DEFAULT_PATH REQUIRED)
    target_link_libraries(probe PRIVATE ${MOQ_FFI} Threads::Threads)
    set_target_properties(probe PROPERTIES BUILD_RPATH ${MOQ_FFI_LIB_DIR})
endif()
