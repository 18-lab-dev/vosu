#[derive(Copy, Clone)]
#[expect(unused, reason = "staying in-sync with lazer")]
pub enum HitResult {
    None,
    Miss,
    Meh,
    Ok,
    Good,
    Great,
    Perfect,
    SmallTickMiss,
    SmallTickHit,
    LargeTickMiss,
    LargeTickHit,
    SmallBonus,
    LargeBonus,
    IgnoreMiss,
    IgnoreHit,
    ComboBreak,
    SliderTailHit,
    LegacyComboIncrease,
}

impl HitResult {
    pub const fn osu_base_score(self) -> i32 {
        match self {
            Self::SmallTickHit => 10,
            Self::LargeTickHit => 30,
            Self::SliderTailHit => 150,
            Self::Meh => 50,
            Self::Ok => 100,
            Self::Good => 200,
            // Perfect does not directly grant more score or accuracy.
            Self::Great | Self::Perfect => 300,
            Self::SmallBonus => 10,
            Self::LargeBonus => 50,
            _ => 0,
        }
    }
}
