//! Portable autoclicker logic.
//!
//! This crate has no operating-system dependencies. It will contain the
//! configuration model, click scheduling and the engine state machine, and is
//! tested deterministically on every platform.

#![forbid(unsafe_code)]
