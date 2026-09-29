//! Code shared by the wisprcheap desktop app and the sync server (the Android app has a Kotlin port,
//! checked against `testdata/vectors.json`). See `SPEC.md` for the wire format.

pub mod crypto;
pub mod hlc;
pub mod pricing;
pub mod profile;
pub mod protocol;
pub mod stats;
