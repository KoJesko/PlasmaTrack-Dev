// JSON rendering of SDK values and attribute arrays (scssdk_value.h).
#pragma once

#include "json.hpp"
#include "scssdk_value.h"

namespace pt {

inline const char* value_type_name(scs_value_type_t t) {
    switch (t) {
        case SCS_VALUE_TYPE_bool: return "bool";
        case SCS_VALUE_TYPE_s32: return "s32";
        case SCS_VALUE_TYPE_u32: return "u32";
        case SCS_VALUE_TYPE_u64: return "u64";
        case SCS_VALUE_TYPE_float: return "float";
        case SCS_VALUE_TYPE_double: return "double";
        case SCS_VALUE_TYPE_fvector: return "fvector";
        case SCS_VALUE_TYPE_dvector: return "dvector";
        case SCS_VALUE_TYPE_euler: return "euler";
        case SCS_VALUE_TYPE_fplacement: return "fplacement";
        case SCS_VALUE_TYPE_dplacement: return "dplacement";
        case SCS_VALUE_TYPE_string: return "string";
        case SCS_VALUE_TYPE_s64: return "s64";
        default: return "unknown";
    }
}

template <typename V>
void write_vector(JsonWriter& w, const V& v) {
    w.begin_object();
    w.key("x"); if constexpr (sizeof(v.x) == 4) w.f32(v.x); else w.f64(v.x);
    w.key("y"); if constexpr (sizeof(v.y) == 4) w.f32(v.y); else w.f64(v.y);
    w.key("z"); if constexpr (sizeof(v.z) == 4) w.f32(v.z); else w.f64(v.z);
    w.end_object();
}

inline void write_euler(JsonWriter& w, const scs_value_euler_t& e) {
    w.begin_object();
    w.key("heading").f32(e.heading);
    w.key("pitch").f32(e.pitch);
    w.key("roll").f32(e.roll);
    w.end_object();
}

// Values keep the SDK's own units; conversions happen in pt-core.
inline void write_value(JsonWriter& w, const scs_value_t& v) {
    switch (v.type) {
        case SCS_VALUE_TYPE_bool: w.boolean(v.value_bool.value != 0); break;
        case SCS_VALUE_TYPE_s32: w.i64(v.value_s32.value); break;
        case SCS_VALUE_TYPE_u32: w.u64(v.value_u32.value); break;
        case SCS_VALUE_TYPE_u64: w.u64(v.value_u64.value); break;
        case SCS_VALUE_TYPE_s64: w.i64(v.value_s64.value); break;
        case SCS_VALUE_TYPE_float: w.f32(v.value_float.value); break;
        case SCS_VALUE_TYPE_double: w.f64(v.value_double.value); break;
        case SCS_VALUE_TYPE_fvector: write_vector(w, v.value_fvector); break;
        case SCS_VALUE_TYPE_dvector: write_vector(w, v.value_dvector); break;
        case SCS_VALUE_TYPE_euler: write_euler(w, v.value_euler); break;
        case SCS_VALUE_TYPE_fplacement:
            w.begin_object();
            w.key("position"); write_vector(w, v.value_fplacement.position);
            w.key("orientation"); write_euler(w, v.value_fplacement.orientation);
            w.end_object();
            break;
        case SCS_VALUE_TYPE_dplacement:
            w.begin_object();
            w.key("position"); write_vector(w, v.value_dplacement.position);
            w.key("orientation"); write_euler(w, v.value_dplacement.orientation);
            w.end_object();
            break;
        case SCS_VALUE_TYPE_string:
            if (v.value_string.value) w.str(v.value_string.value); else w.null();
            break;
        default: w.null(); break;
    }
}

// Renders a NULL-name-terminated attribute array (scssdk_telemetry_event.h:156-166)
// as [{"name":..,"index":null|n,"type":..,"value":..}, ...]. Strings are copied
// now because their pointers are only valid during the callback.
inline void write_attributes(JsonWriter& w, const scs_named_value_t* attrs) {
    w.begin_array();
    for (const scs_named_value_t* a = attrs; a && a->name; ++a) {
        w.begin_object();
        w.key("name").str(a->name);
        w.key("index");
        if (a->index == SCS_U32_NIL) w.null(); else w.u64(a->index);
        w.key("type").str(value_type_name(a->value.type));
        w.key("value");
        write_value(w, a->value);
        w.end_object();
    }
    w.end_array();
}

}  // namespace pt
