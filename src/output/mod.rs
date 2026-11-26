//! Output renderers. Both consume the same `Snapshot`, so JSON and the table
//! can never disagree about a host's state.

pub mod json;
pub mod sanitize;
pub mod stream;
pub mod table;
