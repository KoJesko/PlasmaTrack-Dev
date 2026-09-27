# PlasmaTrack telemetry plugin

A native SCS Telemetry SDK 1.14 plugin (C++17). It forwards raw data only and makes no
scoring decisions.

- **Game thread** (`src/plugin.cpp`): SDK callbacks copy values and render JSON into a lock-free SPSC
  ring (`src/spsc_ring.hpp`). They never block. A full ring drops the message and counts it.
- **Sender thread** (`src/sender.cpp`): the only owner of the socket and the disk.
  - Connects out to the client on `127.0.0.1`.
  - Journals durable events until they are acknowledged (`src/journal.cpp`).
  - Replays on reconnect.
  - Reports the plugin's own SHA-256 and the loaded modules.
  - Never calls into the SDK.
- **Wire protocol:** `docs/WIRE.md`. **Build:** `docs/BUILD.md`. **Install:** `docs/INSTALL.md`.
  **Captures:** `docs/CAPTURE.md`.
