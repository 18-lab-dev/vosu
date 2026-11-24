/// Transport wrapper for the upstream adapter's confined calculation service.
#[derive(Clone, Default)]
pub struct DeclaredCalculateProvider(pub vosu_adapter_upstream::files::Provider);
impl CalculatePpProvider for DeclaredCalculateProvider {
    fn calculate_pp(&self, query: PpQuery) -> ProviderFuture<'_, CalculateResult> {
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
