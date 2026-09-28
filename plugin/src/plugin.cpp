// PlasmaTrack telemetry plugin: SDK entry points and game-thread callbacks.
//
// The plugin forwards raw data only and makes no scoring decisions. Every SDK
// callback runs on the game's main thread (SDK readme.txt, "Calling conventions
// and threading"); callbacks only copy values, render JSON and push it into a
// lock-free ring. They never block and never touch the socket or the disk.
//
// SDK references (third_party/scssdk-1.14/include):
//   entry points ............ scssdk_telemetry.h:93,101
//   events .................. scssdk_telemetry_event.h:23-97
//   job/truck configs ....... common/scssdk_telemetry_common_configs.h
//   gameplay events ......... common/scssdk_telemetry_common_gameplay_events.h
//   channels ................ common/scssdk_telemetry_{common,truck_common,job_common}_channels.h

#include <algorithm>
#include <exception>
#include <memory>
#include <string>

#include "scssdk_telemetry.h"
#include "amtrucks/scssdk_ats.h"
#include "amtrucks/scssdk_telemetry_ats.h"
#include "common/scssdk_telemetry_common_channels.h"
#include "common/scssdk_telemetry_common_configs.h"
#include "common/scssdk_telemetry_common_gameplay_events.h"  // not included by the ATS header
#include "common/scssdk_telemetry_job_common_channels.h"
#include "common/scssdk_telemetry_truck_common_channels.h"
#include "eurotrucks2/scssdk_eut2.h"
#include "eurotrucks2/scssdk_telemetry_eut2.h"

#include "message.hpp"
#include "platform.hpp"
#include "sender.hpp"
#include "settings.hpp"
#include "spsc_ring.hpp"
#include "value_json.hpp"

namespace pt {
namespace {

struct Channel {
    const char* name;
    scs_value_type_t type;
    bool registered = false;
    bool available = false;  // last callback carried a value
    bool updated = false;    // a callback arrived during the current frame
    scs_value_t value{};
};

// Channels registered with each_frame | no_value so every physics frame reports
// either a value or "unavailable", which lets the client tell live data from
// held-over values. Types are the ones each header documents.
Channel make_channels_table(const char* name, scs_value_type_t type) { return Channel{name, type}; }

class Plugin {
public:
    scs_log_t log_fn = nullptr;
    std::string game_id;
    std::string game_name;
    scs_u32_t game_version = 0;
    std::string sess;
    Settings settings;

    SpscRing<Msg> ring{1u << 14};
    SpscRing<std::string> logs{256};
    SharedCounters counters;
    std::unique_ptr<Sender> sender;

    Channel channels[13] = {
        make_channels_table(SCS_TELEMETRY_TRUCK_CHANNEL_speed, SCS_VALUE_TYPE_float),
        make_channels_table(SCS_TELEMETRY_TRUCK_CHANNEL_odometer, SCS_VALUE_TYPE_float),
        make_channels_table(SCS_TELEMETRY_TRUCK_CHANNEL_fuel, SCS_VALUE_TYPE_float),
        make_channels_table(SCS_TELEMETRY_TRUCK_CHANNEL_world_placement, SCS_VALUE_TYPE_dplacement),
        make_channels_table(SCS_TELEMETRY_TRUCK_CHANNEL_local_linear_velocity, SCS_VALUE_TYPE_fvector),
        make_channels_table(SCS_TELEMETRY_CHANNEL_game_time, SCS_VALUE_TYPE_u32),
        make_channels_table(SCS_TELEMETRY_CHANNEL_local_scale, SCS_VALUE_TYPE_float),
        make_channels_table(SCS_TELEMETRY_CHANNEL_multiplayer_time_offset, SCS_VALUE_TYPE_s32),
        make_channels_table(SCS_TELEMETRY_JOB_CHANNEL_cargo_damage, SCS_VALUE_TYPE_float),
        make_channels_table(SCS_TELEMETRY_TRUCK_CHANNEL_navigation_distance, SCS_VALUE_TYPE_float),
        make_channels_table(SCS_TELEMETRY_TRUCK_CHANNEL_navigation_time, SCS_VALUE_TYPE_float),
        make_channels_table(SCS_TELEMETRY_TRUCK_CHANNEL_engine_enabled, SCS_VALUE_TYPE_bool),
        make_channels_table(SCS_TELEMETRY_TRUCK_CHANNEL_parking_brake, SCS_VALUE_TYPE_bool),
    };

    scs_telemetry_frame_start_t frame{};
    std::uint64_t frame_index = 0;
    bool paused = true;  // telemetry is paused when init is called (scssdk_telemetry.h:87)
    bool force_sample = true;
    bool have_sample = false;
    scs_timestamp_t last_sample_sim_us = 0;
    double next_sample_due_us = 0.0;  // paused-simulation time of the next scheduled sample
    std::int64_t last_emit_ms = 0;

    // Allocates a seq, lets `fill` add fields to the envelope, and pushes the
    // message. A full ring drops the message (counted); the game never waits.
    template <typename F>
    void emit(Kind kind, F&& fill, const char* config_id = nullptr) {
        Msg m;
        m.kind = kind;
        m.seq = counters.next_seq.fetch_add(1, std::memory_order_relaxed);
        if (config_id) m.config_id = config_id;
        JsonWriter w;
        envelope(w, kind, sess, m.seq);
        w.key("frame").u64(frame_index);
        w.key("sim_us").u64(frame.paused_simulation_time);
        fill(w);
        w.end_object();
        m.json = w.take();
        if (!ring.try_push(std::move(m))) counters.ring_dropped.fetch_add(1, std::memory_order_relaxed);
        last_emit_ms = steady_ms();
    }

    void flush_logs() {
        std::string line;
        while (logs.try_pop(line)) log_fn(SCS_LOG_TYPE_message, line.c_str());
    }

    void emit_sample() {
        emit(Kind::Sample, [this](JsonWriter& w) {
            w.key("simulation_us").u64(frame.simulation_time);
            w.key("render_us").u64(frame.render_time);
            w.key("ch").begin_object();
            for (const Channel& c : channels) {
                if (!c.registered) continue;
                w.key(c.name);
                if (c.available) write_value(w, c.value); else w.null();
            }
            w.end_object();
            // Available but not refreshed this frame: a held-over value.
            w.key("stale").begin_array();
            for (const Channel& c : channels) {
                if (c.registered && c.available && !c.updated) w.str(c.name);
            }
            w.end_array();
        });
        have_sample = true;
        force_sample = false;
        last_sample_sim_us = frame.paused_simulation_time;
    }
};

Plugin* g_plugin = nullptr;

// ---- callbacks (game main thread) --------------------------------------------

SCSAPI_VOID on_frame_start(const scs_event_t, const void* const info, const scs_context_t) {
    Plugin* p = g_plugin;
    if (!p || !info) return;
    try {
        p->frame = *static_cast<const scs_telemetry_frame_start_t*>(info);
        ++p->frame_index;
        for (Channel& c : p->channels) c.updated = false;
        if (p->frame.flags & SCS_TELEMETRY_FRAME_START_FLAG_timer_restart) {
            p->emit(Kind::TimerRestart, [p](JsonWriter& w) {
                w.key("simulation_us").u64(p->frame.simulation_time);
                w.key("render_us").u64(p->frame.render_time);
            });
            p->force_sample = true;
        }
        p->flush_logs();
    } catch (...) {
    }
}

SCSAPI_VOID on_frame_end(const scs_event_t, const void* const, const scs_context_t) {
    Plugin* p = g_plugin;
    if (!p) return;
    try {
        if (!p->paused) {
            const double rate = p->settings.capture ? 0.0 : p->settings.sample_rate_hz;
            const scs_timestamp_t now = p->frame.paused_simulation_time;
            // Fixed schedule with a small tolerance. The game's physics step is
            // 16666 us (observed in ATS 1.61), so three steps (49998 us) fall just
            // short of a 50 ms interval; comparing against the last sample time
            // would only fire every 4th step (15 Hz instead of 20 Hz).
            const double interval = rate > 0.0 ? 1e6 / rate : 0.0;
            const double tolerance = interval > 0.0 ? std::min(2000.0, interval / 4.0) : 0.0;
            const bool restarted = now < p->last_sample_sim_us;
            const bool scheduled = rate <= 0.0 || !p->have_sample || restarted ||
                                   double(now) + tolerance >= p->next_sample_due_us;
            if (p->force_sample || scheduled) {
                const bool had_sample = p->have_sample;
                p->emit_sample();
                if (scheduled) {
                    const bool on_time = had_sample && !restarted && double(now) < p->next_sample_due_us + interval;
                    p->next_sample_due_us = on_time ? p->next_sample_due_us + interval : double(now) + interval;
                }
            }
        }
        if (steady_ms() - p->last_emit_ms >= 1000) {
            p->emit(Kind::Heartbeat, [p](JsonWriter& w) {
                w.key("paused").boolean(p->paused);
                w.key("ring_dropped").u64(p->counters.ring_dropped.load(std::memory_order_relaxed));
            });
        }
    } catch (...) {
    }
}

SCSAPI_VOID on_pause_change(const scs_event_t event, const void* const, const scs_context_t) {
    Plugin* p = g_plugin;
    if (!p) return;
    try {
        p->paused = event == SCS_TELEMETRY_EVENT_paused;
        p->emit(p->paused ? Kind::Paused : Kind::Started, [](JsonWriter&) {});
        p->force_sample = true;
    } catch (...) {
    }
}

SCSAPI_VOID on_configuration(const scs_event_t, const void* const info, const scs_context_t) {
    Plugin* p = g_plugin;
    if (!p || !info) return;
    try {
        const auto* cfg = static_cast<const scs_telemetry_configuration_t*>(info);
        p->emit(Kind::Config, [cfg](JsonWriter& w) {
            w.key("id").str(cfg->id);
            w.key("attrs");
            write_attributes(w, cfg->attributes);
        }, cfg->id);
        p->force_sample = true;
    } catch (...) {
    }
}

SCSAPI_VOID on_gameplay(const scs_event_t, const void* const info, const scs_context_t) {
    Plugin* p = g_plugin;
    if (!p || !info) return;
    try {
        const auto* ev = static_cast<const scs_telemetry_gameplay_event_t*>(info);
        // Forwarded verbatim, including ids the SDK 1.14 headers do not document.
        p->emit(Kind::Gameplay, [ev](JsonWriter& w) {
            w.key("id").str(ev->id);
            w.key("attrs");
            write_attributes(w, ev->attributes);
        });
        p->force_sample = true;
    } catch (...) {
    }
}

SCSAPI_VOID on_channel(const scs_string_t, const scs_u32_t, const scs_value_t* const value, const scs_context_t ctx) {
    auto* c = static_cast<Channel*>(ctx);
    c->updated = true;
    c->available = value != nullptr;
    if (value) c->value = *value;  // registered types never carry pointers
}

std::string random_session_id() {
    static const char hex[] = "0123456789abcdef";
    std::string s;
    for (std::uint64_t v : {os_random_u64(), os_random_u64()}) {
        for (int shift = 60; shift >= 0; shift -= 4) s += hex[(v >> shift) & 0xf];
    }
    return s;
}

}  // namespace
}  // namespace pt

using namespace pt;

SCSAPI_RESULT scs_telemetry_init(const scs_u32_t version, const scs_telemetry_init_params_t* const params) {
    // API 1.01 is required: it added gameplay events and s64 values (scssdk_telemetry.h:23).
    if (version != SCS_TELEMETRY_VERSION_1_01) return SCS_RESULT_unsupported;
    if (g_plugin) return SCS_RESULT_generic_error;
    const auto* init = static_cast<const scs_telemetry_init_params_v101_t*>(params);

    try {
        auto plugin = std::make_unique<Plugin>();
        Plugin* p = plugin.get();
        p->log_fn = init->common.log;
        p->game_id = init->common.game_id;
        p->game_name = init->common.game_name;
        p->game_version = init->common.game_version;

        if (p->game_id != SCS_GAME_ID_EUT2 && p->game_id != SCS_GAME_ID_ATS) {
            p->log_fn(SCS_LOG_TYPE_warning, ("[plasmatrack] unsupported game id: " + p->game_id).c_str());
            return SCS_RESULT_unsupported;
        }

        const std::string module_path = own_module_path();
        const std::string plugin_dir = dir_of(module_path);
        p->settings = load_settings(join_path(plugin_dir, "plasmatrack.ini"));
        p->sess = random_session_id();

        // Events. gameplay needs game telemetry ETS2 >= 1.14 / ATS >= 1.01; a
        // failure is reported in the init message rather than being fatal.
        struct { scs_event_t ev; scs_telemetry_event_callback_t cb; const char* name; } events[] = {
            {SCS_TELEMETRY_EVENT_frame_start, on_frame_start, "frame_start"},
            {SCS_TELEMETRY_EVENT_frame_end, on_frame_end, "frame_end"},
            {SCS_TELEMETRY_EVENT_paused, on_pause_change, "paused"},
            {SCS_TELEMETRY_EVENT_started, on_pause_change, "started"},
            {SCS_TELEMETRY_EVENT_configuration, on_configuration, "configuration"},
            {SCS_TELEMETRY_EVENT_gameplay, on_gameplay, "gameplay"},
        };
        std::string events_failed;
        for (const auto& e : events) {
            if (init->register_for_event(e.ev, e.cb, nullptr) != SCS_RESULT_ok) {
                if (e.ev == SCS_TELEMETRY_EVENT_frame_start || e.ev == SCS_TELEMETRY_EVENT_frame_end ||
                    e.ev == SCS_TELEMETRY_EVENT_configuration) {
                    p->log_fn(SCS_LOG_TYPE_error, "[plasmatrack] required event registration failed");
                    return SCS_RESULT_generic_error;
                }
                events_failed += events_failed.empty() ? "" : ",";
                events_failed += e.name;
            }
        }
        for (Channel& c : p->channels) {
            c.registered = init->register_for_channel(c.name, SCS_U32_NIL, c.type,
                                                      SCS_TELEMETRY_CHANNEL_FLAG_each_frame |
                                                          SCS_TELEMETRY_CHANNEL_FLAG_no_value,
                                                      on_channel, &c) == SCS_RESULT_ok;
        }

        p->emit(Kind::Init, [&](JsonWriter& w) {
            w.key("api_version").str("1.01");
            w.key("game_id").str(p->game_id);
            w.key("game_name").str(p->game_name);
            w.key("game_version").begin_object();
            w.key("major").u64(p->game_version >> 16);
            w.key("minor").u64(p->game_version & 0xffff);
            w.end_object();
            w.key("plugin_version").str(kPluginVersion);
            w.key("platform").str(
#ifdef _WIN32
                "win_x64"
#else
                "linux_x64"
#endif
            );
            w.key("sample_rate_hz").f64(p->settings.capture ? 0.0 : p->settings.sample_rate_hz);
            w.key("capture").boolean(p->settings.capture);
            w.key("channels").begin_object();
            for (const Channel& c : p->channels) w.key(c.name).boolean(c.registered);
            w.end_object();
            w.key("events_failed").str(events_failed);
        });

        SenderConfig sc;
        sc.settings = p->settings;
        sc.sess = p->sess;
        sc.plugin_path = module_path;
        sc.data_dir = join_path(plugin_dir, "plasmatrack");
        p->sender = std::make_unique<Sender>(std::move(sc), p->ring, p->logs, p->counters);
        p->sender->start();

        p->log_fn(SCS_LOG_TYPE_message, ("[plasmatrack] telemetry initialized, session " + p->sess +
                                         ", client port " + std::to_string(p->settings.port)).c_str());
        g_plugin = plugin.release();
        return SCS_RESULT_ok;
    } catch (...) {
        return SCS_RESULT_generic_error;
    }
}

SCSAPI_VOID scs_telemetry_shutdown(void) {
    Plugin* p = g_plugin;
    if (!p) return;
    try {
        p->emit(Kind::Shutdown, [](JsonWriter&) {});
        // Joins without a timeout: the DLL may be unloaded right after we return.
        p->sender->stop_and_join();
        p->flush_logs();
    } catch (...) {
    }
    g_plugin = nullptr;
    delete p;
}
