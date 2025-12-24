// Library re-exports for tests
// This file makes the code testable without reorganizing the entire binary
//
// Note: Many items appear as "dead code" in library checks because they're
// primarily used by the binary (main.rs). This is expected for a daemon.
#![allow(dead_code)]

include!("./main.rs");
