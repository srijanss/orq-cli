//! Stage logic for a pipeline run: what each stage decides and does, kept
//! apart from the CLI and the popup that drive it.

mod gate;
mod store;

pub use gate::{choose_gate, Gate, GateError};
pub use store::{Event, RoleProfile, RunRecord, RunStore, StoreError};
