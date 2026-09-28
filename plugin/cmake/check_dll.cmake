# Verifies the built DLL:
#  - exports scs_telemetry_init and scs_telemetry_shutdown (SCS SDK) and
#    truckersmp_init and truckersmp_shutdown (TruckersMP client SDK), undecorated
#  - imports only DLLs that ship with Windows (and Wine/Proton): no MinGW
#    runtime DLLs (libc++, libunwind, libstdc++, libgcc, libwinpthread).
execute_process(COMMAND "${OBJDUMP}" -p "${DLL}" OUTPUT_VARIABLE out RESULT_VARIABLE rc)
if(NOT rc EQUAL 0)
  message(FATAL_ERROR "objdump failed on ${DLL}")
endif()

string(REGEX MATCHALL "DLL Name: [^\r\n]+" imports "${out}")
set(allowed "^(kernel32|ws2_32|bcrypt|advapi32|ntdll|ucrtbase|msvcrt|api-ms-win-[a-z0-9-]+)\\.dll$")
foreach(line IN LISTS imports)
  string(REPLACE "DLL Name: " "" dll "${line}")
  string(STRIP "${dll}" dll)
  string(TOLOWER "${dll}" dll_l)
  message(STATUS "import: ${dll}")
  if(NOT dll_l MATCHES "${allowed}")
    message(FATAL_ERROR "Unexpected DLL dependency: ${dll}")
  endif()
endforeach()

foreach(sym scs_telemetry_init scs_telemetry_shutdown truckersmp_init truckersmp_shutdown)
  if(NOT out MATCHES "[ \t]${sym}[\r\n]")
    message(FATAL_ERROR "Missing export: ${sym}")
  endif()
endforeach()
message(STATUS "DLL imports/exports OK")
