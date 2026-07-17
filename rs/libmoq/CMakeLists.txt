cmake_minimum_required(VERSION 3.21)
project(moq VERSION 0.6.1 LANGUAGES C)

option(BUILD_RUST_LIB "Build the Rust library using cargo" ON)

# Determine library extension based on platform
if(WIN32)
    set(LIB_PREFIX "")
    set(LIB_SUFFIX ".lib")
else()
    set(LIB_PREFIX "lib")
    set(LIB_SUFFIX ".a")
endif()

if(BUILD_RUST_LIB)
    if(CMAKE_BUILD_TYPE STREQUAL "Debug")
        set(CARGO_BUILD_TYPE "debug")
        set(CARGO_BUILD_FLAG "")
    else()
        set(CARGO_BUILD_TYPE "release")
        set(CARGO_BUILD_FLAG "--release")
    endif()

    set(RUST_TARGET_DIR "${CMAKE_CURRENT_SOURCE_DIR}/../../target")
    set(RUST_DIR "${RUST_TARGET_DIR}/${CARGO_BUILD_TYPE}")
    set(RUST_LIB "${RUST_DIR}/${LIB_PREFIX}moq${LIB_SUFFIX}")
    set(RUST_HEADER "${RUST_TARGET_DIR}/include/moq.h")

    add_custom_target(rust_build ALL
        COMMAND cargo build ${CARGO_BUILD_FLAG}
        WORKING_DIRECTORY ${CMAKE_CURRENT_SOURCE_DIR}
        BYPRODUCTS ${RUST_LIB} ${RUST_HEADER}
        COMMENT "Building Rust library with cargo"
        VERBATIM
    )
else()
    set(RUST_DIR "${CMAKE_CURRENT_SOURCE_DIR}" CACHE PATH "Directory containing pre-built library and header")
    set(RUST_TARGET_DIR "${RUST_DIR}")
    set(RUST_LIB "${RUST_DIR}/${LIB_PREFIX}moq${LIB_SUFFIX}")
    set(RUST_HEADER "${RUST_DIR}/include/moq.h")
endif()

# Ensure the directories exist (cargo build creates them)
file(MAKE_DIRECTORY ${RUST_DIR})
file(MAKE_DIRECTORY ${RUST_TARGET_DIR}/include)

# Create interface library target that wraps the Rust static library
add_library(moq INTERFACE)
target_include_directories(moq INTERFACE ${RUST_TARGET_DIR}/include)

# Link the static library - must use target_link_libraries so it appears
# after object files in the link command (linker order matters for static libs)
target_link_libraries(moq INTERFACE "${RUST_LIB}")

# System libraries Rust std/deps pull in. When the staticlib is linked into a
# C/C++ target by an external linker, cargo can't inject these itself. build.rs
# reads the same files for moq.pc, so the two never drift.
#
# Reads native-libs/<platform>.txt into MOQ_NATIVE_LIBS: `framework:Foo` becomes
# a linker framework flag, anything else a plain library name.
function(moq_read_native_libs out_var)
    if(APPLE)
        set(_platform "apple")
    elseif(WIN32)
        set(_platform "windows")
    else()
        set(_platform "linux")
    endif()

    set(_file "${CMAKE_CURRENT_SOURCE_DIR}/native-libs/${_platform}.txt")
    file(STRINGS "${_file}" _lines)
    set_property(DIRECTORY APPEND PROPERTY CMAKE_CONFIGURE_DEPENDS "${_file}")

    set(_libs "")
    foreach(_line IN LISTS _lines)
        string(STRIP "${_line}" _line)
        if(_line STREQUAL "" OR _line MATCHES "^#")
            continue()
        endif()
        if(_line MATCHES "^framework:(.+)$")
            list(APPEND _libs "-framework ${CMAKE_MATCH_1}")
        else()
            list(APPEND _libs "${_line}")
        endif()
    endforeach()

    set(${out_var} "${_libs}" PARENT_SCOPE)
endfunction()

moq_read_native_libs(MOQ_NATIVE_LIBS)
target_link_libraries(moq INTERFACE ${MOQ_NATIVE_LIBS})

# Same list, quoted for substitution into cmake/moq-config.cmake.in below.
# nix/overlay.nix substitutes that template itself and formats this identically.
set(MOQ_NATIVE_LIBS_QUOTED "")
foreach(_lib IN LISTS MOQ_NATIVE_LIBS)
    if(MOQ_NATIVE_LIBS_QUOTED)
        string(APPEND MOQ_NATIVE_LIBS_QUOTED " ")
    endif()
    string(APPEND MOQ_NATIVE_LIBS_QUOTED "\"${_lib}\"")
endforeach()

if(BUILD_RUST_LIB)
    add_dependencies(moq rust_build)
endif()

if(PROJECT_IS_TOP_LEVEL)
    include(GNUInstallDirs)

    # Install header and library files
    install(FILES ${RUST_HEADER}
        DESTINATION ${CMAKE_INSTALL_INCLUDEDIR}
    )

    install(FILES ${RUST_LIB}
        DESTINATION ${CMAKE_INSTALL_LIBDIR}
    )

    # Generate and install CMake package config files
    include(CMakePackageConfigHelpers)

    configure_package_config_file(
        ${CMAKE_CURRENT_SOURCE_DIR}/cmake/moq-config.cmake.in
        ${CMAKE_CURRENT_BINARY_DIR}/moq-config.cmake
        INSTALL_DESTINATION ${CMAKE_INSTALL_LIBDIR}/cmake/moq
        PATH_VARS CMAKE_INSTALL_LIBDIR CMAKE_INSTALL_INCLUDEDIR
    )

    write_basic_package_version_file(
        ${CMAKE_CURRENT_BINARY_DIR}/moq-config-version.cmake
        VERSION ${PROJECT_VERSION}
        COMPATIBILITY SameMajorVersion
    )

    install(FILES
        ${CMAKE_CURRENT_BINARY_DIR}/moq-config.cmake
        ${CMAKE_CURRENT_BINARY_DIR}/moq-config-version.cmake
        DESTINATION ${CMAKE_INSTALL_LIBDIR}/cmake/moq
    )
endif()
