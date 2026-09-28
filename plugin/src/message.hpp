// Messages passed from the game thread to the sender thread, and the common
// wire envelope. Wire format: docs/WIRE.md.
#pragma once

#include <cstdint>
#include <string>

#include "json.hpp"
#include "platform.hpp"

namespace pt {

constexpr int kProtocolVersion = 1;
constexpr const char* kPluginVersion = "0.1.0";

enum class Kind : std::uint8_t {
    Init, Config, Gameplay, Paused, Started, TimerRestart, Sample, Heartbeat, Shutdown, Integrity, Modules, Scale, Truckersmp,
};

inline const char* kind_name(Kind k) {
    switch (k) {
        case Kind::Init: return "init";
        case Kind::Config: return "config";
        case Kind::Gameplay: return "gameplay";
        case Kind::Paused: return "paused";
        case Kind::Started: return "started";
        case Kind::TimerRestart: return "timer_restart";
        case Kind::Sample: return "sample";
        case Kind::Heartbeat: return "heartbeat";
        case Kind::Shutdown: return "shutdown";
        case Kind::Integrity: return "integrity";
        case Kind::Modules: return "modules";
        case Kind::Scale: return "scale";
        case Kind::Truckersmp: return "truckersmp";
    }
    return "unknown";
}

// Durable kinds are journaled to disk until the client acknowledges them.
inline bool is_durable(Kind k) { return k != Kind::Sample && k != Kind::Heartbeat; }

struct Msg {
    std::uint64_t seq = 0;
    Kind kind = Kind::Heartbeat;
    std::string config_id;  // Kind::Config only
    std::string json;
};

// Opens a message object with the common envelope fields. The caller adds its
// own fields and calls end_object().
inline void envelope(JsonWriter& w, Kind kind, const std::string& sess, std::uint64_t seq) {
    w.begin_object();
    w.key("v").i64(kProtocolVersion);
    w.key("kind").str(kind_name(kind));
    w.key("sess").str(sess);
    w.key("seq").u64(seq);
    w.key("wall_ms").i64(wall_ms());
}

// Length-prefixed frame: u32 little-endian byte length, then UTF-8 JSON.
inline void append_frame(std::string& out, const std::string& json) {
    const auto n = static_cast<std::uint32_t>(json.size());
    const char len[4] = {char(n & 0xff), char((n >> 8) & 0xff), char((n >> 16) & 0xff), char((n >> 24) & 0xff)};
    out.append(len, 4);
    out += json;
}

}  // namespace pt
