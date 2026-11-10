//! Portable autoclicker logic.
//!
//! This crate has no operating-system dependencies. It contains the
//! configuration model, click scheduling, the engine state machine and the
//! traits platform backends implement, and is tested deterministically on
//! every platform.

#![forbid(unsafe_code)]

pub mod backend;
pub mod config;
pub mod engine;
pub mod geometry;
pub mod keys;
pub mod schedule;
pub mod status;
pub mod wait;

#[cfg(test)]
mod testkit;
