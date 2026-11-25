//! Everything the user can ask for, and the keys that ask for it.

use std::path::PathBuf;

use egui::{Key, KeyboardShortcut, Modifiers};

use crate::markup::Tool;

/// One user action. Menus, toolbar buttons and keyboard shortcuts all go through these.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// Show the file dialog and open what is picked.
    Open,
    OpenPath(PathBuf),
    ClearRecent,
    CloseTab,
    Quit,
    NextPage,
    PreviousPage,
    FirstPage,
    LastPage,
    /// Scroll by this many lines (positive is down).
    ScrollLines(f32),
    /// Scroll by this many viewport heights (positive is down).
    ScrollPages(f32),
    ZoomIn,
    ZoomOut,
    FitWidth,
    FitPage,
    ActualSize,
    RotateRight,
    RotateLeft,
    ToggleSidePanel,
    SplitSide,
    SplitStack,
    /// Show the find bar and put the keyboard in it.
    Find,
    FindNext,
    FindPrevious,
    CloseFind,
    FocusPageBox,
    Copy,
    SelectAll,
    /// Escape: abandon the gesture in progress, else close the find bar, else drop the
    /// selection, else go back to the Select tool.
    Dismiss,
    Undo,
    Redo,
    Save,
    /// Ask for a file name and save there; the tab then belongs to that file.
    SaveAs,
    /// Rewrite the whole file as one revision, dropping what is no longer used.
    SaveOptimised,
    /// Cut the selected markups.
    Cut,
    /// Paste markups onto the page in view.
    Paste,
    /// Copy and paste the selected markups in one step, leaving the clipboard alone.
    Duplicate,
    /// Delete the selected markups.
    Delete,
    /// Select every markup on the page in view.
    SelectAllMarkups,
    /// Make this the active tool.
    Tool(Tool),
    ToggleSnap,
    TogglePropertiesPanel,
    Arrange(Arrange),
    /// Copy what the active view shows to the clipboard, as an image.
    CopySnapshot,
    /// Let the user drag a region of the view to copy as an image.
    CopySnapshotRegion,
    /// Show or hide the markups list at the bottom of the window.
    ToggleMarkupsList,
    /// Ask for a file and write the markups list (or, for XFDF, the markups) to it.
    ExportMarkups(crate::platform::ExportKind),
    /// Ask for an XFDF file and add its markups to the document.
    ImportXfdf,
    /// Ask for a picture and make it what the Image tool places.
    PickImage,
    /// Show the scale panel for the page (or viewport) under the pointer.
    ScalePanel,
    /// Place a legend on the page in view: the markups by subject with their counts, or with
    /// the total of this quantity column.
    InsertLegend(Option<pl_doc::pl_takeoff::Builtin>),
    /// Lay the markups list out for a takeoff: the quantity columns, grouped by subject.
    TakeoffLayout,
    /// Show the dialog that adds a formula column to the markups list.
    FormulaColumn,
    /// Insert a blank page after the selected pages (or the page in view).
    InsertBlankPage,
    /// Ask for a PDF and insert its pages after the selected pages (or the page in view).
    InsertPagesFromFile,
    /// Delete the selected pages (or the page in view), after asking.
    DeletePages,
    /// Turn the selected pages (or the page in view) clockwise by this many degrees (a
    /// multiple of 90; negative is anticlockwise). This changes the file, unlike
    /// [`Command::RotateRight`], which turns only the view.
    RotatePages(i32),
    /// Ask for a file name and write the selected pages (or the page in view) to it.
    ExtractPages,
    /// Ask for a PDF and replace the selected pages with its first pages.
    ReplacePages,
    /// Show the dialog that builds a new document from several files.
    CombineFiles,
    /// Show the flatten dialog.
    Flatten,
    /// Show the print panel.
    Print,
    /// Back to where the view was before the last link or bookmark it followed.
    NavigateBack,
    /// Forward again after [`Command::NavigateBack`].
    NavigateForward,
    /// Add a bookmark to the place in view.
    AddBookmark,
    /// Ask for a drawing set and open it in the Set panel.
    OpenSet,
    /// Ask where to write a new drawing set made of the open documents.
    NewSet,
    /// The next sheet of the open drawing set.
    NextSheet,
    /// The previous sheet of the open drawing set.
    PreviousSheet,
}

/// Lining up, spacing out and restacking the selected markups.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arrange {
    Align(pl_tools::Align),
    Distribute(pl_tools::Axis),
    BringToFront,
    BringForward,
    SendBackward,
    SendToBack,
}

/// A key binding. `plain` bindings are ignored while the user is typing in a text field;
/// `exact` ones only fire when no other modifier key is held (the single-letter tool keys).
#[derive(Clone, Debug)]
pub struct Binding {
    pub shortcut: KeyboardShortcut,
    pub command: Command,
    pub plain: bool,
    pub exact: bool,
}

const CTRL: Modifiers = Modifiers::COMMAND;
const CTRL_SHIFT: Modifiers = Modifiers {
    alt: false,
    ctrl: false,
    shift: true,
    mac_cmd: false,
    command: true,
};

pub const OPEN: KeyboardShortcut = KeyboardShortcut::new(CTRL, Key::O);
pub const CLOSE_TAB: KeyboardShortcut = KeyboardShortcut::new(CTRL, Key::W);
pub const FIND: KeyboardShortcut = KeyboardShortcut::new(CTRL, Key::F);
pub const FIND_NEXT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::NONE, Key::F3);
pub const FIND_PREVIOUS: KeyboardShortcut = KeyboardShortcut::new(Modifiers::SHIFT, Key::F3);
pub const ZOOM_IN: KeyboardShortcut = KeyboardShortcut::new(CTRL, Key::Plus);
pub const ZOOM_OUT: KeyboardShortcut = KeyboardShortcut::new(CTRL, Key::Minus);
pub const FIT_PAGE: KeyboardShortcut = KeyboardShortcut::new(CTRL, Key::Num0);
pub const ACTUAL_SIZE: KeyboardShortcut = KeyboardShortcut::new(CTRL, Key::Num1);
pub const FIT_WIDTH: KeyboardShortcut = KeyboardShortcut::new(CTRL, Key::Num2);
pub const ROTATE_RIGHT: KeyboardShortcut = KeyboardShortcut::new(CTRL, Key::R);
pub const ROTATE_LEFT: KeyboardShortcut = KeyboardShortcut::new(CTRL_SHIFT, Key::R);
pub const SIDE_PANEL: KeyboardShortcut = KeyboardShortcut::new(CTRL, Key::B);
pub const SPLIT_SIDE: KeyboardShortcut = KeyboardShortcut::new(CTRL, Key::Backslash);
pub const SPLIT_STACK: KeyboardShortcut = KeyboardShortcut::new(CTRL_SHIFT, Key::Backslash);
pub const GO_TO_PAGE: KeyboardShortcut = KeyboardShortcut::new(CTRL, Key::G);
pub const COPY: KeyboardShortcut = KeyboardShortcut::new(CTRL, Key::C);
pub const SELECT_ALL: KeyboardShortcut = KeyboardShortcut::new(CTRL, Key::A);
pub const UNDO: KeyboardShortcut = KeyboardShortcut::new(CTRL, Key::Z);
pub const REDO: KeyboardShortcut = KeyboardShortcut::new(CTRL, Key::Y);
pub const REDO_ALT: KeyboardShortcut = KeyboardShortcut::new(CTRL_SHIFT, Key::Z);
pub const SAVE: KeyboardShortcut = KeyboardShortcut::new(CTRL, Key::S);
pub const SAVE_AS: KeyboardShortcut = KeyboardShortcut::new(CTRL_SHIFT, Key::S);
pub const CUT: KeyboardShortcut = KeyboardShortcut::new(CTRL, Key::X);
pub const PASTE: KeyboardShortcut = KeyboardShortcut::new(CTRL, Key::V);
pub const DUPLICATE: KeyboardShortcut = KeyboardShortcut::new(CTRL, Key::D);
pub const SELECT_ALL_MARKUPS: KeyboardShortcut = KeyboardShortcut::new(CTRL_SHIFT, Key::A);
pub const COPY_SNAPSHOT: KeyboardShortcut = KeyboardShortcut::new(CTRL_SHIFT, Key::C);
pub const PROPERTIES_PANEL: KeyboardShortcut = KeyboardShortcut::new(CTRL, Key::E);
pub const MARKUPS_LIST: KeyboardShortcut = KeyboardShortcut::new(CTRL, Key::L);
pub const BRING_TO_FRONT: KeyboardShortcut = KeyboardShortcut::new(CTRL_SHIFT, Key::CloseBracket);
pub const SEND_TO_BACK: KeyboardShortcut = KeyboardShortcut::new(CTRL_SHIFT, Key::OpenBracket);
pub const BRING_FORWARD: KeyboardShortcut = KeyboardShortcut::new(CTRL, Key::CloseBracket);
pub const SEND_BACKWARD: KeyboardShortcut = KeyboardShortcut::new(CTRL, Key::OpenBracket);
pub const PRINT: KeyboardShortcut = KeyboardShortcut::new(CTRL, Key::P);
const ALT: Modifiers = Modifiers::ALT;
pub const NAVIGATE_BACK: KeyboardShortcut = KeyboardShortcut::new(ALT, Key::ArrowLeft);
pub const NAVIGATE_FORWARD: KeyboardShortcut = KeyboardShortcut::new(ALT, Key::ArrowRight);
pub const NEXT_SHEET: KeyboardShortcut = KeyboardShortcut::new(ALT, Key::PageDown);
pub const PREVIOUS_SHEET: KeyboardShortcut = KeyboardShortcut::new(ALT, Key::PageUp);
/// Turns snapping to drawing content on and off.
pub const SNAP_KEY: Key = Key::S;

/// Every binding, more specific modifier sets first (egui matches a shortcut when its
/// modifiers are a subset of those held).
pub fn bindings() -> Vec<Binding> {
    let b = |shortcut, command, plain| Binding {
        shortcut,
        command,
        plain,
        exact: false,
    };
    let key = |k| KeyboardShortcut::new(Modifiers::NONE, k);
    let mut all = vec![
        b(NAVIGATE_BACK, Command::NavigateBack, true),
        b(NAVIGATE_FORWARD, Command::NavigateForward, true),
        b(NEXT_SHEET, Command::NextSheet, false),
        b(PREVIOUS_SHEET, Command::PreviousSheet, false),
        b(PRINT, Command::Print, false),
        b(REDO_ALT, Command::Redo, true),
        b(SAVE_AS, Command::SaveAs, false),
        b(SELECT_ALL_MARKUPS, Command::SelectAllMarkups, true),
        b(COPY_SNAPSHOT, Command::CopySnapshot, false),
        b(
            BRING_TO_FRONT,
            Command::Arrange(Arrange::BringToFront),
            true,
        ),
        b(SEND_TO_BACK, Command::Arrange(Arrange::SendToBack), true),
        b(UNDO, Command::Undo, true),
        b(REDO, Command::Redo, true),
        b(SAVE, Command::Save, false),
        b(CUT, Command::Cut, true),
        b(PASTE, Command::Paste, true),
        b(DUPLICATE, Command::Duplicate, true),
        b(PROPERTIES_PANEL, Command::TogglePropertiesPanel, false),
        b(MARKUPS_LIST, Command::ToggleMarkupsList, false),
        b(BRING_FORWARD, Command::Arrange(Arrange::BringForward), true),
        b(SEND_BACKWARD, Command::Arrange(Arrange::SendBackward), true),
        b(ROTATE_LEFT, Command::RotateLeft, false),
        b(SPLIT_STACK, Command::SplitStack, false),
        b(FIND_PREVIOUS, Command::FindPrevious, false),
        b(OPEN, Command::Open, false),
        b(CLOSE_TAB, Command::CloseTab, false),
        b(FIND, Command::Find, false),
        b(FIND_NEXT, Command::FindNext, false),
        b(ZOOM_IN, Command::ZoomIn, false),
        b(
            KeyboardShortcut::new(CTRL, Key::Equals),
            Command::ZoomIn,
            false,
        ),
        b(ZOOM_OUT, Command::ZoomOut, false),
        b(FIT_PAGE, Command::FitPage, false),
        b(ACTUAL_SIZE, Command::ActualSize, false),
        b(FIT_WIDTH, Command::FitWidth, false),
        b(ROTATE_RIGHT, Command::RotateRight, false),
        b(SIDE_PANEL, Command::ToggleSidePanel, false),
        b(SPLIT_SIDE, Command::SplitSide, false),
        b(GO_TO_PAGE, Command::FocusPageBox, false),
        b(COPY, Command::Copy, true),
        b(SELECT_ALL, Command::SelectAll, true),
        b(
            KeyboardShortcut::new(CTRL, Key::Home),
            Command::FirstPage,
            true,
        ),
        b(
            KeyboardShortcut::new(CTRL, Key::End),
            Command::LastPage,
            true,
        ),
        b(key(Key::Escape), Command::Dismiss, false),
        b(key(Key::ArrowRight), Command::NextPage, true),
        b(key(Key::ArrowLeft), Command::PreviousPage, true),
        b(key(Key::ArrowDown), Command::ScrollLines(1.0), true),
        b(key(Key::ArrowUp), Command::ScrollLines(-1.0), true),
        b(key(Key::PageDown), Command::ScrollPages(1.0), true),
        b(key(Key::PageUp), Command::ScrollPages(-1.0), true),
        b(key(Key::Home), Command::FirstPage, true),
        b(key(Key::End), Command::LastPage, true),
    ];
    // One letter per tool, and one for snapping. They come last, and fire only with exactly
    // their own modifiers: Shift and a letter for the Measure group (first, as egui would
    // otherwise match them on the bare letter), the bare letter for the others.
    for tool in Tool::ALL.into_iter().filter(|t| t.is_measure()) {
        all.push(Binding {
            shortcut: tool.shortcut(),
            command: Command::Tool(tool),
            plain: true,
            exact: true,
        });
    }
    for tool in Tool::ALL.into_iter().filter(|t| !t.is_measure()) {
        all.push(Binding {
            shortcut: key(tool.key()),
            command: Command::Tool(tool),
            plain: true,
            exact: true,
        });
    }
    all.push(Binding {
        shortcut: key(SNAP_KEY),
        command: Command::ToggleSnap,
        plain: true,
        exact: true,
    });
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    /// egui matches a shortcut when its modifiers are a subset of those held, so a binding
    /// must never come after one that would swallow it.
    #[test]
    fn no_binding_is_shadowed_by_an_earlier_one() {
        let all = bindings();
        for (i, later) in all.iter().enumerate() {
            for earlier in &all[..i] {
                let shadows = earlier.shortcut.logical_key == later.shortcut.logical_key
                    && later
                        .shortcut
                        .modifiers
                        .matches_logically(earlier.shortcut.modifiers)
                    && earlier.command != later.command;
                assert!(
                    !shadows,
                    "{:?} is unreachable behind {:?}",
                    later.command, earlier.command
                );
            }
        }
    }

    #[test]
    fn every_shortcut_is_bound_once() {
        let all = bindings();
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert!(
                    a.shortcut != b.shortcut,
                    "{:?} and {:?} share a shortcut",
                    a.command,
                    b.command
                );
            }
        }
        assert!(all.iter().any(|b| b.command == Command::Open));
        assert!(all.iter().any(|b| b.command == Command::Find));
    }

    #[test]
    fn every_tool_has_its_own_key() {
        let all = bindings();
        for tool in Tool::ALL {
            let bound: Vec<&Binding> = all
                .iter()
                .filter(|b| b.command == Command::Tool(tool))
                .collect();
            assert_eq!(bound.len(), 1, "{tool:?}");
            assert!(bound[0].exact && bound[0].plain);
            let wanted = if tool.is_measure() {
                Modifiers::SHIFT
            } else {
                Modifiers::NONE
            };
            assert_eq!(bound[0].shortcut.modifiers, wanted, "{tool:?}");
            assert_eq!(bound[0].shortcut.logical_key, tool.key());
        }
        // Every tool key is its own: no two tools share a key and modifiers.
        for (i, a) in Tool::ALL.iter().enumerate() {
            for b in &Tool::ALL[i + 1..] {
                assert!(a.shortcut() != b.shortcut(), "{a:?} and {b:?}");
            }
        }
        // A key with an exact binding that also has a Ctrl binding must come after it.
        for (i, bare) in all.iter().enumerate().filter(|(_, b)| b.exact) {
            for later in all[i + 1..].iter().filter(|b| !b.exact) {
                assert!(
                    later.shortcut.logical_key != bare.shortcut.logical_key,
                    "{:?} comes after the exact key of {:?}",
                    later.command,
                    bare.command
                );
            }
        }
    }
}
