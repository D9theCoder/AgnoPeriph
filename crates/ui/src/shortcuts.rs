//! Window keyboard shortcuts and the reference list shown in the shortcuts popover.

use gpui_kit::*;

/// Key context of the app's root element.
pub const CONTEXT: &str = "AgnoPeriph";

actions!(
    agnoperiph,
    [
        ApplyChanges,
        UndoChange,
        NextTab,
        PreviousTab,
        ToggleShortcuts,
        Dismiss,
        SelectDevice1,
        SelectDevice2,
        SelectDevice3,
        SelectDevice4,
        SelectDevice5,
        SelectDevice6,
        SelectDevice7,
        SelectDevice8,
        SelectDevice9,
    ]
);

struct KeysBound;

impl Global for KeysBound {}

/// Binds the app shortcuts once per application.
pub fn bind_keys(cx: &mut App) {
    if cx.has_global::<KeysBound>() {
        return;
    }
    cx.set_global(KeysBound);
    let context = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("ctrl-s", ApplyChanges, context),
        KeyBinding::new("ctrl-z", UndoChange, context),
        KeyBinding::new("ctrl-tab", NextTab, context),
        KeyBinding::new("ctrl-shift-tab", PreviousTab, context),
        KeyBinding::new("ctrl-/", ToggleShortcuts, context),
        KeyBinding::new("escape", Dismiss, context),
        KeyBinding::new("ctrl-1", SelectDevice1, context),
        KeyBinding::new("ctrl-2", SelectDevice2, context),
        KeyBinding::new("ctrl-3", SelectDevice3, context),
        KeyBinding::new("ctrl-4", SelectDevice4, context),
        KeyBinding::new("ctrl-5", SelectDevice5, context),
        KeyBinding::new("ctrl-6", SelectDevice6, context),
        KeyBinding::new("ctrl-7", SelectDevice7, context),
        KeyBinding::new("ctrl-8", SelectDevice8, context),
        KeyBinding::new("ctrl-9", SelectDevice9, context),
    ]);
}

/// Rows of the shortcuts reference: description and the keystrokes that trigger it.
pub const REFERENCE: [(&str, &[&str]); 6] = [
    ("Go to device 1–9", &["ctrl-1", "ctrl-9"]),
    ("Next or previous tab", &["ctrl-tab", "ctrl-shift-tab"]),
    ("Apply changes", &["ctrl-s"]),
    ("Undo last change", &["ctrl-z"]),
    ("Show shortcuts", &["ctrl-/"]),
    ("Close dialog or popover", &["escape"]),
];
