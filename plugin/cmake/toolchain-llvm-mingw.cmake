# Cross/native toolchain for llvm-mingw (UCRT), https://github.com/mstorsjo/llvm-mingw
# Usage: cmake -G Ninja -DCMAKE_TOOLCHAIN_FILE=plugin/cmake/toolchain-llvm-mingw.cmake \
#              -DLLVM_MINGW_ROOT=/opt/llvm-mingw ...
# On Windows, LLVM_MINGW_ROOT is the extracted/installed llvm-mingw folder.
set(CMAKE_SYSTEM_NAME Windows)
set(CMAKE_SYSTEM_PROCESSOR x86_64)

if(NOT LLVM_MINGW_ROOT AND DEFINED ENV{LLVM_MINGW_ROOT})
  set(LLVM_MINGW_ROOT "$ENV{LLVM_MINGW_ROOT}")
endif()
if(NOT LLVM_MINGW_ROOT)
  message(FATAL_ERROR "Set LLVM_MINGW_ROOT to the llvm-mingw install directory")
endif()
set(LLVM_MINGW_ROOT "${LLVM_MINGW_ROOT}" CACHE PATH "llvm-mingw root" FORCE)
# try_compile() re-reads this file in a fresh project; forward the setting.
set(ENV{LLVM_MINGW_ROOT} "${LLVM_MINGW_ROOT}")
list(APPEND CMAKE_TRY_COMPILE_PLATFORM_VARIABLES LLVM_MINGW_ROOT)

set(_triple x86_64-w64-mingw32)
set(CMAKE_C_COMPILER   "${LLVM_MINGW_ROOT}/bin/${_triple}-clang${CMAKE_HOST_EXECUTABLE_SUFFIX}")
set(CMAKE_CXX_COMPILER "${LLVM_MINGW_ROOT}/bin/${_triple}-clang++${CMAKE_HOST_EXECUTABLE_SUFFIX}")
set(CMAKE_RC_COMPILER  "${LLVM_MINGW_ROOT}/bin/llvm-rc${CMAKE_HOST_EXECUTABLE_SUFFIX}")
set(PT_OBJDUMP         "${LLVM_MINGW_ROOT}/bin/llvm-objdump${CMAKE_HOST_EXECUTABLE_SUFFIX}" CACHE FILEPATH "")

set(CMAKE_FIND_ROOT_PATH "${LLVM_MINGW_ROOT}/${_triple}")
set(CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER)
set(CMAKE_FIND_ROOT_PATH_MODE_LIBRARY ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_INCLUDE ONLY)

# On a Linux host, tests run under Wine if it is installed.
if(NOT CMAKE_HOST_WIN32)
  find_program(WINE_EXECUTABLE NAMES wine64 wine)
  if(WINE_EXECUTABLE)
    set(CMAKE_CROSSCOMPILING_EMULATOR "${WINE_EXECUTABLE}")
  endif()
endif()
