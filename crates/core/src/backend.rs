//! The platform input abstraction used by the engine.

use crate::geometry::{Point, Rect};
use crate::keys::Modifiers;

/// A logical (left) button transition. Platform backends are responsible for
/// mapping it to the physical button when the user has swapped buttons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonAction {
    Press,
    Release,
}

/// A failed platform input operation.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{operation} failed: {detail}")]
pub struct InputError {
    pub operation: &'static str,
    pub detail: String,
}

impl InputError {
    pub fn new(operation: &'static str, detail: impl Into<String>) -> Self {
        Self {
            operation,
            detail: detail.into(),
        }
    }
}

/// A [`InputBackend::send_buttons`] call that did not insert every action.
/// Actions are inserted in order, so exactly the first `inserted` took effect.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{error} ({inserted} of the requested events were inserted)")]
pub struct SendError {
    pub inserted: usize,
    pub error: InputError,
}

/// Platform input operations. Implementations are owned and called only by
/// the engine thread.
pub trait InputBackend: Send {
    fn cursor_position(&mut self) -> Result<Point, InputError>;
    fn move_cursor(&mut self, to: Point) -> Result<(), InputError>;
    /// Bounds of the monitor containing `at`; an error if no monitor does.
    fn monitor_bounds(&mut self, at: Point) -> Result<Rect, InputError>;
    /// Injects the actions in order as one uninterruptible batch.
    fn send_buttons(&mut self, actions: &[ButtonAction]) -> Result<(), SendError>;
    /// Modifiers currently held down (physical and injected).
    fn held_modifiers(&mut self) -> Result<Modifiers, InputError>;
    /// Advisory only: whether the OS currently reports the logical left button
    /// as down. Never used to override the engine's own record.
    fn button_reported_down(&mut self) -> Option<bool>;
}
