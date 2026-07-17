//! Runtime discovery of keyboard shortcuts active at the current focus target.
//!
//! GPUI owns both halves of the source of truth: the dispatch tree says which
//! actions have handlers on the focused path, and the keymap says which
//! bindings match that path. Keeping the discovery here means shortcut UI
//! updates automatically whenever a caller adds another `App::bind_keys`
//! entry.

use gpui::{App, KeyBinding, Window};

/// One active, unshadowed binding in the focused dispatch stack.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShortcutHint {
    /// Platform-formatted key sequence, such as `ctrl-s` or `cmd-s`.
    pub keystroke: String,
    /// GPUI's registered action name, such as `gpui_notes::SavePage`.
    pub action_name: &'static str,
    context_depth: usize,
    registration_order: usize,
}

/// Return every key binding that can dispatch to the currently focused path.
///
/// Results are ordered leaf context first, then by the order in which the
/// binding was registered. That makes the first few entries suitable for the
/// status bar while keeping the complete overlay deterministic.
#[must_use]
pub fn for_focused(window: &Window, cx: &App) -> Vec<ShortcutHint> {
    let contexts = window.context_stack();
    let focused = window.focused(cx);
    let available_actions = window.available_actions(cx);
    let keymap = cx.key_bindings();
    let keymap = keymap.borrow();

    let mut hints = keymap
        .bindings()
        .enumerate()
        .filter_map(|(registration_order, binding)| {
            if !available_actions
                .iter()
                .any(|action| action.partial_eq(binding.action()))
            {
                return None;
            }

            let active_bindings = focused.as_ref().map_or_else(
                || window.bindings_for_action(binding.action()),
                |focus_handle| window.bindings_for_action_in(binding.action(), focus_handle),
            );
            if !active_bindings
                .iter()
                .any(|active| bindings_are_equal(binding, active))
            {
                return None;
            }

            let context_depth = if let Some(predicate) = binding.predicate() {
                predicate.depth_of(&contexts)?
            } else {
                // GPUI treats context-free bindings as if they matched the
                // deepest context when calculating dispatch precedence.
                contexts.len()
            };
            let keystroke = binding
                .keystrokes()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" ");

            Some(ShortcutHint {
                keystroke,
                action_name: binding.action().name(),
                context_depth,
                registration_order,
            })
        })
        .collect::<Vec<_>>();

    hints.sort_by(|left, right| {
        right
            .context_depth
            .cmp(&left.context_depth)
            .then(left.registration_order.cmp(&right.registration_order))
    });
    hints
}

fn bindings_are_equal(left: &KeyBinding, right: &KeyBinding) -> bool {
    left.action().partial_eq(right.action())
        && left.keystrokes() == right.keystrokes()
        && left.predicate() == right.predicate()
        && left.action_input() == right.action_input()
}
