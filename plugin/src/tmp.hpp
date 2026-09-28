// TruckersMP client SDK side of the plugin (third_party/truckersmp-client-sdk-1.1.0).
//
// The TruckersMP client loads plugins from the same folder as SCS telemetry
// plugins and calls `truckersmp_init` / `truckersmp_shutdown` (TruckersMP_Base.h:121-129),
// so plasmatrack.dll exports both entry points. The telemetry side reports this
// state as `truckersmp` messages (docs/WIRE.md): it tells a job driven on a
// TruckersMP server apart from singleplayer launched through the TruckersMP
// launcher, which a loaded-module check cannot (PLAN §6b).
#pragma once

#include <atomic>
#include <cstdint>

namespace pt::tmp {

struct State {
    std::atomic<bool> loaded{false};     // truckersmp_init succeeded
    std::atomic<bool> connected{false};  // TruckersMP_Network::IsConnected
    std::atomic<std::uint64_t> server_id{0};
    std::atomic<std::uint64_t> account_id{0};
    std::atomic<std::uint32_t> version{0};  // bumped on every change
};

State& state();

}  // namespace pt::tmp
