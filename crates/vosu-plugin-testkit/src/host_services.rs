//! A host-services testkit plugin (`docs/v2/implementation-plan.md` M1
//! Exit "log rate limiting, timer cancellation"): on `create`, requests a
//! one-shot timer (§6.11); once `VOSU_OP_TIMER_FIRED` reaches it, it
//! cancels that timer (§6.11 documents cancelling an already-fired
//! one-shot as a no-op, but exercising the call still proves the wire
//! path) and sends a burst of `LOG_RECORD`s comfortably past the
//! per-instance rate limit (§6.10), so a `PluginHost`-driven test can
//! observe the whole M1 host-services surface — settings (`init`),
//! timers, and rate-limited logging — through one real plugin instead of
//! a bare echo.

use vosu_plugin_api::{
    CborValue, HostServices, Init, LogLevel, LogRecord, Message, Plugin, PluginError, TimerCancel,
    TimerFired, TimerSet, VtableHostServices, vosu_status,
};

/// Comfortably past `vosu_core::plugin::logging::LOG_RATE_LIMIT_PER_SECOND`
/// (200/s): enough to force the host to drop some of the burst within one
/// rate-limit window.
pub const LOG_BURST: u32 = 220;

/// The one-shot timer id this plugin sets at `create`.
pub const STARTUP_TIMER: u64 = 1;

pub struct HostServicesPlugin {
    host: VtableHostServices,
}

impl Plugin for HostServicesPlugin {
    fn create(host: VtableHostServices, _init: Init) -> Result<Self, PluginError> {
        let plugin = Self { host };
        // `interval_ms: None`: a one-shot, firing once after the host's
        // minimum granularity (§6.11).
        match plugin.host.send(
            &Message::TimerSet(TimerSet {
                timer: STARTUP_TIMER,
                interval_ms: None,
            }),
            0,
        ) {
            vosu_status::VOSU_OK => Ok(plugin),
            status => Err(status.into()),
        }
    }

    fn on_message(&mut self, message: Message, _correlation_id: u64) -> Result<(), PluginError> {
        if let Message::TimerFired(TimerFired { timer }) = message {
            let _ = self
                .host
                .send(&Message::TimerCancel(TimerCancel { timer }), 0);
            for i in 0..LOG_BURST {
                let _ = self.host.send(
                    &Message::LogRecord(LogRecord {
                        level: LogLevel::Info,
                        message: format!("burst {i} after timer {timer} fired"),
                        fields: CborValue::Map(Vec::new()),
                    }),
                    0,
                );
            }
        }
        Ok(())
    }
}

vosu_plugin_api::export_plugin!(bundled HostServicesPlugin);
