cmake_minimum_required(VERSION 3.18)
project(webwm-cef C)

set(CMAKE_C_STANDARD 11)

# ---------------------------------------------------------------------------
# CEF root — set via -DCEF_ROOT=/path/to/cef_binary_... or env var CEF_ROOT
# ---------------------------------------------------------------------------
if(NOT CEF_ROOT)
    set(CEF_ROOT "$ENV{CEF_ROOT}")
endif()

if(CEF_ROOT)
    list(APPEND CMAKE_PREFIX_PATH "${CEF_ROOT}")
    find_library(CEF_LIB NAMES cef libcef PATHS "${CEF_ROOT}" "${CEF_ROOT}/Release" NO_DEFAULT_PATH)
    include_directories("${CEF_ROOT}")
else()
    message(WARNING "CEF_ROOT not set — building stub without real CEF linkage")
    set(CEF_LIB "")
endif()

# ---------------------------------------------------------------------------
# DRM / KMS
# ---------------------------------------------------------------------------
find_package(PkgConfig REQUIRED)
pkg_check_modules(DRM  QUIET libdrm)
pkg_check_modules(GBM  QUIET gbm)
pkg_check_modules(EGL  QUIET egl)
pkg_check_modules(GLES QUIET glesv2)

add_executable(webwm-cef
    src/main.c
    src/drm_output.c
    src/input_fwd.c
    src/cef_app.c
)

target_compile_definitions(webwm-cef PRIVATE
    WM_FRONTEND_URL="http://localhost:7703"
    INPUT_WS_URL="ws://localhost:7701"
)

if(DRM_FOUND)
    target_include_directories(webwm-cef PRIVATE ${DRM_INCLUDE_DIRS})
    target_link_libraries(webwm-cef ${DRM_LIBRARIES} ${GBM_LIBRARIES} ${EGL_LIBRARIES} ${GLES_LIBRARIES})
endif()

if(CEF_LIB)
    target_link_libraries(webwm-cef ${CEF_LIB})
    target_compile_definitions(webwm-cef PRIVATE HAVE_CEF)
endif()
