//! Unit conversions and threshold comparisons.
//!
//! The conversion factors are fixed by the PlasmaTrack spec and are not
//! configurable. SDK units: `truck.odometer` is km
//! (common/scssdk_telemetry_truck_common_channels.h:640-645), `truck.speed` is m/s,
//! negative when reversing (same header, :107-114).

/// Spec: 1 km = 0.621371 mi.
pub const MI_PER_KM: f64 = 0.621371;

/// Spec: speed in mph = m/s x 2.23694.
pub const MPH_PER_MPS: f64 = 2.23694;

pub fn km_to_mi(km: f64) -> f64 {
    km * MI_PER_KM
}

pub fn mps_to_mph(mps: f64) -> f64 {
    mps * MPH_PER_MPS
}

/// Quantizes `value` to a whole number of `precision` steps.
///
/// Comparing quantized integers instead of raw floats makes the spec's boundary
/// cases deterministic: a speed that converts to 104.9999999 or 105.0000001 mph
/// is "exactly 105.0" at 0.001 mph precision, so it is not "over 105".
fn quantize(value: f64, precision: f64) -> i64 {
    debug_assert!(precision > 0.0, "precision must be positive");
    (value / precision).round() as i64
}

/// True if `value` is strictly greater than `threshold` at the given precision.
pub fn exceeds(value: f64, threshold: f64, precision: f64) -> bool {
    quantize(value, precision) > quantize(threshold, precision)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversions_use_spec_factors() {
        assert_eq!(km_to_mi(1.0), 0.621371);
        assert_eq!(mps_to_mph(1.0), 2.23694);
        assert!((km_to_mi(100.0) - 62.1371).abs() < 1e-9);
    }

    #[test]
    fn exactly_at_threshold_is_not_over() {
        let p = 0.001;
        for limit in [100.0, 105.0, 125.0, 150.0] {
            // The m/s value a game would report for exactly `limit` mph (f32 channel).
            let mps = (limit / MPH_PER_MPS) as f32;
            let mph = mps_to_mph(mps as f64);
            assert!(!exceeds(mph, limit, p), "{limit} mph reported as {mph}");
            assert!(exceeds(limit + 0.001, limit, p));
        }
    }

    #[test]
    fn manual_review_boundary() {
        let p = 0.001;
        assert!(!exceeds(3000.0, 3000.0, p));
        assert!(exceeds(3000.1, 3000.0, p));
        // 3000 mi driven in ETS2, measured in km.
        let km = 3000.0 / MI_PER_KM;
        assert!(!exceeds(km_to_mi(km), 3000.0, p));
    }
}
