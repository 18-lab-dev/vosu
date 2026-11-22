use serde::{Deserialize, Deserializer, Serialize, Serializer};

macro_rules! numeric_enum {
    ($name:ident { $($variant:ident = $value:expr),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        #[repr(i32)]
        pub enum $name { $($variant = $value),+ }
        impl $name {
            pub const fn value(self) -> i32 { self as i32 }
            pub fn from_value(value: i32) -> Option<Self> {
                match value { $($value => Some(Self::$variant),)+ _ => None }
            }
        }
        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error> where S: Serializer {
                serializer.serialize_i32(*self as i32)
            }
        }
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error> where D: Deserializer<'de> {
                let value = i32::deserialize(deserializer)?;
                Self::from_value(value).ok_or_else(|| serde::de::Error::custom(format_args!("invalid {} value {value}", stringify!($name))))
            }
        }
    };
}
mod country;

pub use country::*;

numeric_enum! {
    GradeEnum {
    XH = 0,
    X = 1,
    SH = 2,
    S = 3,
    A = 4,
    B = 5,
    C = 6,
    D = 7,
    None = 8
    }
}

numeric_enum! {
    GameState {
    Menu = 0,
    Edit = 1,
    Play = 2,
    Exit = 3,
    SelectEdit = 4,
    SelectPlay = 5,
    SelectDrawings = 6,
    ResultScreen = 7,
    Update = 8,
    Busy = 9,
    Unknown = 10,
    Lobby = 11,
    MatchSetup = 12,
    SelectMulti = 13,
    RankingVs = 14,
    OnlineSelection = 15,
    OptionsOffsetWizard = 16,
    RankingTagCoop = 17,
    RankingTeam = 18,
    BeatmapImport = 19,
    PackageUpdater = 20,
    Benchmark = 21,
    Tourney = 22,
    Charts = 23
    }
}

numeric_enum! {
    LobbyStatus {
    NotJoined = 0,
    Idle = 1,
    PendingJoin = 2,
    PendingCreate = 3,
    Setup = 4,
    Play = 5,
    Results = 6
    }
}

numeric_enum! {
    StableBeatmapStatuses {
    Unknown = 0,
    NotSubmitted = 1,
    Pending = 2,
    Ranked = 4,
    Approved = 5,
    Qualified = 6,
    Loved = 7
    }
}

numeric_enum! {
    Rulesets {
    Osu = 0,
    Taiko = 1,
    Fruits = 2,
    Mania = 3
    }
}

numeric_enum! {
    BanchoStatus {
    Idle = 0,
    Afk = 1,
    Playing = 2,
    Editing = 3,
    Modding = 4,
    Multiplayer = 5,
    Watching = 6,
    Unknown = 7,
    Testing = 8,
    Submitting = 9,
    Paused = 10,
    Lobby = 11,
    Multiplaying = 12,
    OsuDirect = 13
    }
}

numeric_enum! {
    UserLoginStatus {
    Reconnecting = 0,
    Guest = 256,
    RecievingData = 257,
    Disconnected = 65537,
    Connected = 65793
    }
}

numeric_enum! {
    ReleaseStream {
    CuttingEdge = 0,
    Stable = 1,
    Beta = 2,
    Fallback = 3
    }
}

numeric_enum! {
    ScoreMeterType {
    None = 0,
    Colour = 1,
    Error = 2
    }
}

numeric_enum! {
    LeaderboardType {
    Local = 0,
    Global = 1,
    Selectedmods = 2,
    Friends = 3,
    Country = 4
    }
}

numeric_enum! {
    GroupType {
    None = 0,
    Artist = 1,
    BPM = 2,
    Creator = 3,
    Date = 4,
    Difficulty = 5,
    Length = 6,
    Rank = 7,
    MyMaps = 8,
    SearchShowAll = 12,
    Title = 13,
    LastPlayed = 14,
    OnlineFavourites = 15,
    ManiaKeys = 16,
    Mode = 17,
    Collection = 18,
    RankedStatus = 19
    }
}

numeric_enum! {
    SortType {
    Artist = 0,
    Bpm = 1,
    Creator = 2,
    Date = 3,
    Difficulty = 4,
    Length = 5,
    Rank = 6,
    Title = 7
    }
}

numeric_enum! {
    ChatStatus {
    Hidden = 0,
    Visible = 1,
    VisibleWithFriendsList = 2
    }
}

numeric_enum! {
    ProgressBarType {
    Off = 0,
    Pie = 1,
    TopRight = 2,
    BottomRight = 3,
    Bottom = 4
    }
}

numeric_enum! {
    ScoringMode {
    Standardised = 0,
    Classic = 1
    }
}

numeric_enum! {
    LazerHitResults {
    None = 0,
    Miss = 1,
    Meh = 2,
    Ok = 3,
    Good = 4,
    Great = 5,
    Perfect = 6,
    SmallTickMiss = 7,
    SmallTickHit = 8,
    LargeTickMiss = 9,
    LargeTickHit = 10,
    SmallBonus = 11,
    LargeBonus = 12,
    IgnoreMiss = 13,
    IgnoreHit = 14,
    ComboBreak = 15,
    SliderTailHit = 16,
    LegacyComboIncrease = 99
    }
}

numeric_enum! {
    LazerSettings {
    Ruleset = 0,
    Token = 1,
    MenuCursorSize = 2,
    GameplayCursorSize = 3,
    AutoCursorSize = 4,
    GameplayCursorDuringTouch = 5,
    DimLevel = 6,
    BlurLevel = 7,
    EditorDim = 8,
    LightenDuringBreaks = 9,
    ShowStoryboard = 10,
    KeyOverlay = 11,
    GameplayLeaderboard = 12,
    PositionalHitsoundsLevel = 13,
    AlwaysPlayFirstComboBreak = 14,
    FloatingComments = 15,
    HUDVisibilityMode = 16,
    ShowHealthDisplayWhenCantFail = 17,
    FadePlayfieldWhenHealthLow = 18,
    MouseDisableButtons = 19,
    MouseDisableWheel = 20,
    ConfineMouseMode = 21,
    AudioOffset = 22,
    VolumeInactive = 23,
    MenuMusic = 24,
    MenuVoice = 25,
    MenuTips = 26,
    CursorRotation = 27,
    MenuParallax = 28,
    Prefer24HourTime = 29,
    BeatmapDetailTab = 30,
    BeatmapLeaderboardSortMode = 31,
    BeatmapDetailModsFilter = 32,
    Username = 33,
    ReleaseStream = 34,
    SavePassword = 35,
    SaveUsername = 36,
    DisplayStarsMinimum = 37,
    DisplayStarsMaximum = 38,
    SongSelectGroupMode = 39,
    SongSelectSortingMode = 40,
    RandomSelectAlgorithm = 41,
    ModSelectHotkeyStyle = 42,
    ShowFpsDisplay = 43,
    ChatDisplayHeight = 44,
    BeatmapListingCardSize = 45,
    ToolbarClockDisplayMode = 46,
    SongSelectBackgroundBlur = 47,
    Version = 48,
    ShowFirstRunSetup = 49,
    ShowConvertedBeatmaps = 50,
    Skin = 51,
    ScreenshotFormat = 52,
    ScreenshotCaptureMenuCursor = 53,
    BeatmapSkins = 54,
    BeatmapColours = 55,
    BeatmapHitsounds = 56,
    IncreaseFirstObjectVisibility = 57,
    ScoreDisplayMode = 58,
    ExternalLinkWarning = 59,
    PreferNoVideo = 60,
    Scaling = 61,
    ScalingPositionX = 62,
    ScalingPositionY = 63,
    ScalingSizeX = 64,
    ScalingSizeY = 65,
    ScalingBackgroundDim = 66,
    UIScale = 67,
    IntroSequence = 68,
    NotifyOnUsernameMentioned = 69,
    NotifyOnPrivateMessage = 70,
    NotifyOnFriendPresenceChange = 71,
    UIHoldActivationDelay = 72,
    HitLighting = 73,
    StarFountains = 74,
    MenuBackgroundSource = 75,
    GameplayDisableWinKey = 76,
    SeasonalBackgroundMode = 77,
    EditorWaveformOpacity = 78,
    EditorShowHitMarkers = 79,
    EditorAutoSeekOnPlacement = 80,
    DiscordRichPresence = 81,
    ShowOnlineExplicitContent = 82,
    LastProcessedMetadataId = 83,
    SafeAreaConsiderations = 84,
    ComboColourNormalisationAmount = 85,
    ProfileCoverExpanded = 86,
    EditorLimitedDistanceSnap = 87,
    ReplaySettingsOverlay = 88,
    ReplayPlaybackControlsExpanded = 89,
    AutomaticallyDownloadMissingBeatmaps = 90,
    EditorShowSpeedChanges = 91,
    TouchDisableGameplayTaps = 92,
    ModSelectTextSearchStartsActive = 93,
    UserOnlineStatus = 94,
    MultiplayerRoomFilter = 95,
    HideCountryFlags = 96,
    EditorTimelineShowTimingChanges = 97,
    EditorTimelineShowTicks = 98,
    AlwaysShowHoldForMenuButton = 99,
    EditorContractSidebars = 100,
    EditorScaleOrigin = 101,
    EditorRotationOrigin = 102,
    EditorTimelineShowBreaks = 103,
    EditorAdjustExistingObjectsOnTimingChanges = 104,
    AlwaysRequireHoldingForPause = 105,
    MultiplayerShowInProgressFilter = 106,
    BeatmapListingFeaturedArtistFilter = 107,
    ShowMobileDisclaimer = 108,
    EditorShowStoryboard = 109,
    EditorSubmissionNotifyOnDiscussionReplies = 110,
    EditorSubmissionLoadInBrowserAfterSubmission = 111,
    WasSupporter = 112,
    LastOnlineTagsPopulation = 113,
    AutomaticallyAdjustBeatmapOffset = 114
    }
}

numeric_enum! {
    FrameworkSetting {
    ShowLogOverlay = 0,
    AudioDevice = 1,
    AudioUseExperimentalWasapi = 2,
    VolumeUniversal = 3,
    VolumeEffect = 4,
    VolumeMusic = 5,
    HardwareVideoDecoder = 6,
    WindowedSize = 7,
    WindowedPositionX = 8,
    WindowedPositionY = 9,
    LastDisplayDevice = 10,
    SizeFullscreen = 11,
    MinimiseOnFocusLossInFullscreen = 12,
    Renderer = 13,
    WindowMode = 14,
    ConfineMouseMode = 15,
    FrameSync = 16,
    ExecutionMode = 17,
    ShowUnicode = 18,
    Locale = 19,
    IgnoredInputHandlers = 20,
    CursorSensitivity = 21,
    MapAbsoluteInputToWindow = 22
    }
}

numeric_enum! {
    LazerBeatmapTabType {
    Details = 0,
    Local = 1,
    Country = 2,
    Global = 3,
    Friends = 4,
    Team = 5
    }
}

numeric_enum! {
    LazerGroupMode {
    None = 0,
    Artist = 1,
    Author = 2,
    Bpm = 3,
    Collections = 4,
    DateAdded = 5,
    DateRanked = 6,
    Difficulty = 7,
    Favourites = 8,
    LastPlayed = 9,
    Length = 10,
    MyMaps = 11,
    RankAchieved = 12,
    RankedStatus = 13,
    Source = 14,
    Title = 15
    }
}

numeric_enum! {
    LazerSortMode {
    Artist = 0,
    Author = 1,
    Bpm = 2,
    Dateadded = 3,
    Dateranked = 4,
    Datesubmitted = 5,
    Difficulty = 6,
    Lastplayed = 7,
    Length = 8,
    Source = 9,
    Title = 10
    }
}

numeric_enum! {
    LazerManiaSetting {
    ScrollSpeed = 0,
    ScrollDirection = 1,
    TimingBasedNoteColouring = 2,
    MobileLayout = 3
    }
}

numeric_enum! {
    LazerOsuSetting {
    SnakingInSliders = 0,
    SnakingOutSliders = 1,
    ShowCursorTrail = 2,
    ShowCursorRipples = 3,
    PlayfieldBorderStyle = 4,
    ReplayClickMarkersEnabled = 5,
    ReplayFrameMarkersEnabled = 6,
    ReplayCursorPathEnabled = 7,
    ReplayCursorHideEnabled = 8,
    ReplayAnalysisDisplayLength = 9
    }
}

numeric_enum! {
    LazerTaikoSetting {
    TouchControlScheme = 0
    }
}

numeric_enum! {
    ManiaScrollingDirection {
    Up = 0,
    Down = 1
    }
}

numeric_enum! {
    RankedPlayStage {
    WaitForJoin = 0,
    RoundWarmup = 1,
    CardDiscard = 2,
    FinishCardDiscard = 3,
    CardPlay = 4,
    FinishCardPlay = 5,
    GameplayWarmup = 6,
    Gameplay = 7,
    Results = 8,
    Ended = 9
    }
}

numeric_enum! {
    ClientType {
    Stable = 0,
    Lazer = 1
    }
}

numeric_enum! {
    Bitness {
    X86 = 32,
    X64 = 64
    }
}

pub struct LogColor;
impl LogColor {
    pub const INFO: &'static str = "\x1b[38;2;80;250;123m";
    pub const WARN: &'static str = "\x1b[38;2;255;184;108m";
    pub const ERROR: &'static str = "\x1b[38;2;255;85;85m";
    pub const DEBUG_ERROR: &'static str = "\x1b[38;2;189;147;249m";
    pub const DEBUG: &'static str = "\x1b[38;2;120;120;120m";
    pub const TIME: &'static str = "\x1b[38;2;80;80;80m";
    pub const GREY: &'static str = "\x1b[38;2;170;170;170m";
    pub const GREEN_TEXT: &'static str = "\x1b[38;2;60;200;100m";
    pub const RESET: &'static str = "\x1b[0m";
}

pub struct LogSymbol;
impl LogSymbol {
    pub const SEPARATOR: &'static str = "┃";
    pub const PROGRESS_BAR_FILLED: &'static str = "━";
    pub const PROGRESS_BAR_EMPTY: &'static str = "─";
}
