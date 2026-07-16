pub(super) fn game_state_name(v: i64) -> &'static str {
    match v {
        0 => "menu",
        1 => "edit",
        2 => "play",
        3 => "exit",
        4 => "selectEdit",
        5 => "selectPlay",
        6 => "selectDrawings",
        7 => "resultScreen",
        8 => "update",
        9 => "busy",
        10 => "unknown",
        11 => "lobby",
        12 => "matchSetup",
        13 => "selectMulti",
        14 => "rankingVs",
        15 => "onlineSelection",
        16 => "optionsOffsetWizard",
        17 => "rankingTagCoop",
        18 => "rankingTeam",
        19 => "beatmapImport",
        20 => "packageUpdater",
        21 => "benchmark",
        22 => "tourney",
        23 => "charts",
        _ => "",
    }
}
pub(super) fn ruleset_name(v: i64) -> &'static str {
    match v {
        0 => "osu",
        1 => "taiko",
        2 => "fruits",
        3 => "mania",
        _ => "",
    }
}
pub(super) fn chat_status_name(v: i64) -> &'static str {
    match v {
        0 => "hidden",
        1 => "visible",
        2 => "visibleWithFriendsList",
        _ => "",
    }
}
pub(super) fn leaderboard_name(v: i64) -> &'static str {
    match v {
        0 => "local",
        1 => "global",
        2 => "selectedmods",
        3 => "friends",
        4 => "country",
        _ => "",
    }
}
pub(super) fn lazer_leaderboard_name(v: i64) -> &'static str {
    match v {
        0 => "details",
        1 => "local",
        2 => "country",
        3 => "global",
        4 => "friends",
        5 => "team",
        _ => "",
    }
}
pub(super) fn progress_bar_name(v: i64) -> &'static str {
    match v {
        0 => "off",
        1 => "pie",
        2 => "topRight",
        3 => "bottomRight",
        4 => "bottom",
        _ => "",
    }
}
pub(super) fn score_meter_name(v: i64) -> &'static str {
    match v {
        0 => "none",
        1 => "colour",
        2 => "error",
        _ => "",
    }
}
pub(super) fn mania_scroll_name(v: i64) -> &'static str {
    match v {
        0 => "up",
        1 => "down",
        _ => "",
    }
}
pub(super) fn sort_name(v: i64) -> &'static str {
    match v {
        0 => "artist",
        1 => "bpm",
        2 => "creator",
        3 => "date",
        4 => "difficulty",
        5 => "length",
        6 => "rank",
        7 => "title",
        _ => "",
    }
}
pub(super) fn lazer_sort_name(v: i64) -> &'static str {
    match v {
        0 => "artist",
        1 => "author",
        2 => "bpm",
        3 => "dateadded",
        4 => "dateranked",
        5 => "datesubmitted",
        6 => "difficulty",
        7 => "lastplayed",
        8 => "length",
        9 => "source",
        10 => "title",
        _ => "",
    }
}
pub(super) fn group_name(v: i64) -> &'static str {
    match v {
        0 => "none",
        1 => "artist",
        2 => "bPM",
        3 => "creator",
        4 => "date",
        5 => "difficulty",
        6 => "length",
        7 => "rank",
        8 => "myMaps",
        12 => "show_All",
        13 => "title",
        14 => "lastPlayed",
        15 => "onlineFavourites",
        16 => "maniaKeys",
        17 => "mode",
        18 => "collection",
        19 => "rankedStatus",
        _ => "",
    }
}
pub(super) fn lazer_group_name(v: i64) -> &'static str {
    match v {
        0 => "none",
        1 => "artist",
        2 => "author",
        3 => "bpm",
        4 => "collections",
        5 => "dateAdded",
        6 => "dateRanked",
        7 => "difficulty",
        8 => "favourites",
        9 => "lastPlayed",
        10 => "length",
        11 => "myMaps",
        12 => "rankAchieved",
        13 => "rankedStatus",
        14 => "source",
        15 => "title",
        _ => "",
    }
}
pub(super) fn user_login_name(v: i64) -> &'static str {
    match v {
        0 => "reconnecting",
        256 => "guest",
        257 => "recieving_data",
        65537 => "disconnected",
        65793 => "connected",
        _ => "",
    }
}
pub(super) fn bancho_name(v: i64) -> &'static str {
    match v {
        0 => "idle",
        1 => "afk",
        2 => "playing",
        3 => "editing",
        4 => "modding",
        5 => "multiplayer",
        6 => "watching",
        7 => "unknown",
        8 => "testing",
        9 => "submitting",
        10 => "paused",
        11 => "lobby",
        12 => "multiplaying",
        13 => "osuDirect",
        _ => "",
    }
}
pub(super) fn beatmap_status_name(v: i64) -> &'static str {
    match v {
        0 => "unknown",
        1 => "notSubmitted",
        2 => "pending",
        4 => "ranked",
        5 => "approved",
        6 => "qualified",
        7 => "loved",
        _ => "",
    }
}
pub(super) fn country_name(v: i64) -> &'static str {
    const CODES: &[&str] = &[
        "", "OC", "EU", "AD", "AE", "AF", "AG", "AI", "AL", "AM", "AN", "AO", "AQ", "AR", "AS",
        "AT", "AU", "AW", "AZ", "BA", "BB", "BD", "BE", "BF", "BG", "BH", "BI", "BJ", "BM", "BN",
        "BO", "BR", "BS", "BT", "BV", "BW", "BY", "BZ", "CA", "CC", "CD", "CF", "CG", "CH", "CI",
        "CK", "CL", "CM", "CN", "CO", "CR", "CU", "CV", "CX", "CY", "CZ", "DE", "DJ", "DK", "DM",
        "DO", "DZ", "EC", "EE", "EG", "EH", "ER", "ES", "ET", "FI", "FJ", "FK", "FM", "FO", "FR",
        "FX", "GA", "GB", "GD", "GE", "GF", "GH", "GI", "GL", "GM", "GN", "GP", "GQ", "GR", "GS",
        "GT", "GU", "GW", "GY", "HK", "HM", "HN", "HR", "HT", "HU", "ID", "IE", "IL", "IN", "IO",
        "IQ", "IR", "IS", "IT", "JM", "JO", "JP", "KE", "KG", "KH", "KI", "KM", "KN", "KP", "KR",
        "KW", "KY", "KZ", "LA", "LB", "LC", "LI", "LK", "LR", "LS", "LT", "LU", "LV", "LY", "MA",
        "MC", "MD", "MG", "MH", "MK", "ML", "MM", "MN", "MO", "MP", "MQ", "MR", "MS", "MT", "MU",
        "MV", "MW", "MX", "MY", "MZ", "NA", "NC", "NE", "NF", "NG", "NI", "NL", "NO", "NP", "NR",
        "NU", "NZ", "OM", "PA", "PE", "PF", "PG", "PH", "PK", "PL", "PM", "PN", "PR", "PS", "PT",
        "PW", "PY", "QA", "RE", "RO", "RU", "RW", "SA", "SB", "SC", "SD", "SE", "SG", "SH", "SI",
        "SJ", "SK", "SL", "SM", "SN", "SO", "SR", "ST", "SV", "SY", "SZ", "TC", "TD", "TF", "TG",
        "TH", "TJ", "TK", "TM", "TN", "TO", "TP", "TR", "TT", "TV", "TW", "TZ", "UA", "UG", "UM",
        "US", "UY", "UZ", "VA", "VC", "VE", "VG", "VI", "VN", "VU", "WF", "WS", "YE", "YT", "YU",
        "ZA", "ZM", "ZR", "ZW", "XX",
    ];
    usize::try_from(v)
        .ok()
        .and_then(|i| CODES.get(i))
        .copied()
        .unwrap_or("")
}

pub(super) fn source_country_name(v: i64) -> &'static str {
    match v {
        216 => "TL",
        239 => "RS",
        242 => "ME",
        245 => "A2",
        246 => "O1",
        247 => "AX",
        248 => "GG",
        249 => "IM",
        250 => "JE",
        251 => "BL",
        252 => "MF",
        _ => country_name(v),
    }
}
