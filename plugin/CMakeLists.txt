cmake_minimum_required(VERSION 3.20)
project(plasmatrack_plugin LANGUAGES CXX)

# The SCS SDK headers are C++ only (unconditional extern "C" blocks, inheritance).
set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)
if(NOT CMAKE_BUILD_TYPE AND NOT CMAKE_CONFIGURATION_TYPES)
  set(CMAKE_BUILD_TYPE Release)
endif()

set(SCS_SDK_INCLUDE "${CMAKE_CURRENT_SOURCE_DIR}/../third_party/scssdk-1.14/include")
# TruckersMP client SDK (MIT, header-only; only its C layer is used).
set(TMP_SDK_INCLUDE "${CMAKE_CURRENT_SOURCE_DIR}/../third_party/truckersmp-client-sdk-1.1.0/include")

# Everything except the SDK entry points, shared by the plugin and the tests.
add_library(pt_plugin_core STATIC
  src/journal.cpp
  src/platform_posix.cpp
  src/platform_win.cpp
  src/sender.cpp
  src/settings.cpp
  src/sha256.cpp
)
set_target_properties(pt_plugin_core PROPERTIES POSITION_INDEPENDENT_CODE ON)
target_include_directories(pt_plugin_core PUBLIC src "${SCS_SDK_INCLUDE}")
if(CMAKE_CXX_COMPILER_ID MATCHES "Clang|GNU")
  target_compile_options(pt_plugin_core PUBLIC -Wall -Wextra -Wno-unused-parameter)
endif()
if(WIN32)
  target_compile_definitions(pt_plugin_core PUBLIC WIN32_LEAN_AND_MEAN NOMINMAX _WIN32_WINNT=0x0601)
  # K32EnumProcessModules / K32GetModuleBaseNameW live in kernel32 (no psapi.dll).
  target_link_libraries(pt_plugin_core PUBLIC ws2_32 bcrypt)
else()
  find_package(Threads REQUIRED)
  target_link_libraries(pt_plugin_core PUBLIC Threads::Threads ${CMAKE_DL_LIBS})
endif()

# ---- the plugin -----------------------------------------------------------
add_library(plasmatrack_telemetry SHARED src/plugin.cpp src/tmp.cpp)
target_link_libraries(plasmatrack_telemetry PRIVATE pt_plugin_core)
target_include_directories(plasmatrack_telemetry PRIVATE "${TMP_SDK_INCLUDE}")
set_target_properties(plasmatrack_telemetry PROPERTIES PREFIX "" OUTPUT_NAME plasmatrack)
if(WIN32)
  target_sources(plasmatrack_telemetry PRIVATE plasmatrack.def)
  # Self-contained DLL: no libc++/libstdc++, libunwind/libgcc or winpthread DLLs
  # next to the game. Only system DLLs may remain (checked by the imports test).
  target_link_options(plasmatrack_telemetry PRIVATE -static)
  if(CMAKE_CXX_COMPILER_ID STREQUAL "GNU")
    target_link_options(plasmatrack_telemetry PRIVATE -static-libgcc -static-libstdc++)
  endif()
else()
  target_compile_options(plasmatrack_telemetry PRIVATE -fvisibility=hidden)
  target_link_options(plasmatrack_telemetry PRIVATE
    -static-libstdc++ -static-libgcc
    "-Wl,--version-script=${CMAKE_CURRENT_SOURCE_DIR}/plasmatrack.map")
endif()

# ---- tests ----------------------------------------------------------------
include(CTest)
if(BUILD_TESTING)
  add_executable(pt_unit_tests tests/unit_tests.cpp)
  target_link_libraries(pt_unit_tests PRIVATE pt_plugin_core)
  if(WIN32)
    target_link_options(pt_unit_tests PRIVATE -static)
  endif()
  add_test(NAME unit_tests COMMAND pt_unit_tests)

  add_executable(pt_fake_game tests/fake_game.cpp)
  target_link_libraries(pt_fake_game PRIVATE pt_plugin_core)
  target_include_directories(pt_fake_game PRIVATE "${TMP_SDK_INCLUDE}")
  if(WIN32)
    target_link_options(pt_fake_game PRIVATE -static)
  endif()
  add_test(NAME fake_game COMMAND pt_fake_game $<TARGET_FILE:plasmatrack_telemetry>)

  # Used by scripts/e2e-plugin-client.ps1 together with the real client binary.
  add_executable(pt_drive_plugin tests/drive_plugin.cpp)
  target_link_libraries(pt_drive_plugin PRIVATE pt_plugin_core)
  if(WIN32)
    target_link_options(pt_drive_plugin PRIVATE -static)
  endif()

  if(WIN32)
    if(NOT PT_OBJDUMP)
      find_program(PT_OBJDUMP NAMES llvm-objdump x86_64-w64-mingw32-objdump objdump)
    endif()
    if(PT_OBJDUMP)
      add_test(NAME dll_imports_exports
        COMMAND ${CMAKE_COMMAND} -DOBJDUMP=${PT_OBJDUMP} -DDLL=$<TARGET_FILE:plasmatrack_telemetry>
                -P ${CMAKE_CURRENT_SOURCE_DIR}/cmake/check_dll.cmake)
    endif()
  endif()
endif()
