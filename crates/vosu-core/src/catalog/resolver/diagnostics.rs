#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequirementSite {
    pub consumer: String,
    pub capability: String,
    pub range: String,
}
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Failure {
    #[error("resolution-limit")]
    ResolutionLimit,
    #[error("invalid resolver input")]
    InvalidInput,
    #[error("requested item unavailable")]
    RequestedUnavailable,
    #[error("unsupported host: {0}")]
    Host(String),
    #[error("unsupported target: {0}")]
    Target(String),
    #[error("package exceeds local limit: {0}")]
    Size(String),
    #[error("untrusted in-process runtime: {0}")]
    Runtime(String),
    #[error("missing provider: {0:?}")]
    Missing(RequirementSite),
    #[error("conflicting requirements: {0:?}")]
    Conflict(Vec<RequirementSite>),
    #[error("capability cycle: {0:?}")]
    Cycle(Vec<RequirementSite>),
}
