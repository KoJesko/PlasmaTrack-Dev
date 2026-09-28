// Drives the real plugin through a short scripted job so it talks to a real,
// separately running PlasmaTrack client (scripts/e2e-plugin-client.ps1).
//
// Usage: pt_drive_plugin <plasmatrack.dll> <work dir> <client port>
// Prints the plugin session id on success.
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <map>
#include <string>
#include <thread>
#include <vector>

#ifdef _WIN32
#include <windows.h>
#else
#include <dlfcn.h>
#endif

#include "scssdk_telemetry.h"
#include "common/scssdk_telemetry_common_channels.h"
#include "common/scssdk_telemetry_common_configs.h"
#include "common/scssdk_telemetry_common_gameplay_events.h"
#include "common/scssdk_telemetry_truck_common_channels.h"
#include "amtrucks/scssdk_ats.h"
#include "amtrucks/scssdk_telemetry_ats.h"

#include "platform.hpp"

namespace {
std::map<scs_event_t, scs_telemetry_event_callback_t> g_events;
struct Ch { std::string name; scs_value_type_t type; scs_telemetry_channel_callback_t cb; scs_context_t ctx; };
std::vector<Ch> g_channels;

SCSAPI_RESULT reg_event(const scs_event_t e, const scs_telemetry_event_callback_t cb, const scs_context_t) { g_events[e] = cb; return SCS_RESULT_ok; }
SCSAPI_RESULT unreg_event(const scs_event_t e) { g_events.erase(e); return SCS_RESULT_ok; }
SCSAPI_RESULT reg_channel(const scs_string_t n, const scs_u32_t, const scs_value_type_t t, const scs_u32_t,
                          const scs_telemetry_channel_callback_t cb, const scs_context_t ctx) {
    g_channels.push_back({n, t, cb, ctx});
    return SCS_RESULT_ok;
}
SCSAPI_RESULT unreg_channel(const scs_string_t, const scs_u32_t, const scs_value_type_t) { return SCS_RESULT_ok; }
SCSAPI_VOID log_fn(const scs_log_type_t, const scs_string_t msg) { std::fprintf(stderr, "game log: %s\n", msg); }

void fire(scs_event_t e, const void* info = nullptr) {
    auto it = g_events.find(e);
    if (it != g_events.end()) it->second(e, info, nullptr);
}

scs_named_value_t attr(const char* name, scs_value_type_t type) {
    scs_named_value_t a{};
    a.name = name;
    a.index = SCS_U32_NIL;
    a.value.type = type;
    return a;
}
}  // namespace

int main(int argc, char** argv) {
    if (argc != 4) {
        std::fprintf(stderr, "usage: %s <plugin> <work dir> <port>\n", argv[0]);
        return 2;
    }
    const std::string work = argv[2];
    pt::make_dirs(work);
    const std::string dll = pt::join_path(work, "plasmatrack.dll");
    {
        std::FILE* in = pt::open_file_utf8(argv[1], "rb");
        std::FILE* out = pt::open_file_utf8(dll, "wb");
        if (!in || !out) return 1;
        char buf[65536];
        std::size_t n;
        while ((n = std::fread(buf, 1, sizeof buf, in)) > 0) std::fwrite(buf, 1, n, out);
        std::fclose(in);
        std::fclose(out);
        std::FILE* ini = pt::open_file_utf8(pt::join_path(work, "plasmatrack.ini"), "wb");
        std::fprintf(ini, "port = %s\nsample_rate_hz = 20\n", argv[3]);
        std::fclose(ini);
    }
#ifdef _WIN32
    HMODULE lib = LoadLibraryA(dll.c_str());
    auto init = reinterpret_cast<scs_result_t (*)(scs_u32_t, const scs_telemetry_init_params_t*)>(
        reinterpret_cast<void*>(GetProcAddress(lib, "scs_telemetry_init")));
    auto shutdown = reinterpret_cast<void (*)()>(reinterpret_cast<void*>(GetProcAddress(lib, "scs_telemetry_shutdown")));
#else
    void* lib = dlopen(dll.c_str(), RTLD_NOW);
    auto init = reinterpret_cast<scs_result_t (*)(scs_u32_t, const scs_telemetry_init_params_t*)>(dlsym(lib, "scs_telemetry_init"));
    auto shutdown = reinterpret_cast<void (*)()>(dlsym(lib, "scs_telemetry_shutdown"));
#endif
    if (!lib || !init || !shutdown) return 1;

    scs_telemetry_init_params_v101_t p{};
    p.common.game_name = "American Truck Simulator";
    p.common.game_id = SCS_GAME_ID_ATS;
    p.common.game_version = SCS_TELEMETRY_ATS_GAME_VERSION_1_05;
    p.common.log = log_fn;
    p.register_for_event = reg_event;
    p.unregister_from_event = unreg_event;
    p.register_for_channel = reg_channel;
    p.unregister_from_channel = unreg_channel;
    if (init(SCS_TELEMETRY_VERSION_1_01, &p) != SCS_RESULT_ok) return 1;

    scs_named_value_t job[4] = {attr(SCS_TELEMETRY_CONFIG_ATTRIBUTE_cargo_id, SCS_VALUE_TYPE_string),
                                attr(SCS_TELEMETRY_CONFIG_ATTRIBUTE_planned_distance_km, SCS_VALUE_TYPE_u32),
                                attr(SCS_TELEMETRY_CONFIG_ATTRIBUTE_destination_city, SCS_VALUE_TYPE_string), {}};
    job[0].value.value_string.value = "e2e_cargo";
    job[1].value.value_u32.value = 5;
    job[2].value.value_string.value = "Las Vegas";
    scs_telemetry_configuration_t job_cfg{SCS_TELEMETRY_CONFIG_job, job};
    fire(SCS_TELEMETRY_EVENT_configuration, &job_cfg);
    fire(SCS_TELEMETRY_EVENT_started);

    scs_named_value_t delivered[2] = {attr(SCS_TELEMETRY_GAMEPLAY_EVENT_ATTRIBUTE_revenue, SCS_VALUE_TYPE_s64), {}};
    delivered[0].value.value_s64.value = 777;
    scs_telemetry_gameplay_event_t ev{SCS_TELEMETRY_GAMEPLAY_EVENT_job_delivered, delivered};

    // 3 s of simulated driving at 60 Hz, paced in real time so the plugin's
    // sender thread and the client exchange frames and acks while it runs.
    scs_timestamp_t sim = 0;
    double odo = 1000.0;
    for (int i = 0; i < 180; ++i) {
        sim += 16667;
        odo += 25.0 * 0.016667 / 1000.0;
        scs_telemetry_frame_start_t fs{};
        fs.simulation_time = fs.paused_simulation_time = fs.render_time = sim;
        fire(SCS_TELEMETRY_EVENT_frame_start, &fs);
        if (i == 170) fire(SCS_TELEMETRY_EVENT_gameplay, &ev);
        for (auto& c : g_channels) {
            scs_value_t v{};
            v.type = c.type;
            if (c.name == SCS_TELEMETRY_TRUCK_CHANNEL_speed) v.value_float.value = 25.0f;
            else if (c.name == SCS_TELEMETRY_TRUCK_CHANNEL_odometer) v.value_float.value = float(odo);
            else if (c.name == SCS_TELEMETRY_TRUCK_CHANNEL_fuel) v.value_float.value = 500.0f;
            else if (c.name == SCS_TELEMETRY_TRUCK_CHANNEL_world_placement) v.value_dplacement.position = {double(i), 0, 0};
            c.cb(c.name.c_str(), SCS_U32_NIL, &v, c.ctx);
        }
        fire(SCS_TELEMETRY_EVENT_frame_end);
        std::this_thread::sleep_for(std::chrono::milliseconds(16));
    }
    fire(SCS_TELEMETRY_EVENT_paused);
    std::this_thread::sleep_for(std::chrono::milliseconds(1000));  // let the client ack
    shutdown();
#ifdef _WIN32
    FreeLibrary(lib);
#else
    dlclose(lib);
#endif
    // The session id is the name of the plugin's journal file.
    for (const auto& f : pt::list_dir(pt::join_path(work, "plasmatrack/journal"))) {
        if (f.size() == 38 && f.substr(32) == ".jsonl") std::printf("%s\n", f.substr(0, 32).c_str());
    }
    return 0;
}
