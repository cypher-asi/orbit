pub mod logging;
pub mod models;
pub mod routes;
pub mod service;

// Re-export the key types and the `emit` function for convenient access
// by other modules: `events::emit(...)`, `events::NewAuditEvent { ... }`.
pub use models::{AuditEvent, EventFilter, NewAuditEvent};
pub use service::emit;
pub use service::get_event;
pub use service::list_events;
