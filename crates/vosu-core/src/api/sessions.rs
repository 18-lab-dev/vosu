//! Session targets use only core-minted public ids, never transient handles.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionTarget {
    Active,
    Id(String),
}
