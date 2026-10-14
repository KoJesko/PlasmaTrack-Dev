//! Game identity. ATS and ETS2 are kept separate everywhere: jobs, totals,
//! leaderboards, maps and timeline state are all partitioned by [`Game`].

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Game {
    Ets2,
    Ats,
}

impl Game {
    /// Maps the SDK's `scs_sdk_init_params_v100_t::game_id` (scssdk.h:156-169).
    ///
    /// Values: `SCS_GAME_ID_EUT2` = "eut2" (eurotrucks2/scssdk_eut2.h:16),
    /// `SCS_GAME_ID_ATS` = "ats" (amtrucks/scssdk_ats.h:16).
    pub fn from_sdk_game_id(game_id: &str) -> Option<Game> {
        match game_id {
            "eut2" => Some(Game::Ets2),
            "ats" => Some(Game::Ats),
            _ => None,
        }
    }

    /// The API name (`"ets2"` / `"ats"`), the same as the serde form.
    pub fn as_str(self) -> &'static str {
        match self {
            Game::Ets2 => "ets2",
            Game::Ats => "ats",
        }
    }

    pub fn from_api(s: &str) -> Option<Game> {
        match s {
            "ets2" => Some(Game::Ets2),
            "ats" => Some(Game::Ats),
            _ => None,
        }
    }

    pub fn sdk_game_id(self) -> &'static str {
        match self {
            Game::Ets2 => "eut2",
            Game::Ats => "ats",
        }
    }

    /// Minimum telemetry game version (major, minor) we support: the version
    /// that added `planned_distance.km`, both at game patch 1.36.
    /// ETS2: SCS_TELEMETRY_EUT2_GAME_VERSION_1_15 (eurotrucks2/scssdk_telemetry_eut2.h:53,74).
    /// ATS:  SCS_TELEMETRY_ATS_GAME_VERSION_1_02 (amtrucks/scssdk_telemetry_ats.h:32,40).
    /// Gameplay events (job.delivered etc.) arrived one version earlier.
    pub fn min_telemetry_game_version(self) -> (u16, u16) {
        match self {
            Game::Ets2 => (1, 15),
            Game::Ats => (1, 2),
        }
    }

    /// `game_version` is packed with SCS_MAKE_VERSION (major << 16 | minor), scssdk.h:124.
    pub fn is_supported_version(self, packed_game_version: u32) -> bool {
        let major = (packed_game_version >> 16) as u16;
        let minor = (packed_game_version & 0xffff) as u16;
        (major, minor) >= self.min_telemetry_game_version()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_sdk_game_ids() {
        assert_eq!(Game::from_sdk_game_id("eut2"), Some(Game::Ets2));
        assert_eq!(Game::from_sdk_game_id("ats"), Some(Game::Ats));
        assert_eq!(Game::from_sdk_game_id("ets2"), None);
        for g in [Game::Ets2, Game::Ats] {
            assert_eq!(Game::from_sdk_game_id(g.sdk_game_id()), Some(g));
        }
    }

    #[test]
    fn minimum_version_gate() {
        let v = |maj: u32, min: u32| (maj << 16) | min;
        assert!(!Game::Ets2.is_supported_version(v(1, 14)));
        assert!(Game::Ets2.is_supported_version(v(1, 15)));
        assert!(Game::Ets2.is_supported_version(v(1, 18)));
        assert!(!Game::Ats.is_supported_version(v(1, 1)));
        assert!(Game::Ats.is_supported_version(v(1, 2)));
    }
}
