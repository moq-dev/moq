cmake_minimum_required(VERSION 3.21)

# The package version is human-owned, like go/wrapper/VERSION; release-cpp.yml tags cpp-v<it>.
file(STRINGS "${CMAKE_CURRENT_LIST_DIR}/VERSION" _version LIMIT_COUNT 1)
if(NOT _version MATCHES "^[0-9]+\\.[0-9]+\\.[0-9]+$")
    message(FATAL_ERROR "Unsupported version '${_version}' in cpp/moq/VERSION (want x.y.z)")
endif()

project(moq-cpp VERSION ${_version} LANGUAGES CXX)

# Builds moq-ffi as a staticlib with cargo and renders its bindings with uniffi-bindgen-cpp,
# the same no-Corrosion custom target rs/libmoq/CMakeLists.txt uses. Multi-config generators
# (Visual Studio) have no CMAKE_BUILD_TYPE and get the release profile.
set(_workspace "${CMAKE_CURRENT_SOURCE_DIR}/../..")
if(CMAKE_BUILD_TYPE STREQUAL "Debug")
    set(_profile debug)
    set(_cargo_flags "")
else()
    set(_profile release)
    set(_cargo_flags --release)
endif()
if(DEFINED ENV{CARGO_TARGET_DIR})
    set(_cargo_target "$ENV{CARGO_TARGET_DIR}")
else()
    set(_cargo_target "${_workspace}/target")
endif()
if(WIN32)
    set(MOQ_FFI_LIB "${_cargo_target}/${_profile}/moq_ffi.lib")
else()
    set(MOQ_FFI_LIB "${_cargo_target}/${_profile}/libmoq_ffi.a")
endif()

find_program(UNIFFI_BINDGEN_CPP uniffi-bindgen-cpp REQUIRED)

# Rendered to a scratch directory and copied only when changed, so a no-op cargo build does
# not recompile the bindings.
set(_generated "${CMAKE_CURRENT_BINARY_DIR}/include/moq/ffi")
set(_generated_files moq.hpp moq.cpp moq_scaffolding.hpp uniffi_expected.hpp)
list(TRANSFORM _generated_files PREPEND "${_generated}/" OUTPUT_VARIABLE _generated_paths)
set(_render "${CMAKE_CURRENT_BINARY_DIR}/render")
list(TRANSFORM _generated_files PREPEND "${_render}/" OUTPUT_VARIABLE _render_paths)
add_custom_target(moq_ffi ALL
    COMMAND cargo build --locked --package moq-ffi ${_cargo_flags}
    COMMAND ${CMAKE_COMMAND} -E rm -rf "${_render}"
    COMMAND "${UNIFFI_BINDGEN_CPP}" --library "${MOQ_FFI_LIB}"
        --config "${CMAKE_CURRENT_SOURCE_DIR}/uniffi.toml" --out-dir "${_render}"
    COMMAND ${CMAKE_COMMAND} -E make_directory "${_generated}"
    COMMAND ${CMAKE_COMMAND} -E copy_if_different ${_render_paths} "${_generated}"
    WORKING_DIRECTORY "${_workspace}"
    BYPRODUCTS "${MOQ_FFI_LIB}" ${_generated_paths}
    COMMENT "Building moq-ffi and rendering its C++ bindings"
    VERBATIM
)
# System libraries the staticlib needs from an external linker. moq-ffi and libmoq share one
# dependency tree, so they share rs/libmoq/native-libs/, which says how to regenerate it.
if(APPLE)
    set(_platform apple)
elseif(WIN32)
    set(_platform windows)
else()
    set(_platform linux)
endif()
file(STRINGS "${_workspace}/rs/libmoq/native-libs/${_platform}.txt" _lines)
set(MOQ_NATIVE_LIBS "")
foreach(_line IN LISTS _lines)
    string(STRIP "${_line}" _line)
    if(_line STREQUAL "" OR _line MATCHES "^#")
        continue()
    elseif(_line MATCHES "^framework:(.+)$")
        list(APPEND MOQ_NATIVE_LIBS "-framework ${CMAKE_MATCH_1}")
    else()
        list(APPEND MOQ_NATIVE_LIBS "${_line}")
    endif()
endforeach()

# The bindings compile here with the including project's standard and flags; see src/moq.cpp.
# Named moq-cpp so an in-tree build can sit beside libmoq's `moq` target.
add_library(moq-cpp STATIC src/moq.cpp)
add_library(moq-cpp::moq ALIAS moq-cpp)
add_dependencies(moq-cpp moq_ffi)
target_compile_features(moq-cpp PUBLIC cxx_std_17)
target_include_directories(moq-cpp PUBLIC
    "$<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/include>"
    "$<BUILD_INTERFACE:${CMAKE_CURRENT_BINARY_DIR}/include>"
)
find_package(Threads REQUIRED)
target_link_libraries(moq-cpp PUBLIC "${MOQ_FFI_LIB}" ${MOQ_NATIVE_LIBS} Threads::Threads)

if(PROJECT_IS_TOP_LEVEL)
    include(GNUInstallDirs)
    include(CMakePackageConfigHelpers)

    install(DIRECTORY include/moq DESTINATION ${CMAKE_INSTALL_INCLUDEDIR})
    install(FILES ${_generated_paths} DESTINATION ${CMAKE_INSTALL_INCLUDEDIR}/moq/ffi)
    install(FILES src/moq.cpp DESTINATION ${CMAKE_INSTALL_DATADIR}/moq-cpp)
    install(FILES "${MOQ_FFI_LIB}" DESTINATION ${CMAKE_INSTALL_LIBDIR})

    # The config takes MOQ_NATIVE_LIBS as a list; pkg-config wants linker flags.
    get_filename_component(MOQ_FFI_LIB_NAME "${MOQ_FFI_LIB}" NAME)
    set(MOQ_NATIVE_LIBS_PC "")
    foreach(_lib IN LISTS MOQ_NATIVE_LIBS)
        if(_lib MATCHES "^-framework ")
            string(APPEND MOQ_NATIVE_LIBS_PC " ${_lib}")
        else()
            string(APPEND MOQ_NATIVE_LIBS_PC " -l${_lib}")
        endif()
    endforeach()

    configure_package_config_file(cmake/moq-cpp-config.cmake.in
        "${CMAKE_CURRENT_BINARY_DIR}/moq-cpp-config.cmake"
        INSTALL_DESTINATION ${CMAKE_INSTALL_LIBDIR}/cmake/moq-cpp
        PATH_VARS CMAKE_INSTALL_INCLUDEDIR CMAKE_INSTALL_LIBDIR CMAKE_INSTALL_DATADIR
    )
    write_basic_package_version_file("${CMAKE_CURRENT_BINARY_DIR}/moq-cpp-config-version.cmake"
        COMPATIBILITY SameMinorVersion
    )
    install(FILES
        "${CMAKE_CURRENT_BINARY_DIR}/moq-cpp-config.cmake"
        "${CMAKE_CURRENT_BINARY_DIR}/moq-cpp-config-version.cmake"
        DESTINATION ${CMAKE_INSTALL_LIBDIR}/cmake/moq-cpp
    )

    configure_file(cmake/moq-cpp.pc.in "${CMAKE_CURRENT_BINARY_DIR}/moq-cpp.pc" @ONLY)
    install(FILES "${CMAKE_CURRENT_BINARY_DIR}/moq-cpp.pc" DESTINATION ${CMAKE_INSTALL_LIBDIR}/pkgconfig)
endif()
