/// I0 replaces the old calculation provider with this requester-backed provider.
#[derive(Clone, Default)]
pub struct DeclaredCalculateProvider(pub vosu_adapter_upstream::files::Provider);
impl CalculatePpProvider for DeclaredCalculateProvider {
    fn calculate_pp(&self, query: PpQuery) -> ProviderFuture<'_, CalculateResult> {
        let query = vosu_adapter_upstream::calculate::PpQuery {
            path: query.path,
            lazer: query.lazer,
            mode: query.mode,
            ar: query.ar,
            cs: query.cs,
            hp: query.hp,
            od: query.od,
            clock_rate: query.clock_rate,
            passed_objects: query.passed_objects,
            combo: query.combo,
            n_misses: query.n_misses,
            n100: query.n100,
            n300: query.n300,
            n50: query.n50,
            n_geki: query.n_geki,
            n_katu: query.n_katu,
            mods: query.mods.map(|mods| match mods { super::routes::ModsInput::Bits(v) => vosu_adapter_upstream::calculate::ModsInput::Bits(v), super::routes::ModsInput::Json(v) => vosu_adapter_upstream::calculate::ModsInput::Json(v), super::routes::ModsInput::Acronyms(v) => vosu_adapter_upstream::calculate::ModsInput::Acronyms(v) }),
            accuracy: query.accuracy,
            slider_end_hits: query.slider_end_hits,
            small_tick_hits: query.small_tick_hits,
            large_tick_hits: query.large_tick_hits,
            hitresult_priority: query.hitresult_priority,
        };
        Box::pin(async move {
            match self.0.calculate(query).await {
                vosu_adapter_upstream::calculate::ResultBody::Ready(body) => CalculateResult::Ready(body),
                vosu_adapter_upstream::calculate::ResultBody::NotReady => CalculateResult::NotReady,
                vosu_adapter_upstream::calculate::ResultBody::Failed { status, body } => CalculateResult::Failed {
                    status: StatusCode::from_u16(status).expect("adapter returns valid HTTP status"), body,
                },
            }
        })
    }
}
