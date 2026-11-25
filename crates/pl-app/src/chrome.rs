//! The window furniture: command bar, find bar, side panel, status bar, tabs and the start page.

use egui::{
    Align, Button, Id, Key, Layout, Panel, RichText, Role, Sense, Stroke, StrokeKind, TextEdit,
    ThemePreference, Ui, WidgetInfo,
};
use egui_tiles::{SimplificationOptions, TileId, Tiles, UiResponse};
use pl_doc::FitMode;

use crate::app::{display_name, AppState, LengthUnit, SidePage, ViewId};
use crate::canvas;
use crate::commands::{self, Command};
use crate::icons::{icon_button, Icon};
use crate::theme::{self, Palette};

const PAGE_BOX_ID: &str = "plumbline-page-box";
const ZOOM_BOX_ID: &str = "plumbline-zoom-box";
const FIND_BOX_ID: &str = "plumbline-find-box";

/// Whether `id` is the find bar's text box.
pub(crate) fn is_find_box(id: Id) -> bool {
    id == Id::unique(FIND_BOX_ID)
}

/// Lays out the whole window for one frame.
pub fn show(app: &mut AppState, ui: &mut Ui) {
    let mut commands: Vec<Command> = Vec::new();

    Panel::top("plumbline-commands").show(ui, |ui| {
        ui.add_space(2.0);
        command_bar(app, ui, &mut commands);
        ui.add_space(2.0);
    });
    if app.chrome.find_open && app.active.is_some() {
        Panel::top("plumbline-find").show(ui, |ui| {
            ui.add_space(1.0);
            find_bar(app, ui, &mut commands);
            ui.add_space(1.0);
        });
    }
    Panel::bottom("plumbline-status").show(ui, |ui| status_bar(app, ui));
    if app.active.is_some() {
        crate::panels::tool_bar(app, ui, &mut commands);
        // The markups list runs under everything but the tool bar.
        if app.list.open {
            crate::list::panel(app, ui);
        }
        if app.markup.properties_open {
            crate::panels::properties_panel(app, ui);
        }
    }
    if app.chrome.side_open && app.active.is_some() {
        Panel::left("plumbline-side")
            .resizable(true)
            .default_size(196.0)
            .size_range(150.0..=380.0)
            .show(ui, |ui| side_panel(app, ui));
    }
    egui::CentralPanel::no_frame().show(ui, |ui| {
        if app.views.is_empty() {
            start_page(app, ui, &mut commands);
        } else {
            let mut tree = std::mem::replace(
                &mut app.tree,
                egui_tiles::Tree::empty(Id::unique("plumbline-swap")),
            );
            tree.ui(&mut TabBehavior { app }, ui);
            app.tree = tree;
        }
    });

    // A view whose tab is no longer in front cannot stay the active one. This runs before the
    // commands, which may open a view that is first shown in the next frame.
    if let Some(first) = app.shown_this_frame.first().copied() {
        if app
            .active
            .is_none_or(|a| !app.shown_this_frame.contains(&a))
        {
            app.active = Some(first);
        }
    }

    let ctx = ui.ctx().clone();
    for command in commands {
        app.run(&ctx, command);
    }
    app.follow_clicked_link();
}

// ---- command bar ----------------------------------------------------------------------------

fn menu_item(
    ui: &mut Ui,
    text: &str,
    shortcut: Option<&egui::KeyboardShortcut>,
    enabled: bool,
) -> bool {
    let mut button = Button::new(text);
    if let Some(shortcut) = shortcut {
        button = button.shortcut_text(ui.ctx().format_shortcut(shortcut));
    }
    let clicked = ui.add_enabled(enabled, button).clicked();
    if clicked {
        ui.close();
    }
    clicked
}

fn file_menu(app: &mut AppState, ui: &mut Ui, out: &mut Vec<Command>) {
    let has_view = app.active.is_some();
    ui.menu_button("File", |ui| {
        if menu_item(ui, "Open\u{2026}", Some(&commands::OPEN), true) {
            out.push(Command::Open);
        }
        ui.menu_button("Open recent", |ui| {
            let entries: Vec<std::path::PathBuf> = app
                .recent
                .as_ref()
                .map(|r| r.entries().iter().map(|e| e.path.clone()).collect())
                .unwrap_or_default();
            if entries.is_empty() {
                ui.weak("No recent files");
            }
            for path in &entries {
                let item = ui
                    .button(display_name(path))
                    .on_hover_text(path.to_string_lossy());
                if item.clicked() {
                    out.push(Command::OpenPath(path.clone()));
                    ui.close();
                }
            }
            if !entries.is_empty() {
                ui.separator();
                if menu_item(ui, "Clear this list", None, true) {
                    out.push(Command::ClearRecent);
                }
            }
        });
        ui.separator();
        crate::panels::save_items(app, ui, out);
        ui.separator();
        if menu_item(ui, "Print\u{2026}", Some(&commands::PRINT), has_view) {
            out.push(Command::Print);
        }
        if menu_item(ui, "Combine files\u{2026}", None, true) {
            out.push(Command::CombineFiles);
        }
        ui.separator();
        if menu_item(ui, "Open set\u{2026}", None, true) {
            out.push(Command::OpenSet);
        }
        if menu_item(ui, "New set from open files\u{2026}", None, has_view) {
            out.push(Command::NewSet);
        }
        ui.separator();
        if menu_item(ui, "Close tab", Some(&commands::CLOSE_TAB), has_view) {
            out.push(Command::CloseTab);
        }
        ui.separator();
        if menu_item(ui, "Exit", None, true) {
            out.push(Command::Quit);
        }
    });
}

fn view_menu(app: &mut AppState, ui: &mut Ui, out: &mut Vec<Command>) {
    let has_view = app.active.is_some();
    ui.menu_button("View", |ui| {
        let items: [(&str, &egui::KeyboardShortcut, Command); 5] = [
            ("Zoom in", &commands::ZOOM_IN, Command::ZoomIn),
            ("Zoom out", &commands::ZOOM_OUT, Command::ZoomOut),
            ("Fit width", &commands::FIT_WIDTH, Command::FitWidth),
            ("Fit page", &commands::FIT_PAGE, Command::FitPage),
            ("Actual size", &commands::ACTUAL_SIZE, Command::ActualSize),
        ];
        for (text, shortcut, command) in items {
            if menu_item(ui, text, Some(shortcut), has_view) {
                out.push(command);
            }
        }
        ui.separator();
        let items: [(&str, &egui::KeyboardShortcut, Command); 4] = [
            (
                "Rotate view clockwise",
                &commands::ROTATE_RIGHT,
                Command::RotateRight,
            ),
            (
                "Rotate view anticlockwise",
                &commands::ROTATE_LEFT,
                Command::RotateLeft,
            ),
            (
                "Split side by side",
                &commands::SPLIT_SIDE,
                Command::SplitSide,
            ),
            ("Split stacked", &commands::SPLIT_STACK, Command::SplitStack),
        ];
        for (text, shortcut, command) in items {
            if menu_item(ui, text, Some(shortcut), has_view) {
                out.push(command);
            }
        }
        ui.separator();
        let items: [(&str, &egui::KeyboardShortcut, Command); 4] = [
            ("Back", &commands::NAVIGATE_BACK, Command::NavigateBack),
            (
                "Forward",
                &commands::NAVIGATE_FORWARD,
                Command::NavigateForward,
            ),
            (
                "Next sheet of the set",
                &commands::NEXT_SHEET,
                Command::NextSheet,
            ),
            (
                "Previous sheet of the set",
                &commands::PREVIOUS_SHEET,
                Command::PreviousSheet,
            ),
        ];
        let has_set = app.sets.set.is_some();
        for (i, (text, shortcut, command)) in items.into_iter().enumerate() {
            let enabled = if i < 2 { has_view } else { has_set };
            if menu_item(ui, text, Some(shortcut), enabled) {
                out.push(command);
            }
        }
        ui.separator();
        if menu_item(ui, "Side panel", Some(&commands::SIDE_PANEL), true) {
            out.push(Command::ToggleSidePanel);
        }
        if menu_item(
            ui,
            "Properties panel",
            Some(&commands::PROPERTIES_PANEL),
            has_view,
        ) {
            out.push(Command::TogglePropertiesPanel);
        }
        if menu_item(ui, "Markups list", Some(&commands::MARKUPS_LIST), has_view) {
            out.push(Command::ToggleMarkupsList);
        }
        if menu_item(ui, "Snap to drawing content", None, has_view) {
            out.push(Command::ToggleSnap);
        }
        ui.menu_button("Units", |ui| {
            for unit in LengthUnit::ALL {
                if ui
                    .radio_value(&mut app.chrome.unit, unit, unit.label())
                    .clicked()
                {
                    ui.close();
                }
            }
        });
        ui.menu_button("Theme", |ui| {
            for (theme, label) in [
                (ThemePreference::System, "Match system"),
                (ThemePreference::Light, "Light"),
                (ThemePreference::Dark, "Dark"),
            ] {
                if ui
                    .radio_value(&mut app.chrome.theme, theme, label)
                    .clicked()
                {
                    ui.close();
                }
            }
        });
    });
}

/// A small text box whose content follows `shown` until the user starts typing in it. Returns
/// the text when the user commits it with Enter.
fn value_box(
    ui: &mut Ui,
    id: Id,
    buffer: &mut String,
    shown: String,
    width: f32,
    name: &str,
    take_focus: &mut bool,
) -> Option<String> {
    let focused = ui.memory(|m| m.has_focus(id));
    if !focused {
        *buffer = shown;
    }
    let response = ui
        .add(
            TextEdit::singleline(buffer)
                .id(id)
                .desired_width(width)
                .align(egui::Align2::CENTER_CENTER),
        )
        .accessible_name(name)
        .on_hover_text(name);
    if std::mem::take(take_focus) {
        response.request_focus();
    }
    if response.gained_focus() {
        // Typing replaces the value rather than adding to it.
        buffer.clear();
    }
    if response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
        return Some(buffer.trim().to_owned());
    }
    None
}

fn command_bar(app: &mut AppState, ui: &mut Ui, out: &mut Vec<Command>) {
    ui.horizontal(|ui| {
        file_menu(app, ui, out);
        crate::panels::edit_menu(app, ui, out);
        view_menu(app, ui, out);
        crate::panels::arrange_menu(app, ui, out);
        crate::panels::measure_menu(app, ui, out);
        ui.separator();
        if icon_button(ui, Icon::SidePanel, "Side panel", app.chrome.side_open).clicked() {
            out.push(Command::ToggleSidePanel);
        }

        let info = app.active_view().map(|v| {
            (
                v.shown_page(),
                v.view.page_count(),
                v.view.zoom(),
                v.view.fit(),
            )
        });
        let (page, count, zoom, fit) = info.unwrap_or((0, 0, 1.0, FitMode::None));
        let has_view = info.is_some();
        ui.add_enabled_ui(has_view, |ui| {
            ui.separator();
            if icon_button(ui, Icon::ChevronUp, "Previous page", false).clicked() {
                out.push(Command::PreviousPage);
            }
            let shown = if has_view {
                (page + 1).to_string()
            } else {
                String::new()
            };
            let mut focus = std::mem::take(&mut app.chrome.focus_page_box);
            if let Some(text) = value_box(
                ui,
                Id::unique(PAGE_BOX_ID),
                &mut app.chrome.page_box,
                shown,
                38.0,
                "Page number",
                &mut focus,
            ) {
                match text.parse::<u32>() {
                    Ok(n) if n >= 1 && n <= count => {
                        if let Some(dv) = app.active_view_mut() {
                            dv.go_to_page(n - 1);
                        }
                    }
                    _ if text.is_empty() => {}
                    _ => {
                        app.chrome.status =
                            Some(format!("There is no page \"{text}\" (1 to {count})"));
                    }
                }
            }
            ui.label(if has_view {
                format!("of {count}")
            } else {
                "of -".to_owned()
            });
            if icon_button(ui, Icon::ChevronDown, "Next page", false).clicked() {
                out.push(Command::NextPage);
            }

            ui.separator();
            if icon_button(ui, Icon::Minus, "Zoom out", false).clicked() {
                out.push(Command::ZoomOut);
            }
            let shown = if has_view {
                format!("{:.0}%", zoom * 100.0)
            } else {
                String::new()
            };
            let mut focus = false;
            if let Some(text) = value_box(
                ui,
                Id::unique(ZOOM_BOX_ID),
                &mut app.chrome.zoom_box,
                shown,
                50.0,
                "Zoom percent",
                &mut focus,
            ) {
                let number = text.trim_end_matches('%').trim().parse::<f64>();
                if let (Ok(percent), Some(dv)) = (number, app.active_view_mut()) {
                    if percent.is_finite() && percent > 0.0 {
                        dv.view.set_zoom(percent / 100.0);
                    }
                }
            }
            if icon_button(ui, Icon::Plus, "Zoom in", false).clicked() {
                out.push(Command::ZoomIn);
            }
            if ui
                .add(Button::selectable(fit == FitMode::Width, "Fit width"))
                .clicked()
            {
                out.push(Command::FitWidth);
            }
            if ui
                .add(Button::selectable(fit == FitMode::Page, "Fit page"))
                .clicked()
            {
                out.push(Command::FitPage);
            }
            if ui
                .add(Button::selectable(
                    has_view && fit == FitMode::None && (zoom - 1.0).abs() < 1e-9,
                    "100%",
                ))
                .on_hover_text("Actual size")
                .clicked()
            {
                out.push(Command::ActualSize);
            }

            ui.separator();
            if icon_button(ui, Icon::TurnLeft, "Rotate view anticlockwise", false).clicked() {
                out.push(Command::RotateLeft);
            }
            if icon_button(ui, Icon::TurnRight, "Rotate view clockwise", false).clicked() {
                out.push(Command::RotateRight);
            }
            ui.separator();
            if icon_button(ui, Icon::SplitSide, "Split side by side", false).clicked() {
                out.push(Command::SplitSide);
            }
            if icon_button(ui, Icon::SplitStack, "Split stacked", false).clicked() {
                out.push(Command::SplitStack);
            }
            ui.separator();
            if icon_button(ui, Icon::Lens, "Find", app.chrome.find_open).clicked() {
                out.push(if app.chrome.find_open {
                    Command::CloseFind
                } else {
                    Command::Find
                });
            }
        });
    });
}

// ---- find bar -------------------------------------------------------------------------------

fn find_bar(app: &mut AppState, ui: &mut Ui, out: &mut Vec<Command>) {
    ui.horizontal(|ui| {
        ui.label("Find");
        let response = ui
            .add(
                TextEdit::singleline(&mut app.chrome.find_text)
                    .id(Id::unique(FIND_BOX_ID))
                    .desired_width(240.0)
                    .hint_text("Text to find"),
            )
            .accessible_name("Find text");
        if std::mem::take(&mut app.chrome.focus_find) {
            response.request_focus();
        }
        if response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
            out.push(if ui.input(|i| i.modifiers.shift) {
                Command::FindPrevious
            } else {
                Command::FindNext
            });
            app.chrome.focus_find = true;
        }
        let mut changed = response.changed();
        changed |= ui
            .toggle_value(&mut app.chrome.find_options.match_case, "Match case")
            .changed();
        changed |= ui
            .toggle_value(&mut app.chrome.find_options.whole_word, "Whole word")
            .changed();
        if changed {
            // The hits on screen no longer belong to what the box says.
            if let Some(dv) = app.active_view_mut() {
                dv.search = None;
            }
        }
        if icon_button(ui, Icon::ChevronUp, "Previous match", false).clicked() {
            out.push(Command::FindPrevious);
        }
        if icon_button(ui, Icon::ChevronDown, "Next match", false).clicked() {
            out.push(Command::FindNext);
        }
        let summary = match app.active_view().and_then(|v| v.search.as_ref()) {
            None => String::new(),
            Some(search) if !search.is_finished() => {
                let (done, total) = search.progress();
                format!("Searching\u{2026} page {done} of {total}")
            }
            Some(search) if search.is_empty() => "No matches".to_owned(),
            Some(search) => format!(
                "{} of {}",
                search.current_index().map_or(0, |i| i + 1),
                search.len()
            ),
        };
        ui.label(summary);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if icon_button(ui, Icon::Cross, "Close find bar", false).clicked() {
                out.push(Command::CloseFind);
            }
        });
    });
}

// ---- side panel -----------------------------------------------------------------------------

fn side_panel(app: &mut AppState, ui: &mut Ui) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.selectable_value(&mut app.chrome.side_page, SidePage::Thumbnails, "Pages");
        ui.selectable_value(&mut app.chrome.side_page, SidePage::Bookmarks, "Bookmarks");
        ui.selectable_value(&mut app.chrome.side_page, SidePage::Layers, "Layers");
        if app.sets.set.is_some() {
            ui.selectable_value(&mut app.chrome.side_page, SidePage::Set, "Set")
                .on_hover_text("The sheets of the open drawing set");
        }
    });
    if app.chrome.side_page == SidePage::Set && app.sets.set.is_none() {
        app.chrome.side_page = SidePage::Thumbnails;
    }
    ui.separator();
    let mut commands = Vec::new();
    match app.chrome.side_page {
        SidePage::Thumbnails => crate::pages::thumbnails(app, ui, &mut commands),
        SidePage::Bookmarks => crate::bookmarks::panel(app, ui),
        SidePage::Layers => layers(app, ui),
        SidePage::Set => crate::setpanel::panel(app, ui),
    }
    let ctx = ui.ctx().clone();
    for command in commands {
        app.run(&ctx, command);
    }
}

/// The document's layers, each with a box that shows or hides it in this window. Nothing here
/// changes the file.
fn layers(app: &mut AppState, ui: &mut Ui) {
    let Some(doc_id) = app.active_view().map(|v| v.doc) else {
        return;
    };
    // The list is read from the file, on a worker thread, the first time this page is shown
    // for a document.
    if !app.ensure_layers(doc_id) {
        ui.add_space(6.0);
        if app.layers_loading(doc_id) {
            ui.weak("Reading layers\u{2026}");
        } else {
            ui.weak("The layers could not be read.");
        }
        return;
    }
    let list = app.layers();
    if list.is_empty() {
        ui.add_space(6.0);
        ui.weak("This document has no layers.");
        return;
    }
    let mut toggle = None;
    let mut reset = false;
    let scroll = egui::ScrollArea::vertical()
        .id_salt("plumbline-layers")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (index, (name, shown)) in list.iter().enumerate() {
                let label = if name.trim().is_empty() {
                    format!("Layer {}", index + 1)
                } else {
                    name.clone()
                };
                let mut on = *shown;
                if ui
                    .checkbox(&mut on, label)
                    .on_hover_text("Show or hide this layer here; the file is not changed")
                    .changed()
                {
                    toggle = Some((index, on));
                }
            }
            ui.add_space(6.0);
            reset = ui
                .add(Button::new("Show as saved").frame(false))
                .on_hover_text("Show every layer the way the file has it")
                .clicked();
        });
    theme::name_scroll_bars(ui.ctx(), scroll.id, "Layers");
    if let Some((index, on)) = toggle {
        app.set_layer_visible(Some(index), on);
    }
    if reset {
        app.set_layer_visible(None, true);
    }
}

// ---- status bar -----------------------------------------------------------------------------

fn status_bar(app: &mut AppState, ui: &mut Ui) {
    let info = app.status_info();
    let muted = Palette::of(ui.visuals()).muted_text;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        let message = app.chrome.status.clone().unwrap_or_default();
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if !info.scale.is_empty() {
                // The scale at the pointer; a click opens the scale panel.
                // (Quiet either way: no scale is muted, not an alarm.)
                let unset = info.scale == "Scale not set";
                let text = if unset {
                    RichText::new(&info.scale).color(muted)
                } else {
                    RichText::new(&info.scale)
                };
                let response = ui
                    .add(Button::new(text).frame(false))
                    .accessible_name(format!("Scale: {}", info.scale))
                    .on_hover_text("The scale at the pointer. Click to set the scale.");
                if response.clicked() {
                    app.open_scale_panel(None);
                }
                ui.separator();
            }
            if !info.page.is_empty() {
                ui.label(&info.zoom);
                ui.separator();
                ui.label(&info.page);
                ui.separator();
                ui.label(&info.size);
                if let Some(cursor) = &info.cursor {
                    ui.separator();
                    ui.label(RichText::new(cursor).color(muted));
                }
            }
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                if let Some(activity) = app.background_activity() {
                    activity_mark(ui, &activity);
                    ui.separator();
                }
                if !info.tool.is_empty() {
                    ui.label(&info.tool);
                    ui.separator();
                }
                // The last notice, or else what the active tool does.
                let text = if message.is_empty() {
                    info.hint.clone()
                } else {
                    message
                };
                ui.add(egui::Label::new(RichText::new(text).color(muted)).truncate());
            });
        });
    });
}

/// A small mark that something is going on in the background: three dots taking turns, and
/// what it is. It never blocks anything; it asks for a slow repaint to keep moving.
fn activity_mark(ui: &mut Ui, text: &str) {
    let palette = Palette::of(ui.visuals());
    let (rect, response) = ui.allocate_exact_size(egui::vec2(22.0, 12.0), Sense::hover());
    let label = format!("{text}\u{2026}");
    response.widget_info(|| WidgetInfo::labeled(Role::ProgressIndicator, true, &label));
    let phase = (ui.input(|i| i.time) * 3.0) as usize % 3;
    for dot in 0..3 {
        let centre = egui::pos2(rect.left() + 4.0 + 7.0 * dot as f32, rect.center().y);
        let colour = if dot == phase {
            palette.accent
        } else {
            palette.muted_text.gamma_multiply(0.5)
        };
        ui.painter().circle_filled(centre, 2.2, colour);
    }
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_millis(120));
    ui.label(label);
}

// ---- start page -----------------------------------------------------------------------------

fn start_page(app: &mut AppState, ui: &mut Ui, out: &mut Vec<Command>) {
    let palette = Palette::of(ui.visuals());
    ui.painter()
        .rect_filled(ui.max_rect(), 0.0, ui.visuals().panel_fill);
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() * 0.22).max(24.0));
        ui.label(RichText::new("Plumbline").font(theme::bold(24.0)));
        ui.add_space(4.0);
        ui.label(
            RichText::new("Open a PDF, or drop one on this window.").color(palette.muted_text),
        );
        ui.add_space(14.0);
        if ui.button("Open a file\u{2026}").clicked() {
            out.push(Command::Open);
        }
        let entries: Vec<std::path::PathBuf> = app
            .recent
            .as_ref()
            .map(|r| r.entries().iter().take(8).map(|e| e.path.clone()).collect())
            .unwrap_or_default();
        if !entries.is_empty() {
            ui.add_space(22.0);
            ui.label(RichText::new("Recent").font(theme::bold(13.5)));
            ui.add_space(2.0);
            for path in entries {
                let item = ui
                    .add(Button::new(display_name(&path)).frame(false))
                    .on_hover_text(path.to_string_lossy());
                if item.clicked() {
                    out.push(Command::OpenPath(path));
                }
            }
        }
    });
}

// ---- tabs and splits ------------------------------------------------------------------------

struct TabBehavior<'a> {
    app: &'a mut AppState,
}

impl egui_tiles::Behavior<ViewId> for TabBehavior<'_> {
    fn pane_ui(&mut self, ui: &mut Ui, _tile_id: TileId, pane: &mut ViewId) -> UiResponse {
        canvas::show(self.app, ui, *pane);
        UiResponse::None
    }

    fn tab_title_for_pane(&mut self, pane: &ViewId) -> egui::WidgetText {
        self.app.view_title(*pane).into()
    }

    fn on_tab_close(&mut self, tiles: &mut Tiles<ViewId>, tile_id: TileId) -> bool {
        let Some(pane) = tiles.get_pane(&tile_id).copied() else {
            return true;
        };
        // A document with unsaved markups is asked about first; the prompt closes the tab.
        let last_view = !self
            .app
            .views
            .values()
            .any(|v| v.id != pane && Some(v.doc) == self.app.views.get(&pane).map(|p| p.doc));
        if last_view && (self.app.is_modified(pane) || self.app.is_saving(pane)) {
            // Asked about first, or (while it is being saved) refused for now.
            self.app.request_close(pane);
            return false;
        }
        true
    }

    fn is_tab_closable(&self, _tiles: &Tiles<ViewId>, _tile_id: TileId) -> bool {
        true
    }

    fn on_tab_button(
        &mut self,
        tiles: &mut Tiles<ViewId>,
        tile_id: TileId,
        button_response: egui::Response,
    ) -> egui::Response {
        if button_response.clicked() {
            if let Some(pane) = tiles.get_pane(&tile_id) {
                self.app.active = Some(*pane);
            }
        }
        button_response
    }

    fn simplification_options(&self) -> SimplificationOptions {
        SimplificationOptions {
            all_panes_must_have_tabs: true,
            ..SimplificationOptions::default()
        }
    }

    fn tab_bar_color(&self, visuals: &egui::Visuals) -> egui::Color32 {
        visuals.panel_fill
    }

    fn gap_width(&self, _style: &egui::Style) -> f32 {
        2.0
    }
}

// ---- overlays -------------------------------------------------------------------------------

/// Asks for a document's password when one is needed.
pub fn password_prompt(app: &mut AppState, ctx: &egui::Context) {
    let Some(prompt) = app.chrome.password.as_mut() else {
        return;
    };
    let mut submit = false;
    let mut cancel = false;
    let modal = egui::Modal::new(Id::unique("plumbline-password")).show(ctx, |ui| {
        ui.set_width(340.0);
        ui.label(RichText::new("Password needed").font(theme::bold(15.0)));
        ui.add_space(4.0);
        ui.label(format!(
            "{} is protected. Enter its password to open it.",
            display_name(&prompt.path)
        ));
        let field = ui
            .add(
                TextEdit::singleline(&mut prompt.text)
                    .password(true)
                    .desired_width(f32::INFINITY),
            )
            .accessible_name("Password");
        if std::mem::take(&mut prompt.focus) {
            field.request_focus();
        }
        if field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
            submit = true;
        }
        if prompt.wrong {
            ui.label("That password did not open the file.");
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            submit |= ui.button("Open").clicked();
            cancel |= ui.button("Cancel").clicked();
        });
    });
    if cancel || modal.should_close() {
        app.chrome.password = None;
    } else if submit {
        let path = prompt.path.clone();
        let password = prompt.text.clone();
        app.open_with_password(&path, Some(&password));
    }
}

/// Shows that a file held over the window can be dropped.
pub fn drop_hint(ctx: &egui::Context) {
    if ctx.input(|i| i.raw.hovered_files.is_empty()) {
        return;
    }
    let rect = ctx.content_rect();
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        Id::unique("plumbline-drop-hint"),
    ));
    let visuals = ctx.global_style().visuals.clone();
    painter.rect_filled(rect, 0.0, visuals.panel_fill.gamma_multiply(0.85));
    painter.rect_stroke(
        rect.shrink(12.0),
        6.0,
        Stroke::new(2.0, Palette::of(&visuals).accent),
        StrokeKind::Inside,
    );
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "Drop to open",
        theme::bold(20.0),
        visuals.strong_text_color(),
    );
}
