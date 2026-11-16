#![cfg(windows)]

mod platform;
mod usn;
pub use platform::*;
pub use usn::{JournalRecord, parse_records};
