//! Core's two independent listener roles, not caller-provided origin headers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListenerRole {
    Overlay,
    Dashboard,
}
