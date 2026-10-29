# The moq-ffi native input shared by cpp/moq and c/moq: an extracted prebuilt package
# (MOQ_FFI_DIR), or a cargo build of this checkout. Sets MOQ_FFI_LIB (the staticlib to link),
# MOQ_FFI_METADATA (the cdylib uniffi-bindgen-cpp reads), MOQ_FFI_CARGO (the target that builds
# them, or empty for a prebuilt input), and MOQ_NATIVE_LIBS (what an external linker adds).
#
# Run with -P, it is that cargo build: it links the libraries cargo reports into OUT, so no
# cargo setting (target-dir, build.target) can move them from where the packages look.

if(CMAKE_SCRIPT_MODE_FILE)
    # The shared default build (.cargo/config.toml), which the prebuilt input matches.
    set(_args build-ffi --message-format=json-render-diagnostics)
    if(RELEASE)
        list(APPEND _args --release)
    endif()
    execute_process(
        COMMAND cargo ${_args}
        WORKING_DIRECTORY "${WORKSPACE}"
        OUTPUT_VARIABLE _messages
        COMMAND_ERROR_IS_FATAL ANY
    )
    string(REGEX MATCH "{\"reason\":\"compiler-artifact\"[^\n]*\"name\":\"moq_ffi\",[^\n]*"
        _artifact "${_messages}")
    if(NOT _artifact)
        message(FATAL_ERROR "cargo reported no moq_ffi library")
    endif()
    string(JSON _count LENGTH "${_artifact}" filenames)
    # The wrappers compile for the host, so moq-ffi must too. A build target (CARGO_BUILD_TARGET
    # or a cargo config's build.target) puts the library under <target-dir>/<triple>/<profile>/
    # rather than <target-dir>/<profile>/.
    execute_process(
        COMMAND cargo metadata --locked --no-deps --format-version 1
        WORKING_DIRECTORY "${WORKSPACE}"
        OUTPUT_VARIABLE _metadata
        COMMAND_ERROR_IS_FATAL ANY
    )
    string(JSON _target_dir GET "${_metadata}" target_directory)
    string(JSON _file GET "${_artifact}" filenames 0)
    file(TO_CMAKE_PATH "${_target_dir}" _target_dir)
    file(TO_CMAKE_PATH "${_file}" _file)
    get_filename_component(_dir "${_file}" DIRECTORY)
    file(RELATIVE_PATH _within "${_target_dir}" "${_dir}")
    if(_within MATCHES "^([^/]+)/[^/]+$")
        set(_triple "${CMAKE_MATCH_1}")
        execute_process(COMMAND rustc -vV OUTPUT_VARIABLE _rustc COMMAND_ERROR_IS_FATAL ANY)
        string(REGEX MATCH "host: ([^\r\n]+)" _ "${_rustc}")
        if(NOT _triple STREQUAL CMAKE_MATCH_1)
            message(FATAL_ERROR "cargo built moq-ffi for ${_triple}, not the host "
                "(${CMAKE_MATCH_1}); a source build targets the host, so unset the cargo build "
                "target or use MOQ_FFI_DIR")
        endif()
    endif()
    foreach(_name IN ITEMS ${STATIC} ${DYNAMIC})
        set(_found "")
        math(EXPR _last "${_count} - 1")
        foreach(_i RANGE ${_last})
            string(JSON _file GET "${_artifact}" filenames ${_i})
            get_filename_component(_base "${_file}" NAME)
            if(_base STREQUAL _name)
                set(_found "${_file}")
            endif()
        endforeach()
        if(NOT _found)
            message(FATAL_ERROR "cargo built no ${_name} for moq_ffi")
        endif()
        # A hard link costs no space and keeps cargo's timestamp, so an unchanged library
        # relinks nothing; a copy is the fallback across filesystems. Not a symlink, which
        # install() would ship as a dangling link.
        file(MAKE_DIRECTORY "${OUT}")
        file(REMOVE "${OUT}/${_name}.tmp")
        file(CREATE_LINK "${_found}" "${OUT}/${_name}.tmp" RESULT _linked)
        if(_linked EQUAL 0)
            file(RENAME "${OUT}/${_name}.tmp" "${OUT}/${_name}")
        else()
            file(COPY_FILE "${_found}" "${OUT}/${_name}" ONLY_IF_DIFFERENT)
        endif()
    endforeach()
    return()
endif()

if(WIN32)
    set(_moq_ffi_static moq_ffi.lib)
    set(_moq_ffi_dynamic moq_ffi.dll)
elseif(APPLE)
    set(_moq_ffi_static libmoq_ffi.a)
    set(_moq_ffi_dynamic libmoq_ffi.dylib)
else()
    set(_moq_ffi_static libmoq_ffi.a)
    set(_moq_ffi_dynamic libmoq_ffi.so)
endif()
get_filename_component(_moq_workspace "${CMAKE_CURRENT_LIST_DIR}/../../.." ABSOLUTE)

# The architecture the compiler targets, normalized to x86_64 or aarch64. A same-OS override
# (`-m32`, Visual Studio's `-A`, CMAKE_OSX_ARCHITECTURES) changes it without changing
# CMAKE_SYSTEM_PROCESSOR or setting CMAKE_CROSSCOMPILING, so both inputs compare against it.
set(_moq_arch "${CMAKE_SYSTEM_PROCESSOR}")
if(CMAKE_CXX_COMPILER_ARCHITECTURE_ID)
    set(_moq_arch "${CMAKE_CXX_COMPILER_ARCHITECTURE_ID}")
endif()
if(APPLE AND CMAKE_OSX_ARCHITECTURES)
    set(_moq_arch "${CMAKE_OSX_ARCHITECTURES}")
endif()
set(_moq_host_arch "${CMAKE_HOST_SYSTEM_PROCESSOR}")
foreach(_var _moq_arch _moq_host_arch)
    if(${_var} MATCHES "^(x86_64|AMD64|amd64|x64)$")
        set(${_var} x86_64)
    elseif(${_var} MATCHES "^(aarch64|arm64|ARM64)$")
        set(${_var} aarch64)
    endif()
endforeach()
if(NOT CMAKE_SIZEOF_VOID_P EQUAL 8)
    set(_moq_arch "${CMAKE_SIZEOF_VOID_P}-byte pointers")
endif()

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
    unset(_ffi_target)
    if(CMAKE_SYSTEM_NAME STREQUAL "Linux" AND _moq_arch MATCHES "^(x86_64|aarch64)$")
        set(_ffi_target ${_moq_arch}-unknown-linux-gnu)
    elseif(APPLE AND _moq_arch STREQUAL "aarch64")
        set(_ffi_target aarch64-apple-darwin)
    elseif(MSVC AND _moq_arch STREQUAL "x86_64")
        set(_ffi_target x86_64-pc-windows-msvc)
    endif()
    if(NOT DEFINED _ffi_target OR NOT _ffi_target STREQUAL _input_target)
        message(FATAL_ERROR "Prebuilt moq-ffi target ${_input_target} does not match this host")
    endif()
    set(MOQ_FFI_LIB "${MOQ_FFI_DIR}/lib/${_moq_ffi_static}")
    set(MOQ_FFI_METADATA "${MOQ_FFI_DIR}/lib/${_moq_ffi_dynamic}")
    set(_native_libs "${MOQ_FFI_DIR}/native-libs.txt")
    foreach(_input "${MOQ_FFI_LIB}" "${MOQ_FFI_METADATA}" "${_native_libs}")
        if(NOT EXISTS "${_input}")
            message(FATAL_ERROR "Missing prebuilt FFI input: ${_input}")
        endif()
    endforeach()
    set(MOQ_FFI_CARGO "")
else()
    # Cargo builds for the host, so a project targeting anything else stages a prebuilt input.
    if(CMAKE_CROSSCOMPILING OR NOT _moq_arch STREQUAL _moq_host_arch)
        message(FATAL_ERROR "A source build of moq-ffi targets the host; for another target "
            "or architecture, set MOQ_FFI_DIR to a native package for it")
    endif()
    # The top-level build tree, so a project adding both packages shares one cargo build and
    # one rule for its outputs, as Ninja requires.
    set(_out "${CMAKE_BINARY_DIR}/moq-ffi")
    set(MOQ_FFI_LIB "${_out}/${_moq_ffi_static}")
    set(MOQ_FFI_METADATA "${_out}/${_moq_ffi_dynamic}")
    set(MOQ_FFI_CARGO moq_ffi_cargo)
    if(NOT TARGET moq_ffi_cargo)
        # Multi-config generators leave CMAKE_BUILD_TYPE empty and get a release build.
        set(_release ON)
        if(CMAKE_BUILD_TYPE STREQUAL "Debug")
            set(_release OFF)
        endif()
        # The packages ship the OpenH264 software fallback, like the prebuilt input does.
        add_custom_target(moq_ffi_cargo
            COMMAND "${CMAKE_COMMAND}" -DWORKSPACE=${_moq_workspace} -DOUT=${_out}
                -DSTATIC=${_moq_ffi_static} -DDYNAMIC=${_moq_ffi_dynamic} -DRELEASE=${_release}
                -P "${CMAKE_CURRENT_LIST_FILE}"
            BYPRODUCTS "${MOQ_FFI_LIB}" "${MOQ_FFI_METADATA}"
            COMMENT "Building moq-ffi"
            VERBATIM
        )
    endif()
    # System libraries the staticlib needs from an external linker. moq-ffi and moq-c share one
    # dependency tree, so they share rs/moq-c/native-libs/, which says how to regenerate it.
    if(APPLE)
        set(_platform apple)
    elseif(WIN32)
        set(_platform windows)
    else()
        set(_platform linux)
    endif()
    set(_native_libs "${_moq_workspace}/rs/moq-c/native-libs/${_platform}.txt")
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
