# TruckersMP Game Client SDK 1.1.0 (vendored)

- Source: https://github.com/TruckersMP/GameClientSDK
- Commit: bee3b9e2b314c2b47fee9f94635e9945a71924e1 (2026-08-28)
- `TRUCKERSMP_SDK_VERSION` = 0x01000100 (1.1.0), include/TruckersMP/TruckersMP.h:13
- License: MIT (LICENSE, copied unmodified)
- Docs: https://docs.truckersmp.com/client-sdk/
- Header-only; nothing is linked. PlasmaTrack uses only the C layer (TruckersMP_Base.h, TruckersMP.h).
- TruckersMP supports an SDK version for about one year (docs, concepts/versioning), so the plugin must be rebuilt against newer headers at least yearly.

SHA-256 of the vendored files:

```
28a9bc7ab45c72530ec4612917c7abe204849a49d7a987074951065cb1e34723 *LICENSE
4ee9201906126d22392f775e6ae84fd68a1cf784c68bc56684e95f4029aa039f *include/TruckersMP/Bus.hxx
ce084eec73f1a45635f6009ad18ca95497e6307a8a1bdb41b627003a5c0a3bde *include/TruckersMP/TruckersMP.h
3879ee04d334dc59d394a914e16ca591afd44ee900eca8f2828276b38ee834f5 *include/TruckersMP/TruckersMP.hxx
ebef58cba20cd6f7a97fb687f303301c8e42875b6a9d5a677fe5f4a2606f0aac *include/TruckersMP/TruckersMP_Base.h
c2f06d3cb61dc6d9d2726698c0bb4c754fc40e280fd6057c1c2f340e2fd6f651 *include/TruckersMP/TruckersMP_Bus.h
e416d1bb5dee566da121807a4850f62cf50010ff4270fa9699790a8691118298 *include/TruckersMP/TruckersMP_Runtime.hxx
```
