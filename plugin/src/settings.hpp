// Plugin settings, read from `plasmatrack.ini` next to the plugin module.
//
// The file lives beside the DLL (not in the user's home) because Flatpak Steam
// cannot see the host home directory; the PlasmaTrack client writes it into
// every Steam library it finds. Missing keys use the defaults below.
#pragma once

#include <cstdint>
#include <string>

namespace pt {

struct Settings {
    std::uint16_t port = 30455;
    // Samples per second of paused-simulation time; 0 = every physics frame.
    double sample_rate_hz = 20.0;
    // Wall-clock seconds of samples kept in memory for a reconnecting client.
    double replay_buffer_s = 120.0;
    // Capture mode (docs/CAPTURE.md): sample every frame and also write every
    // message to <plugin dir>/plasmatrack/captures/.
    bool capture = false;
    // Upper bound for one session's on-disk journal, in MiB.
    std::uint32_t journal_max_mib = 64;
};

// Parses `key = value` lines; `#` and `;` start comments. Unknown keys and
// malformed values are ignored (defaults kept). Locale-independent.
Settings parse_settings(const std::string& text);
Settings load_settings(const std::string& ini_path);

}  // namespace pt
