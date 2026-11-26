/// Config facade for `/api/settingsSave` and `GET /api/settings`.
pub trait ConfigUpdateService: Send + Sync {
    fn save(&self, values: Value) -> ProviderFuture<'_, Result<(), String>>;
    /// `SettingsView` (A7.1): the Settings page's values, never
    /// `ENABLE_DASHBOARD`, a secret or a credential.
    fn view(&self) -> Value;
}

/// Runtime report input facade; report rendering remains in `report.rs`.
pub trait ReportInputProvider: Send + Sync {
    fn capture(&self) -> Result<ReportInput, String>;
    /// Read-only dashboard metadata; no machine probing or log collection.
    fn dashboard_status(&self) -> Value {
        json!({ "instances": [], "available": false })
    }
}

pub struct DeferredUpdateTrigger(Option<Box<dyn FnOnce() + Send + 'static>>);

impl DeferredUpdateTrigger {
    pub fn new(trigger: impl FnOnce() + Send + 'static) -> Self {
        Self(Some(Box::new(trigger)))
    }

    fn fire(mut self) {
        if let Some(trigger) = self.0.take() {
            trigger();
        }
    }
}

pub enum UpdateRouteResult {
    Latest,
    NoFiles,
    Updated(DeferredUpdateTrigger),
    /// A newer stable release exists whose major version is greater than the
    /// running build's; the caller did not supply consent, so nothing was
    /// installed.
    MajorAvailable { version: String, release_url: String },
}

/// Single-flight update operation. Implementations prepare and spawn the helper
/// but defer daemon shutdown through `DeferredUpdateTrigger` until the terminal
/// response body has been consumed (or dropped on disconnect).
pub trait UpdaterHandoff: Send + Sync {
    /// `major_consent` must be `true` for the caller to accept installing a
    /// release whose major version exceeds the running build's; otherwise
    /// such a release is reported as `UpdateRouteResult::MajorAvailable`
    /// instead of being installed.
    fn run_update(
        &self,
        major_consent: bool,
    ) -> ProviderFuture<'_, Result<UpdateRouteResult, String>>;
}

