# Third-party notices

| Component | Where it ends up | License | Text |
|---|---|---|---|
| SCS Telemetry SDK 1.14 headers | compiled into the plugin | MIT, © 2016 SCS Software | `third_party/scssdk-1.14/sdk_license.txt` |
| LLVM libc++, libc++abi, libunwind, compiler-rt (llvm-mingw) | statically linked into the Windows plugin DLL | Apache-2.0 WITH LLVM-exception | `third_party/notices/LLVM-LICENSE.txt` |
| MinGW-w64 runtime (CRT startup, headers) | statically linked into the Windows plugin DLL | ZPL-2.1 and others listed in the file | `third_party/notices/COPYING.MinGW-w64-runtime.txt` |
| winpthreads | may be statically linked by GCC-based builds | MIT / BSD-3-Clause | `third_party/notices/winpthreads-COPYING.txt` |
| Rust crates (serde, toml, thiserror, serde_json, sha2) | pt-core | MIT OR Apache-2.0 | crate sources (`cargo metadata`) |

`third_party/notices/COPYING.MinGW-w64-runtime.txt` and `winpthreads-COPYING.txt` were fetched from
https://github.com/mingw-w64/mingw-w64 (master) on 2026-09-27. `LLVM-LICENSE.txt` is `LICENSE.TXT` from
llvm-mingw 20260922 (UCRT, x86_64). Ship this file and `third_party/notices/` with every plugin release.
