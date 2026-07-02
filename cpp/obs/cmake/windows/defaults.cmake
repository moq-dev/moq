# CMake Windows defaults module

include_guard(GLOBAL)

# Enable find_package targets to become globally available targets
set(CMAKE_FIND_PACKAGE_TARGETS_GLOBAL TRUE)

include(buildspec)

if(CMAKE_INSTALL_PREFIX_INITIALIZED_TO_DEFAULT)
  cmake_path(SET ALLUSERSPROFILE_PATH $ENV{ALLUSERSPROFILE})
  set(
    CMAKE_INSTALL_PREFIX
    "${ALLUSERSPROFILE_PATH}/obs-studio/plugins"
    CACHE STRING
    "Default plugin installation directory"
    FORCE
  )
endif()
