//! Outbound network contracts.
//!
//! `harness` covers the exact-listener loopback harness and runs in every
//! test pass. `forge` and `os_store` are the D15 internal-CA evidence: they are
//! `#[ignore]`d because they need a real Forgejo, an internal CA installed in
//! the operating system's trust store and exact fixture hostnames, which only
//! `python scripts/test-catalog-forge.py` (Linux containers) or its
//! `--windows-vm` mode provisions. Run outside that runner with `--ignored`,
//! each of them fails with the missing prerequisite instead of passing.

mod fixture;
mod forge;
mod harness;
mod os_store;
mod recorder;
