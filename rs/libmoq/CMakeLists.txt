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

# Force absolute path to be used in linker command
target_link_options(moq INTERFACE "$<LINK_ONLY:${RUST_LIB}>")

# Link required system frameworks on macOS
if(APPLE)
    target_link_options(moq INTERFACE "LINKER:-framework,CoreFoundation" "LINKER:-framework,Security")
endif()

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
