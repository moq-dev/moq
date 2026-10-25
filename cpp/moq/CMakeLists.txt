cmake_minimum_required(VERSION 3.21)

# The package version is human-owned, like go/wrapper/VERSION; release-cpp.yml tags cpp-v<it>.
file(STRINGS "${CMAKE_CURRENT_LIST_DIR}/VERSION" _version LIMIT_COUNT 1)
if(NOT _version MATCHES "^[0-9]+\\.[0-9]+\\.[0-9]+$")
    message(FATAL_ERROR "Unsupported version '${_version}' in cpp/moq/VERSION (want x.y.z)")
endif()

project(moq-cpp VERSION ${_version} LANGUAGES CXX)

set(_workspace "${CMAKE_CURRENT_SOURCE_DIR}/../..")
set(MOQ_FFI_DIR "" CACHE PATH "Extracted default-feature moq-ffi native package")
if(MOQ_FFI_DIR)
    get_filename_component(MOQ_FFI_DIR "${MOQ_FFI_DIR}" ABSOLUTE)
    file(READ "${MOQ_FFI_DIR}/manifest.json" _manifest)
    foreach(_field version target features profile source)
        string(JSON _input_${_field} GET "${_manifest}" ${_field})
    endforeach()
    if(NOT _input_features STREQUAL "default" OR NOT _input_profile STREQUAL "release")
        message(FATAL_ERROR "Prebuilt moq-ffi requires default features and release profile")
    endif()
    if(NOT _input_version MATCHES "^[0-9]+\\.[0-9]+\\.[0-9]+([-+][0-9A-Za-z.+-]+)?$")
        message(FATAL_ERROR "Malformed prebuilt moq-ffi version: ${_input_version}")
    endif()
    if(CMAKE_SYSTEM_NAME STREQUAL "Linux")
        if(CMAKE_SYSTEM_PROCESSOR MATCHES "^(aarch64|arm64)$")
            set(_ffi_target aarch64-unknown-linux-gnu)
        elseif(CMAKE_SYSTEM_PROCESSOR MATCHES "^(x86_64|AMD64)$")
            set(_ffi_target x86_64-unknown-linux-gnu)
        endif()
    elseif(APPLE AND CMAKE_SYSTEM_PROCESSOR MATCHES "^(aarch64|arm64)$")
        set(_ffi_target aarch64-apple-darwin)
    elseif(MSVC AND CMAKE_SIZEOF_VOID_P EQUAL 8 AND CMAKE_SYSTEM_PROCESSOR MATCHES "^(AMD64|x86_64)$")
        set(_ffi_target x86_64-pc-windows-msvc)
    endif()
    if(NOT DEFINED _ffi_target OR NOT _ffi_target STREQUAL _input_target)
        message(FATAL_ERROR "Prebuilt moq-ffi target ${_input_target} does not match this host")
    endif()
else()
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
endif()

if(MOQ_FFI_DIR)
    set(_cargo_target "${MOQ_FFI_DIR}/lib")
    set(_profile "")
endif()
if(WIN32)
    set(MOQ_FFI_LIB "${_cargo_target}/${_profile}/moq_ffi.lib")
    set(_metadata "${_cargo_target}/${_profile}/moq_ffi.dll")
elseif(APPLE)
    set(MOQ_FFI_LIB "${_cargo_target}/${_profile}/libmoq_ffi.a")
    set(_metadata "${_cargo_target}/${_profile}/libmoq_ffi.dylib")
else()
    set(MOQ_FFI_LIB "${_cargo_target}/${_profile}/libmoq_ffi.a")
    set(_metadata "${_cargo_target}/${_profile}/libmoq_ffi.so")
endif()
if(MOQ_FFI_DIR)
    foreach(_input "${MOQ_FFI_LIB}" "${_metadata}" "${MOQ_FFI_DIR}/native-libs.txt")
        if(NOT EXISTS "${_input}")
            message(FATAL_ERROR "Missing prebuilt FFI input: ${_input}")
        endif()
    endforeach()
    set(_build_ffi "")
else()
    # The OpenH264 software fallback is opt-in in Rust; the C++ package ships it, like the
    # prebuilt input does.
    set(_build_ffi COMMAND cargo build --locked --package moq-ffi --features openh264 ${_cargo_flags})
    set(_ffi_byproducts "${MOQ_FFI_LIB}" "${_metadata}")
endif()

find_program(UNIFFI_BINDGEN_CPP uniffi-bindgen-cpp REQUIRED)

# Read metadata from the linked cdylib, like the other binding generators,
# without depending on UniFFI support for every static archive object format.
# Linking still uses the staticlib, so consumers need no runtime shared library.
# Rendered to a scratch directory and copied only when changed, so a no-op cargo build does
# not recompile the bindings.
set(_generated "${CMAKE_CURRENT_BINARY_DIR}/include/moq/ffi")
# moq::IMPLEMENTATION names this package's version, which lives in VERSION.
set(_version_header "${CMAKE_CURRENT_BINARY_DIR}/include/moq/version.hpp")
configure_file(cmake/version.hpp.in "${_version_header}" @ONLY)
set(_generated_files moq.hpp moq.cpp moq_scaffolding.hpp uniffi_expected.hpp)
list(TRANSFORM _generated_files PREPEND "${_generated}/" OUTPUT_VARIABLE _generated_paths)
set(_render "${CMAKE_CURRENT_BINARY_DIR}/render")
list(TRANSFORM _generated_files PREPEND "${_render}/" OUTPUT_VARIABLE _render_paths)
add_custom_target(moq_ffi ALL
    ${_build_ffi}
    COMMAND ${CMAKE_COMMAND} -E rm -rf "${_render}"
    COMMAND "${UNIFFI_BINDGEN_CPP}" --library "${_metadata}"
        --config "${CMAKE_CURRENT_SOURCE_DIR}/uniffi.toml" --out-dir "${_render}"
    COMMAND ${CMAKE_COMMAND} -E make_directory "${_generated}"
    COMMAND ${CMAKE_COMMAND} -E copy_if_different ${_render_paths} "${_generated}"
    WORKING_DIRECTORY "${_workspace}"
    BYPRODUCTS ${_ffi_byproducts} ${_generated_paths}
    COMMENT "Preparing moq-ffi and rendering its C++ bindings"
    VERBATIM
)
# System libraries the staticlib needs from an external linker. moq-ffi and moq-c share one
# dependency tree, so they share rs/moq-c/native-libs/, which says how to regenerate it.
if(APPLE)
    set(_platform apple)
elseif(WIN32)
    set(_platform windows)
else()
    set(_platform linux)
endif()
if(MOQ_FFI_DIR)
    set(_native_libs "${MOQ_FFI_DIR}/native-libs.txt")
else()
    set(_native_libs "${_workspace}/rs/moq-c/native-libs/${_platform}.txt")
endif()
file(STRINGS "${_native_libs}" _lines)
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

# The bindings compile here at the including project's CMAKE_CXX_STANDARD (else the compiler's
# default, at least C++17) and with its flags. Only a target that links them builds them: the
# package ships the source, not this library.
# Named moq-cpp so an in-tree build can sit beside moq-c's `moq` target.
add_library(moq-cpp STATIC EXCLUDE_FROM_ALL src/moq.cpp)
add_library(moq::cpp ALIAS moq-cpp)
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
    install(FILES "${_version_header}" DESTINATION ${CMAKE_INSTALL_INCLUDEDIR}/moq)
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
