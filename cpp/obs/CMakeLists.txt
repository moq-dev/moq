cmake_minimum_required(VERSION 3.28)

include("${CMAKE_CURRENT_SOURCE_DIR}/cmake/common/bootstrap.cmake" NO_POLICY_SCOPE)

project(${_name} VERSION ${_version})

option(ENABLE_FRONTEND_API "Use obs-frontend-api for UI functionality" OFF)
option(ENABLE_QT "Use Qt functionality" OFF)

include(compilerconfig)
include(defaults)
include(helpers)

add_library(obs-moq MODULE)

if(${BUILD_PLUGIN})
  find_package(libobs REQUIRED)
  # FFmpeg dependency (used by the MoQ source to decode subscribed video). On
  # macOS and Windows, link the ffmpeg the obs-deps bundle ships -- the same one
  # OBS itself uses at runtime -- via the vendored FindFFmpeg finder (obs-deps
  # ship ffmpeg with no pkg-config .pc files, so it searches the prefix). On
  # Linux there's no obs-deps; pkg-config finds the system ffmpeg.
  if(WIN32 OR APPLE)
    find_package(FFmpeg REQUIRED avcodec avutil swscale swresample)
    target_link_libraries(obs-moq PRIVATE FFmpeg::avcodec FFmpeg::avutil FFmpeg::swscale FFmpeg::swresample)
  else()
    include(FindPkgConfig)
    pkg_check_modules(FFMPEG REQUIRED libavcodec libavutil libswscale libswresample)
    target_include_directories(obs-moq PRIVATE ${FFMPEG_INCLUDE_DIRS})
    target_link_directories(obs-moq PRIVATE ${FFMPEG_LIBRARY_DIRS})
    target_link_libraries(obs-moq PRIVATE ${FFMPEG_LIBRARIES})
  endif()
else()
  find_package(FFmpeg REQUIRED avcodec avutil swscale swresample)
  target_link_libraries(obs-moq PRIVATE FFmpeg::avcodec FFmpeg::avutil FFmpeg::swscale FFmpeg::swresample)
endif()

target_link_libraries(obs-moq PRIVATE OBS::libobs)

# Default to the in-tree moq checkout (this file lives at cpp/obs, so the repo
# root is two levels up). A consumer building cpp/obs in isolation can point
# MOQ_LOCAL elsewhere, or unset it to fall back to the published release.
set(MOQ_LOCAL "${CMAKE_CURRENT_SOURCE_DIR}/../.." CACHE PATH "Path to the moq repo (builds rs/libmoq in-tree)")

if(MOQ_LOCAL AND EXISTS "${MOQ_LOCAL}/rs/libmoq/CMakeLists.txt")
  add_subdirectory("${MOQ_LOCAL}/rs/libmoq" moq)
  target_link_libraries(obs-moq PRIVATE moq)
else()
  include(FetchContent)
  FetchContent_Declare(
    moq
    URL
      https://github.com/moq-dev/moq/releases/download/libmoq-v${MOQ_VERSION}/moq-${MOQ_VERSION}-${MOQ_TARGET}.${MOQ_ARCHIVE}
  )
  FetchContent_MakeAvailable(moq)

  find_package(moq REQUIRED PATHS ${moq_SOURCE_DIR} NO_DEFAULT_PATH)
  target_link_libraries(obs-moq PRIVATE moq::moq)
endif()

# libmoq is a Rust static library; its std runtime pulls in symbols from Windows
# system libraries (e.g. NtCreateNamedPipeFile from ntdll) that MSVC does not
# auto-resolve when the archive is linked into this module. These are the OS
# import libraries reported by `rustc --print native-static-libs` that aren't
# linked by default; they resolve by name from the Windows SDK (no hard-coded
# paths). The libmoq package config also declares these, but linking them here
# keeps the plugin buildable against any libmoq (local dev tree or prebuilt
# release) whose config may predate that declaration. Duplicate links are
# harmless.
if(WIN32)
  target_link_libraries(
    obs-moq
    PRIVATE ntdll userenv ws2_32 dbghelp bcrypt
  )
endif()

# The obs-deps Qt6 build references the AGL framework transitively (via
# WrapOpenGL), but recent macOS SDKs ship no linkable AGL binary -- it exists
# only in the runtime dyld shared cache. Generate a stub whose install name
# points at the real framework so the link succeeds; dyld resolves AGL at load.
if(
  (ENABLE_QT OR ENABLE_FRONTEND_API)
  AND APPLE
  AND NOT EXISTS "${CMAKE_OSX_SYSROOT}/System/Library/Frameworks/AGL.framework/Versions/A/AGL"
)
  set(_agl_stub_dir "${CMAKE_BINARY_DIR}/agl-stub")
  set(_agl_stub_lib "${_agl_stub_dir}/AGL.framework/AGL")
  if(NOT EXISTS "${_agl_stub_lib}")
    file(MAKE_DIRECTORY "${_agl_stub_dir}/AGL.framework")
    set(_agl_arch_flags "")
    foreach(_arch IN LISTS CMAKE_OSX_ARCHITECTURES)
      list(APPEND _agl_arch_flags "-arch" "${_arch}")
    endforeach()
    execute_process(
      COMMAND
        xcrun clang -dynamiclib ${_agl_arch_flags} -install_name /System/Library/Frameworks/AGL.framework/Versions/A/AGL
        -o "${_agl_stub_lib}" -x c /dev/null
      RESULT_VARIABLE _agl_stub_result
    )
    if(NOT _agl_stub_result EQUAL 0)
      message(WARNING "Failed to build AGL stub (${_agl_stub_result}); Qt link may fail")
    endif()
  endif()
  target_link_options(obs-moq PRIVATE "-F${_agl_stub_dir}")
endif()

if(ENABLE_FRONTEND_API)
  find_package(obs-frontend-api REQUIRED)
  target_link_libraries(obs-moq PRIVATE OBS::obs-frontend-api)
endif()

if(ENABLE_QT)
  find_package(Qt6 COMPONENTS Widgets Core)
  target_link_libraries(obs-moq PRIVATE Qt6::Core Qt6::Widgets)
  target_compile_options(
    obs-moq
    PRIVATE $<$<C_COMPILER_ID:Clang,AppleClang>:-Wno-quoted-include-in-framework-header -Wno-comma>
  )
  set_target_properties(
    obs-moq
    PROPERTIES AUTOMOC ON AUTOUIC ON AUTORCC ON
  )
endif()

target_sources(
  obs-moq
  PRIVATE
    src/obs-moq.cpp
    src/moq-output.h
    src/moq-service.h
    src/moq-output.cpp
    src/moq-service.cpp
    src/moq-source.cpp
    src/moq-source.h
)

# The dock requires both the frontend API (to register it) and Qt (to build it).
if(ENABLE_FRONTEND_API AND ENABLE_QT)
  target_sources(obs-moq PRIVATE src/moq-dock.cpp src/moq-dock.h)
  target_compile_definitions(obs-moq PRIVATE MOQ_FRONTEND_ENABLED MOQ_VERSION_STRING="${MOQ_VERSION}")
endif()

if(${BUILD_PLUGIN})
  set_target_properties_plugin(obs-moq PROPERTIES OUTPUT_NAME ${_name})
else()
  set_target_properties_obs(obs-moq PROPERTIES FOLDER plugins PREFIX "")
endif()
