//! Requests a compositor decides on before they take effect.
//!
//! A few protocols let a client act as the person or read what other clients hold: a
//! virtual keyboard types into the focused window, and data control reads and sets the
//! clipboard at any time. A compositor that asks the person first can keep such a request
//! until it knows. The protocol hands each request to its handler as a [`Pending`] one,
//! which the compositor carries out at once, keeps to carry out or refuse later, or refuses:
//!
//! - virtual keyboard: [`SeatHandler::virtual_keyboard_request`](crate::input::SeatHandler::virtual_keyboard_request);
//! - ext and wlr data control:
//!   [`SelectionHandler::data_control_request`](crate::wayland::selection::SelectionHandler::data_control_request).
//!
//! By default every request is carried out at once, as before. A compositor that keeps one
//! request of a client keeps that client's later requests of the same protocol behind it,
//! so they take effect in order.

use std::fmt;

/// What a pending request would do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Effect {
    /// Prepares the requests after it and shows nothing on its own, such as a keymap
    /// upload: worth keeping only behind a kept request.
    Setup,
    /// Acts as the person: a key press or a modifier change.
    Input,
    /// Reads what another client holds: receiving the clipboard.
    Read,
    /// Changes what every client sees: setting or clearing the clipboard.
    Change,
}

/// A client's request, handed over to be carried out now, later or never.
pub struct Pending<D> {
    effect: Effect,
    run: Box<dyn FnOnce(&mut D)>,
    refuse: Box<dyn FnOnce(&mut D)>,
}

impl<D> fmt::Debug for Pending<D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pending").field("effect", &self.effect).finish()
    }
}

impl<D> Pending<D> {
    pub(crate) fn new(
        effect: Effect,
        run: impl FnOnce(&mut D) + 'static,
        refuse: impl FnOnce(&mut D) + 'static,
    ) -> Self {
        Self {
            effect,
            run: Box::new(run),
            refuse: Box::new(refuse),
        }
    }

    /// What it would do.
    pub fn effect(&self) -> Effect {
        self.effect
    }

    /// Carry it out. One kept past its client's disconnect still takes effect: a virtual
    /// keyboard's keys are typed, as if the client had stayed.
    pub fn run(self, state: &mut D) {
        (self.run)(state)
    }

    /// Drop it, telling the client where the protocol can: a data-control source is
    /// cancelled, and a read gets end of file.
    pub fn refuse(self, state: &mut D) {
        (self.refuse)(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pending_request_does_one_thing_once() {
        let made = |log: &'static str| {
            Pending::<Vec<&'static str>>::new(
                Effect::Change,
                move |state| state.push(log),
                |state| state.push("refused"),
            )
        };
        let mut state = Vec::new();
        let first = made("first");
        assert_eq!(first.effect(), Effect::Change);
        let second = made("second");
        second.refuse(&mut state);
        first.run(&mut state);
        assert_eq!(state, ["refused", "first"]);
    }
}
