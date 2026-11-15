//! `arc test`: run a Protocol against checked-in fixture files in place of its fetches.

// Nothing consumes this yet; the commands that run tests arrive in later changes.
#![allow(dead_code)]

mod spec;

#[allow(unused_imports)]
pub(crate) use spec::{Expect, ProtocolTest, TESTS_FILENAME, load_tests};
