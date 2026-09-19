//! Provider-neutral domain objects.
//!
//! Everything in here is shared vocabulary: adapters produce it, services transform it, the API
//! layer projects it. Nothing in here knows what a rollout file, a JSONL record or a SQLite
//! column is — that knowledge stops at the adapter boundary (`adapters/`).

mod account;
mod metrics;
mod project;
mod session;
mod status;

pub use account::{
    AccountStatus, Capacity, CapacityWindow, CapacityWindowName, Identity, ProviderSummary,
};
pub use metrics::{Kpis, MetricBasis, MetricState, Metrics};
pub use project::{summarize as summarize_project, MetricsTotals, ProjectSummary, StatusCounts};
pub use session::{
    Diagnostics, FileSignature, ProjectKey, ProviderId, ResumeBlockedReason, Session, SessionKey,
    SourceSignature, SourceSummary, NO_DIRECTORY,
};
pub use status::{
    LiveCounts, LiveObservation, LiveSignature, LiveState, ProcessPresence, SessionStatus,
    StatusSnapshot,
};
