// TruckersMP client SDK entry points. Uses only the SDK's C layer
// (TruckersMP_Base.h, TruckersMP.h); every call mirrors what the SDK's own C++
// wrapper does in Session::Create (TruckersMP.hxx:1892-1920).
//
// Threading: the TruckersMP client calls init/shutdown and delivers events on
// the game thread (docs: concepts/threading). State is stored in atomics that
// the telemetry callbacks (also the game thread) read.

#include "tmp.hpp"

#include <cstring>

#include "TruckersMP/TruckersMP.h"
#include "message.hpp"

namespace pt::tmp {
namespace {

State g_state;
TruckersMP_PluginContext* g_ctx = nullptr;
const TruckersMP_Core* g_core = nullptr;
const TruckersMP_Network* g_net = nullptr;
const TruckersMP_Accounts* g_acc = nullptr;

void refresh() {
    bool connected = false;
    std::uint64_t server = 0;
    std::uint64_t account = 0;
    if (g_net && g_net->IsConnected) {
        bool c = false;
        if (g_net->IsConnected(g_ctx, &c)) connected = c;
        if (connected && g_net->GetGameServerID) g_net->GetGameServerID(g_ctx, &server);
    }
    if (g_acc && g_acc->GetAccountID) g_acc->GetAccountID(g_ctx, &account);
    g_state.connected.store(connected);
    g_state.server_id.store(server);
    g_state.account_id.store(account);
    g_state.version.fetch_add(1);
}

void TMP_API on_network(void*, std::uint64_t, void*, std::uint32_t) {
    refresh();
}

void copy(char* dst, std::size_t n, const char* src) {
    std::size_t len = std::strlen(src);
    if (len >= n) len = n - 1;
    std::memcpy(dst, src, len);
    dst[len] = '\0';
}

void clear() {
    g_ctx = nullptr;
    g_core = nullptr;
    g_net = nullptr;
    g_acc = nullptr;
}

}  // namespace

State& state() { return g_state; }

}  // namespace pt::tmp

using namespace pt::tmp;

TMP_EXPORT bool TMP_API truckersmp_init(const TruckersMP_Host* host, TruckersMP_PluginDesc* desc) {
    try {
        if (!host || host->m_size < sizeof(TruckersMP_Host) || !host->GetApi || !host->DeclareVersion) return false;
        if (host->DeclareVersion(host->m_ctx, TRUCKERSMP_SDK_VERSION) != TruckersMP_Result_Ok) return false;
        g_ctx = host->m_ctx;
        g_core = static_cast<const TruckersMP_Core*>(host->GetApi(g_ctx, TRUCKERSMP_API_CORE));
        if (!g_core) {
            clear();
            return false;
        }
        g_net = static_cast<const TruckersMP_Network*>(host->GetApi(g_ctx, TRUCKERSMP_API_NETWORK));
        g_acc = static_cast<const TruckersMP_Accounts*>(host->GetApi(g_ctx, TRUCKERSMP_API_ACCOUNTS));
        if (desc) {
            copy(desc->m_name, sizeof desc->m_name, "PlasmaTrack");
            copy(desc->m_author, sizeof desc->m_author, "PlasmaTrack");
            copy(desc->m_version, sizeof desc->m_version, pt::kPluginVersion);
            copy(desc->m_description, sizeof desc->m_description,
                 "Job tracker. Reads whether you are connected to a TruckersMP server, which server, "
                 "and your TruckersMP account ID.");
        }
        if (g_core->AddListener) {
            g_core->AddListener(g_ctx, TRUCKERSMP_EVT_NETWORK_CONNECTED, on_network, nullptr);
            g_core->AddListener(g_ctx, TRUCKERSMP_EVT_NETWORK_DISCONNECTED, on_network, nullptr);
        }
        g_state.loaded.store(true);
        refresh();  // a load in the middle of a server session sees the current state
        return true;
    } catch (...) {
        clear();
        return false;
    }
}

TMP_EXPORT void TMP_API truckersmp_shutdown(void) {
    try {
        if (g_core && g_core->RemoveListener) {
            g_core->RemoveListener(g_ctx, TRUCKERSMP_EVT_NETWORK_CONNECTED, on_network, nullptr);
            g_core->RemoveListener(g_ctx, TRUCKERSMP_EVT_NETWORK_DISCONNECTED, on_network, nullptr);
        }
    } catch (...) {
    }
    clear();
    g_state.loaded.store(false);
    g_state.connected.store(false);
    g_state.server_id.store(0);
    g_state.version.fetch_add(1);
}
