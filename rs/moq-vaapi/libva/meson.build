# libva package version number, (as distinct from shared library version)
# XXX: we want the package version to remain at 1.0.x for VA-API 0.32.y
#
# - major version is automatically generated from VA-API major version
# - minor version is automatically generated from VA-API minor version
# - increment micro for any library release
# - reset micro version to zero when VA-API major or minor version is changed
project(
  'libva', 'c',
  version : '2.23.0',
  meson_version : '>= 0.53.0',
  default_options : [ 'warning_level=1',
                      'buildtype=debugoptimized' ])

# VA-API version
# - increment major for any ABI change
# - increment minor for any interface change (e.g. new/modified function)
# - increment micro for any other change (new flag, new codec definition, etc.)
# - reset micro version to zero when minor version is incremented
# - reset minor version to zero when major version is incremented
va_api_major_version = 1
va_api_minor_version = 23
va_api_micro_version = 0

va_api_version = '@0@.@1@.@2@'.format(va_api_major_version,
				      va_api_minor_version,
				      va_api_micro_version)

version_arr = meson.project_version().split('.')
libva_major_version = version_arr[0]
libva_minor_version = version_arr[1]
libva_micro_version = version_arr[2]
libva_version = '@0@.@1@.@2@'.format(libva_major_version,
				     libva_minor_version,
				     libva_micro_version)
if version_arr.length() == 4
  libva_version = '@0@.pre@1@'.format(libva_version, version_arr[3])
endif


# libva library version number (generated, do not change)
# XXX: we want the SONAME to remain at libva.so.1 for VA-API major == 0
#
# The library name is generated libva.<x>.<y>.0 where
# <x> = VA-API major version + 1
# <y> = 100 * VA-API minor version + VA-API micro version
#
# For example:
# VA-API 0.32.0 generates libva.so.1.3200.0
# VA-API 0.34.1 generates libva.so.1.3401.0
# VA-API 1.2.13 generates libva.so.2.213.0
libva_interface_bias = va_api_major_version + 1
libva_interface_age = 0
libva_binary_age = 100 * va_api_minor_version + va_api_micro_version - libva_interface_age

libva_lt_current = 100 * va_api_minor_version + va_api_micro_version + libva_interface_bias
libva_lt_revision = libva_interface_age
libva_lt_age = libva_binary_age - libva_interface_age

libva_lt_current = libva_lt_current - libva_lt_age

libva_lt_version = '@0@.@1@.@2@'.format(libva_lt_current,
					libva_lt_age,
					libva_lt_revision)

sysconfdir = join_paths(get_option('prefix'), get_option('sysconfdir'))

driverdir = get_option('driverdir')
if driverdir == ''
  # "libdir" on Windows is essentially only for static and import libraries,
  # while "bindir" is the actual runtime directory - containing both
  # executable and dynamic libraries. During install meson uses correct install
  # location depending on the type of library, requiring zero user intervention
  # in the common case.
  if host_machine.system() == 'windows'
    driverdir = join_paths(get_option('prefix'), get_option('bindir'))
  else
    driverdir = join_paths(get_option('prefix'), get_option('libdir'), 'dri')
  endif
endif

configinc = include_directories('.')

cc = meson.get_compiler('c')
dl_dep = cc.find_library('dl', required : false)

WITH_DRM = not get_option('disable_drm') and (host_machine.system() != 'windows')
libdrm_dep = dependency('libdrm', version : '>= 2.4.75', required : (host_machine.system() != 'windows'))

WITH_X11 = false
if get_option('with_x11') != 'no'
  x11_dep = dependency('x11', required : get_option('with_x11') == 'yes')
  xext_dep = dependency('xext', required : get_option('with_x11') == 'yes')
  xfixes_dep = dependency('xfixes', required : get_option('with_x11') == 'yes')

  WITH_X11 = (x11_dep.found() and xext_dep.found() and xfixes_dep.found())

  x11_xcb_dep = dependency('x11-xcb', required : get_option('with_x11') == 'yes')
  xcb_dep = dependency('xcb', required : get_option('with_x11') == 'yes')
  xcb_dri3_dep = dependency('xcb-dri3', required : get_option('with_x11') == 'yes')
  WITH_X11 = (WITH_X11 and x11_xcb_dep.found() and xcb_dep.found() and xcb_dri3_dep.found())
endif

if not WITH_X11 and get_option('with_glx') == 'yes'
  error('VA/GLX explicitly enabled, but VA/X11 isn\'t built')
endif

WITH_GLX = false
if WITH_X11 and get_option('with_glx') != 'no'
  gl_dep = dependency('gl', required : get_option('with_glx') == 'yes')
  WITH_GLX = gl_dep.found()
endif

WITH_WAYLAND = false
if get_option('with_wayland') != 'no'
  wayland_dep = dependency('wayland-client', version : '>= 1.11.0',
			   required : get_option('with_wayland') == 'yes')
  wayland_scanner_dep = dependency('wayland-scanner', version : '>= 1.15',
                                   required : get_option('with_wayland') == 'yes',
                                   native : true)
  if wayland_scanner_dep.found()
    wl_scanner = find_program(wayland_scanner_dep.get_variable(pkgconfig: 'wayland_scanner'))
  endif
  WITH_WAYLAND = wayland_dep.found() and wayland_scanner_dep.found()
endif

WITH_WIN32 = false
libwin32_dep = []
if get_option('with_win32') != 'no'
    WITH_WIN32 = (host_machine.system() == 'windows')
endif

if (not WITH_DRM and not WITH_X11 and not WITH_WAYLAND and not WITH_WIN32)
  error('Please install at least one backend dev files (DRM, X11, Wayland, WIN32)')
endif

c_args = []
if get_option('with_legacy').contains('emgd')
  c_args += ['-DHAVE_EMGD']
elif get_option('with_legacy').contains('nvctrl')
  c_args += ['-DHAVE_NVCTRL']
elif get_option('with_legacy').contains('fglrx')
  c_args += ['-DHAVE_FGLRX']
endif

if cc.has_function('secure_getenv')
  c_args += ['-DHAVE_SECURE_GETENV']
endif

add_project_arguments(c_args, language: ['c'])

subdir('va')
subdir('pkgconfig')

doxygen = find_program('doxygen', required: false)

if get_option('enable_docs') and doxygen.found()
  subdir('doc')
endif

