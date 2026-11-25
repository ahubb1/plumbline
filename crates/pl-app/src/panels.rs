//! The markup furniture: the tool bar, the properties panel with its presets strip, the Edit
//! and Arrange menus, and the save and recovery dialogs.

use egui::{
    Button, Color32, Id, Key, Panel, RichText, Role, Sense, Stroke, StrokeKind, TextEdit, Ui,
    WidgetInfo,
};
use pl_doc::pl_annot::{
    AnnotKind, Annotation, FontStyle, FreeText, HAlign, LineEnding, Rgb, StampSpec, Style,
};
use pl_tools::{Align, Axis, ToolKind};

use crate::app::AppState;
use crate::commands::{self, Arrange, Command};
use crate::icons::{icon_button, icon_button_tip, Icon};
use crate::markup::{CloseChoice, Tool};
use crate::theme::{self, Palette};

/// The colours offered as swatches, by name.
pub const SWATCHES: [(&str, Rgb); 9] = [
    (
        "Red",
        Rgb {
            r: 0.8,
            g: 0.0,
            b: 0.0,
        },
    ),
    (
        "Orange",
        Rgb {
            r: 0.9,
            g: 0.45,
            b: 0.0,
        },
    ),
    (
        "Yellow",
        Rgb {
            r: 1.0,
            g: 0.9,
            b: 0.2,
        },
    ),
    (
        "Green",
        Rgb {
            r: 0.1,
            g: 0.55,
            b: 0.2,
        },
    ),
    (
        "Blue",
        Rgb {
            r: 0.1,
            g: 0.3,
            b: 0.8,
        },
    ),
    (
        "Violet",
        Rgb {
            r: 0.5,
            g: 0.2,
            b: 0.7,
        },
    ),
    (
        "Black",
        Rgb {
            r: 0.0,
            g: 0.0,
            b: 0.0,
        },
    ),
    (
        "Grey",
        Rgb {
            r: 0.45,
            g: 0.45,
            b: 0.45,
        },
    ),
    (
        "White",
        Rgb {
            r: 1.0,
            g: 1.0,
            b: 1.0,
        },
    ),
];

/// The dash styles offered, by name, as `/BS /D` arrays in page units.
pub const DASHES: [(&str, &[f64]); 4] = [
    ("Solid", &[]),
    ("Dashed", &[6.0, 4.0]),
    ("Dotted", &[1.5, 3.0]),
    ("Dash-dot", &[8.0, 3.0, 1.5, 3.0]),
];

fn ending_label(ending: LineEnding) -> &'static str {
    match ending {
        LineEnding::None => "None",
        LineEnding::OpenArrow => "Open arrow",
        LineEnding::ClosedArrow => "Closed arrow",
        LineEnding::Butt => "Bar",
        LineEnding::Circle => "Circle",
        LineEnding::Square => "Square",
        LineEnding::Diamond => "Diamond",
        LineEnding::Slash => "Slash",
    }
}

pub(crate) fn kind_label(kind: &AnnotKind) -> &str {
    match kind {
        AnnotKind::Square => "Rectangle",
        AnnotKind::Circle => "Ellipse",
        AnnotKind::Line => "Line",
        AnnotKind::PolyLine => "Polyline",
        AnnotKind::Polygon => "Polygon",
        AnnotKind::Ink => "Freehand",
        AnnotKind::Text => "Note",
        AnnotKind::FreeText => "Text box",
        AnnotKind::Stamp => "Stamp",
        AnnotKind::Highlight => "Highlight",
        AnnotKind::Underline => "Underline",
        AnnotKind::StrikeOut => "Strikeout",
        AnnotKind::Squiggly => "Squiggly underline",
        other => other.pdf_name(),
    }
}

// ---- tool bar -------------------------------------------------------------------------------

/// The tool bar: the tools two abreast in groups, then the snap, properties and list toggles.
pub fn tool_bar(app: &mut AppState, ui: &mut Ui, out: &mut Vec<Command>) {
    Panel::left("plumbline-tools")
        .resizable(false)
        .exact_size(62.0)
        .show(ui, |ui| {
            // On a short window the tools scroll rather than run off the bottom.
            let scroll = egui::ScrollArea::vertical()
                .id_salt("plumbline-tools-scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| tool_bar_body(app, ui, out));
            theme::name_scroll_bars(ui.ctx(), scroll.id, "Tools");
        });
}

fn tool_bar_body(app: &mut AppState, ui: &mut Ui, out: &mut Vec<Command>) {
    ui.add_space(4.0);
    ui.spacing_mut().item_spacing = egui::vec2(2.0, 2.0);
    let active = app.tool();
    let mut start = 0;
    let ends = Tool::GROUP_STARTS
        .into_iter()
        .chain([Tool::ALL.len()])
        .collect::<Vec<usize>>();
    for end in ends {
        for pair in Tool::ALL[start..end].chunks(2) {
            ui.horizontal(|ui| {
                for tool in pair {
                    let name = format!("{} tool", tool.label());
                    if icon_button_tip(ui, tool.icon(), &name, &tool.tooltip(), *tool == active)
                        .clicked()
                    {
                        out.push(Command::Tool(*tool));
                    }
                }
            });
        }
        start = end;
        ui.add_space(2.0);
        ui.separator();
        ui.add_space(2.0);
    }
    ui.horizontal(|ui| {
        let snap_tip = format!(
            "Snap to drawing content ({}). Right-click for the kinds of point.",
            commands::SNAP_KEY.name()
        );
        let snap = icon_button_tip(
            ui,
            Icon::Snap,
            "Snap to drawing content",
            &snap_tip,
            app.snap_enabled(),
        );
        if snap.clicked() {
            out.push(Command::ToggleSnap);
        }
        snap.context_menu(|ui| crate::measure::snap_menu(app, ui));
        if icon_button(
            ui,
            Icon::Sliders,
            "Properties panel",
            app.properties_panel_open(),
        )
        .clicked()
        {
            out.push(Command::TogglePropertiesPanel);
        }
    });
    ui.horizontal(|ui| {
        let tip = format!(
            "Markups list ({})",
            ui.ctx().format_shortcut(&commands::MARKUPS_LIST)
        );
        if icon_button_tip(
            ui,
            Icon::List,
            "Markups list",
            &tip,
            app.markups_list_open(),
        )
        .clicked()
        {
            out.push(Command::ToggleMarkupsList);
        }
    });
}

// ---- menus ----------------------------------------------------------------------------------

fn item(ui: &mut Ui, text: &str, shortcut: Option<&egui::KeyboardShortcut>, enabled: bool) -> bool {
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

/// The save entries of the File menu.
pub fn save_items(app: &AppState, ui: &mut Ui, out: &mut Vec<Command>) {
    let has_view = app.active().is_some();
    if item(ui, "Save", Some(&commands::SAVE), has_view) {
        out.push(Command::Save);
    }
    if item(ui, "Save as\u{2026}", Some(&commands::SAVE_AS), has_view) {
        out.push(Command::SaveAs);
    }
    if item(ui, "Save optimised", None, has_view) {
        out.push(Command::SaveOptimised);
    }
}

pub fn edit_menu(app: &AppState, ui: &mut Ui, out: &mut Vec<Command>) {
    let has_view = app.active().is_some();
    let selected = app.selected_count() > 0;
    let has_text = app
        .active_view()
        .is_some_and(|v| v.selection.as_ref().is_some_and(|s| !s.text.is_empty()));
    ui.menu_button("Edit", |ui| {
        if item(ui, "Undo", Some(&commands::UNDO), app.can_undo()) {
            out.push(Command::Undo);
        }
        if item(ui, "Redo", Some(&commands::REDO), app.can_redo()) {
            out.push(Command::Redo);
        }
        ui.separator();
        if item(ui, "Cut", Some(&commands::CUT), selected) {
            out.push(Command::Cut);
        }
        if item(ui, "Copy", Some(&commands::COPY), selected || has_text) {
            out.push(Command::Copy);
        }
        if item(ui, "Paste", Some(&commands::PASTE), app.can_paste()) {
            out.push(Command::Paste);
        }
        if item(ui, "Duplicate", Some(&commands::DUPLICATE), selected) {
            out.push(Command::Duplicate);
        }
        if item(ui, "Delete", None, selected) {
            out.push(Command::Delete);
        }
        ui.separator();
        if item(
            ui,
            "Select all text on page",
            Some(&commands::SELECT_ALL),
            has_view,
        ) {
            out.push(Command::SelectAll);
        }
        if item(
            ui,
            "Select all markups on page",
            Some(&commands::SELECT_ALL_MARKUPS),
            has_view,
        ) {
            out.push(Command::SelectAllMarkups);
        }
        ui.separator();
        ui.menu_button("Pages", |ui| {
            let several = app.page_targets().len() > 1;
            let what = if several { "pages" } else { "page" };
            for (text, command) in [
                ("Insert blank page".to_owned(), Command::InsertBlankPage),
                (
                    "Insert pages from file\u{2026}".to_owned(),
                    Command::InsertPagesFromFile,
                ),
                (
                    format!("Replace {what} from file\u{2026}"),
                    Command::ReplacePages,
                ),
                (
                    format!("Extract {what} to file\u{2026}"),
                    Command::ExtractPages,
                ),
                (format!("Rotate {what} clockwise"), Command::RotatePages(90)),
                (
                    format!("Rotate {what} anticlockwise"),
                    Command::RotatePages(-90),
                ),
                (format!("Delete {what}\u{2026}"), Command::DeletePages),
            ] {
                if item(ui, &text, None, has_view) {
                    out.push(command);
                }
            }
        });
        if item(ui, "Add bookmark", None, has_view) {
            out.push(Command::AddBookmark);
        }
        if item(ui, "Flatten markups\u{2026}", None, has_view) {
            out.push(Command::Flatten);
        }
        ui.separator();
        if item(
            ui,
            "Copy snapshot of view",
            Some(&commands::COPY_SNAPSHOT),
            has_view,
        ) {
            out.push(Command::CopySnapshot);
        }
        if item(ui, "Copy snapshot of region\u{2026}", None, has_view) {
            out.push(Command::CopySnapshotRegion);
        }
    });
}

/// The Measure menu: the scale, snapping, legends and the takeoff layout of the list.
pub fn measure_menu(app: &mut AppState, ui: &mut Ui, out: &mut Vec<Command>) {
    let has_view = app.active().is_some();
    ui.menu_button("Measure", |ui| {
        if item(ui, "Scale\u{2026}", None, has_view) {
            out.push(Command::ScalePanel);
        }
        if item(ui, "Calibrate", None, has_view) {
            out.push(Command::Tool(Tool::Calibrate));
        }
        ui.separator();
        ui.menu_button("Snap to", |ui| crate::measure::snap_menu(app, ui));
        ui.separator();
        ui.menu_button("Insert legend", |ui| {
            use pl_doc::pl_takeoff::Builtin;
            for (text, quantity) in [
                ("Counts by subject", None),
                ("Lengths by subject", Some(Builtin::Length)),
                ("Areas by subject", Some(Builtin::Area)),
                ("Volumes by subject", Some(Builtin::Volume)),
                ("Counted items by subject", Some(Builtin::Count)),
            ] {
                if item(ui, text, None, has_view) {
                    out.push(Command::InsertLegend(quantity));
                }
            }
        });
        if item(ui, "Takeoff layout of the markups list", None, has_view) {
            out.push(Command::TakeoffLayout);
        }
        if item(ui, "Formula column\u{2026}", None, has_view) {
            out.push(Command::FormulaColumn);
        }
    });
}

pub fn arrange_menu(app: &AppState, ui: &mut Ui, out: &mut Vec<Command>) {
    let count = app.selected_count();
    ui.menu_button("Arrange", |ui| {
        for (text, how) in [
            ("Align left", Align::Left),
            ("Align right", Align::Right),
            ("Align top", Align::Top),
            ("Align bottom", Align::Bottom),
            ("Align centres", Align::Centre),
            ("Align middles", Align::Middle),
        ] {
            if item(ui, text, None, count >= 2) {
                out.push(Command::Arrange(Arrange::Align(how)));
            }
        }
        ui.separator();
        for (text, axis) in [
            ("Distribute across", Axis::Horizontal),
            ("Distribute down", Axis::Vertical),
        ] {
            if item(ui, text, None, count >= 3) {
                out.push(Command::Arrange(Arrange::Distribute(axis)));
            }
        }
        ui.separator();
        for (text, shortcut, how) in [
            (
                "Bring to front",
                &commands::BRING_TO_FRONT,
                Arrange::BringToFront,
            ),
            (
                "Bring forward",
                &commands::BRING_FORWARD,
                Arrange::BringForward,
            ),
            (
                "Send backward",
                &commands::SEND_BACKWARD,
                Arrange::SendBackward,
            ),
            ("Send to back", &commands::SEND_TO_BACK, Arrange::SendToBack),
        ] {
            if item(ui, text, Some(shortcut), count >= 1) {
                out.push(Command::Arrange(how));
            }
        }
    });
}

// ---- properties panel -----------------------------------------------------------------------

/// What the panel is showing the properties of.
struct Subject {
    title: String,
    kind: AnnotKind,
    style: Style,
    /// The first selected markup, when there is a selection.
    markup: Option<Annotation>,
    tool: Option<ToolKind>,
    /// False for a markup shown from its own appearance, or a locked one.
    editable: bool,
    /// The text settings of a text box or a text tool.
    text: Option<FreeText>,
}

fn subject(app: &AppState) -> Option<Subject> {
    // Only the first selected markup is looked at, however many there are: this runs every
    // frame.
    let count = app.selected_count();
    if let Some(first) = app.first_selected() {
        let title = if count == 1 {
            format!("{} selected", kind_label(&first.kind))
        } else {
            format!("{count} markups selected")
        };
        // A text box from another program can be taken up as ours; its settings are shown as
        // they would be then.
        let mut shown = first.clone();
        if shown.kind == AnnotKind::FreeText {
            shown.make_editable();
        }
        return Some(Subject {
            title,
            kind: first.kind.clone(),
            style: shown.style.clone(),
            editable: shown.is_modelled() && !first.meta.flags.is_locked(),
            text: shown.free_text.clone(),
            markup: Some(first.clone()),
            tool: None,
        });
    }
    let tool = app.style_tool()?;
    let preset = app.presets().current(tool);
    let text = tool.is_text().then(|| {
        let mut text = FreeText::default();
        preset.text.apply_to(&mut text);
        text
    });
    Some(Subject {
        title: format!("{} tool style", tool.label()),
        kind: tool.annot_kind().unwrap_or(AnnotKind::Square),
        style: preset.style,
        markup: None,
        tool: Some(tool),
        editable: true,
        text,
    })
}

fn color32(rgb: Rgb) -> Color32 {
    let [r, g, b] = rgb.to_u8();
    Color32::from_rgb(r, g, b)
}

fn same_color(a: Option<Rgb>, b: Rgb) -> bool {
    a.is_some_and(|a| a.to_u8() == b.to_u8())
}

/// One colour swatch; `None` is the "no colour" swatch.
fn swatch(ui: &mut Ui, color: Option<Rgb>, name: &str, selected: bool) -> bool {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), Sense::click());
    let enabled = ui.is_enabled();
    response.widget_info(|| WidgetInfo::selected(Role::Button, enabled, selected, name));
    if ui.is_rect_visible(rect) {
        let palette = Palette::of(ui.visuals());
        let inner = rect.shrink(2.0);
        let fade = if enabled { 1.0 } else { 0.4 };
        match color {
            Some(rgb) => {
                ui.painter()
                    .rect_filled(inner, 2.0, color32(rgb).gamma_multiply(fade));
            }
            None => {
                ui.painter()
                    .rect_filled(inner, 2.0, ui.visuals().extreme_bg_color);
                ui.painter().line_segment(
                    [inner.left_bottom(), inner.right_top()],
                    Stroke::new(1.2, palette.muted_text),
                );
            }
        }
        let edge = if selected {
            Stroke::new(2.0, palette.accent)
        } else if response.hovered() {
            Stroke::new(1.0, ui.visuals().widgets.hovered.bg_stroke.color)
        } else {
            Stroke::new(1.0, palette.page_edge)
        };
        ui.painter().rect_stroke(
            if selected { rect.shrink(0.5) } else { inner },
            2.0,
            edge,
            StrokeKind::Inside,
        );
    }
    response.on_hover_text(name).clicked()
}

fn parse_hex(text: &str) -> Option<Rgb> {
    let hex = text.trim().trim_start_matches('#');
    if hex.len() != 6 || !hex.is_ascii() {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some(Rgb::from_u8(byte(0)?, byte(2)?, byte(4)?))
}

fn hex_of(color: Option<Rgb>) -> String {
    match color {
        Some(rgb) => {
            let [r, g, b] = rgb.to_u8();
            format!("#{r:02X}{g:02X}{b:02X}")
        }
        None => String::new(),
    }
}

/// A row of swatches and a hex box. Returns the colour picked: `Some(None)` is "no colour".
fn color_row(
    ui: &mut Ui,
    what: &str,
    current: Option<Rgb>,
    allow_none: bool,
    buffer: &mut String,
) -> Option<Option<Rgb>> {
    let mut picked = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(2.0, 2.0);
        if allow_none && swatch(ui, None, &format!("No {what} colour"), current.is_none()) {
            picked = Some(None);
        }
        for (name, rgb) in SWATCHES {
            let label = format!("{} colour {name}", capitalised(what));
            if swatch(ui, Some(rgb), &label, same_color(current, rgb)) {
                picked = Some(Some(rgb));
            }
        }
    });
    let id = Id::unique(("plumbline-hex", what));
    let focused = ui.memory(|m| m.has_focus(id));
    if !focused {
        *buffer = hex_of(current);
    }
    let name = format!("{} colour as hex", capitalised(what));
    let response = ui
        .add(
            TextEdit::singleline(buffer)
                .id(id)
                .desired_width(72.0)
                .hint_text("#RRGGBB"),
        )
        .accessible_name(&name)
        .on_hover_text(&name);
    if response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
        if let Some(rgb) = parse_hex(buffer) {
            picked = Some(Some(rgb));
        }
    }
    picked
}

fn capitalised(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn heading(ui: &mut Ui, text: &str) -> egui::Response {
    ui.add_space(4.0);
    ui.label(RichText::new(text).font(theme::bold(12.5)))
}

/// A labelled text box that shows `shown` until the user types in it. Returns the text
/// whenever the user changes it; `started` is set when the keyboard arrives in it.
#[allow(clippy::too_many_arguments)]
fn text_field(
    ui: &mut Ui,
    id: Id,
    buffer: &mut String,
    shown: &str,
    name: &str,
    rows: usize,
    started: &mut bool,
    take_focus: bool,
) -> Option<String> {
    let label = ui.label(name);
    let focused = ui.memory(|m| m.has_focus(id));
    if !focused {
        *buffer = shown.to_owned();
    }
    let edit = if rows > 1 {
        TextEdit::multiline(buffer).desired_rows(rows)
    } else {
        TextEdit::singleline(buffer)
    };
    let response = ui
        .add(edit.id(id).desired_width(f32::INFINITY))
        .labelled_by(label.id);
    if take_focus {
        response.request_focus();
    }
    if response.gained_focus() {
        *started = true;
    }
    response.changed().then(|| buffer.clone())
}

/// The properties panel, on the right.
pub fn properties_panel(app: &mut AppState, ui: &mut Ui) {
    Panel::right("plumbline-properties")
        .resizable(true)
        .default_size(216.0)
        .size_range(196.0..=340.0)
        .show(ui, |ui| {
            let editable = app.active_editable();
            let scroll = egui::ScrollArea::vertical()
                .id_salt("plumbline-properties-scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add_enabled_ui(editable, |ui| properties(app, ui));
                });
            theme::name_scroll_bars(ui.ctx(), scroll.id, "Properties");
        });
}

fn properties(app: &mut AppState, ui: &mut Ui) {
    let muted = Palette::of(ui.visuals()).muted_text;
    ui.add_space(4.0);
    ui.label(RichText::new("Properties").font(theme::bold(13.5)));
    // What the Stamp and Image tools place is chosen here, whatever is selected.
    match app.tool() {
        Tool::Stamp => stamp_palette(app, ui),
        Tool::Image => picture_chooser(app, ui),
        _ => {}
    }
    let Some(subject) = subject(app) else {
        ui.add_space(2.0);
        ui.label(
            RichText::new("Select a markup, or pick a drawing tool, to see its properties.")
                .color(muted),
        );
        ui.add_space(6.0);
        author_field(app, ui);
        return;
    };
    ui.label(RichText::new(&subject.title).color(muted));
    if subject.markup.is_some() && !subject.editable {
        ui.label(
            RichText::new(
                "This markup is shown as its author made it; it can be moved or deleted.",
            )
            .color(muted),
        );
    }
    // A measurement's value, units, caption, depth...; or the measurement tool's.
    crate::measure::measure_properties(app, ui);
    // A link: where it leads, and the buttons that follow it or change it.
    crate::links::link_properties(app, ui);
    if let Some(tool) = subject.tool {
        presets_strip(app, ui, tool);
    }

    let kind = subject.kind.clone();
    let style = subject.style.clone();
    let is_text = kind == AnnotKind::FreeText;
    let is_stamp = kind == AnnotKind::Stamp;
    // A text box has a border and a background like a rectangle.
    let is_shape = is_text
        || matches!(
            kind,
            AnnotKind::Square | AnnotKind::Circle | AnnotKind::Polygon
        );
    let is_line = matches!(kind, AnnotKind::Line | AnnotKind::PolyLine);
    let has_border = is_shape || is_line || kind == AnnotKind::Ink;
    let text = subject.text.clone();
    let session = app.markup.fields.session;
    let key = |index: u64| Some(session.wrapping_mul(16).wrapping_add(index));

    ui.add_enabled_ui(subject.editable, |ui| {
        if let Some(text) = &text {
            text_settings(app, ui, text);
        }
        if !is_stamp {
            let what = if is_text { "border" } else { "stroke" };
            heading(ui, &format!("{} colour", capitalised(what)));
            let mut hex = std::mem::take(&mut app.markup.fields.stroke_hex);
            if let Some(color) = color_row(ui, what, style.stroke, is_shape, &mut hex) {
                app.edit_style(None, &move |_, s| s.stroke = color);
            }
            app.markup.fields.stroke_hex = hex;
        }

        if is_shape || is_line {
            heading(ui, "Fill colour");
            let mut hex = std::mem::take(&mut app.markup.fields.fill_hex);
            if let Some(color) = color_row(ui, "fill", style.fill, true, &mut hex) {
                app.edit_style(None, &move |_, s| s.fill = color);
            }
            app.markup.fields.fill_hex = hex;
        }

        if has_border {
            heading(ui, "Line");
            ui.horizontal(|ui| {
                ui.label("Width");
                let mut width = style.width;
                let response = ui
                    .add(
                        egui::DragValue::new(&mut width)
                            .range(0.0..=72.0)
                            .speed(0.1)
                            .max_decimals(2)
                            .suffix(" pt"),
                    )
                    .accessible_name("Line width");
                if response.drag_started() || response.gained_focus() {
                    app.markup.fields.session += 1;
                }
                if response.changed() && width.is_finite() {
                    let session = app.markup.fields.session;
                    app.edit_style(
                        Some(session.wrapping_mul(16).wrapping_add(1)),
                        &move |_, s| s.width = width.clamp(0.0, 72.0),
                    );
                }
            });
        }

        if is_shape || is_line {
            ui.horizontal(|ui| {
                ui.label("Dash");
                let current = DASHES
                    .iter()
                    .find(|(_, array)| style.dash.as_deref().unwrap_or(&[]) == *array)
                    .map_or("Custom", |(name, _)| name);
                let mut picked: Option<&[f64]> = None;
                egui::ComboBox::from_id_salt("plumbline-dash")
                    .selected_text(current)
                    .width(96.0)
                    .show_ui(ui, |ui| {
                        for (name, array) in DASHES {
                            if ui.selectable_label(current == name, name).clicked() {
                                picked = Some(array);
                            }
                        }
                    })
                    .response
                    .accessible_name("Dash style");
                if let Some(array) = picked {
                    let dash = (!array.is_empty()).then(|| array.to_vec());
                    app.edit_style(None, &move |_, s| s.dash = dash.clone());
                }
            });
        }

        if is_line {
            for (label, name, start) in [("Start", "Line start", true), ("End", "Line end", false)]
            {
                ui.horizontal(|ui| {
                    ui.label(label);
                    let current = if start {
                        style.line_start
                    } else {
                        style.line_end
                    };
                    let mut picked = None;
                    egui::ComboBox::from_id_salt(("plumbline-ending", start))
                        .selected_text(ending_label(current))
                        .width(110.0)
                        .show_ui(ui, |ui| {
                            for ending in LineEnding::ALL {
                                if ui
                                    .selectable_label(current == ending, ending_label(ending))
                                    .clicked()
                                {
                                    picked = Some(ending);
                                }
                            }
                        })
                        .response
                        .accessible_name(name);
                    if let Some(ending) = picked {
                        app.edit_style(None, &move |_, s| {
                            if start {
                                s.line_start = ending;
                            } else {
                                s.line_end = ending;
                            }
                        });
                    }
                });
            }
        }

        if is_shape && !is_text {
            let mut cloud = style.cloud.is_some();
            if ui.checkbox(&mut cloud, "Cloud border").changed() {
                app.edit_style(None, &move |_, s| s.cloud = cloud.then_some(1.0));
            }
        }

        let opacity_heading = heading(ui, "Opacity");
        ui.horizontal(|ui| {
            let mut percent = (style.opacity * 100.0).round();
            let response = ui
                .add(
                    egui::Slider::new(&mut percent, 5.0..=100.0)
                        .step_by(5.0)
                        .show_value(false),
                )
                .labelled_by(opacity_heading.id);
            if response.drag_started() || response.gained_focus() {
                app.markup.fields.session += 1;
            }
            if response.changed() {
                let session = app.markup.fields.session;
                app.edit_style(
                    Some(session.wrapping_mul(16).wrapping_add(2)),
                    &move |_, s| s.opacity = (percent / 100.0).clamp(0.05, 1.0),
                );
            }
            ui.label(format!("{percent:.0}%"));
        });
    });

    heading(ui, "Details");
    author_field(app, ui);
    let mut started = false;
    match &subject.markup {
        Some(markup) => {
            let locked = markup.meta.flags.is_locked();
            ui.add_enabled_ui(!locked, |ui| {
                let mut buffer = std::mem::take(&mut app.markup.fields.subject);
                let shown = markup.meta.subject.clone().unwrap_or_default();
                if let Some(text) = text_field(
                    ui,
                    Id::unique("plumbline-subject"),
                    &mut buffer,
                    &shown,
                    "Subject",
                    1,
                    &mut started,
                    false,
                ) {
                    app.edit_meta(key(4), &move |a| {
                        a.meta.subject = (!text.is_empty()).then(|| text.clone());
                    });
                }
                app.markup.fields.subject = buffer;

                let mut buffer = std::mem::take(&mut app.markup.fields.comment);
                let shown = markup.meta.contents.clone().unwrap_or_default();
                // Not while the click that asked for it is still going on: its release
                // would take the keyboard away again.
                let focus = app.markup.fields.focus_comment
                    && !ui.input(|i| i.pointer.any_down() || i.pointer.any_released());
                if focus {
                    app.markup.fields.focus_comment = false;
                } else if app.markup.fields.focus_comment && ui.input(|i| i.pointer.any_released())
                {
                    // The frame after the release is the one that takes the keyboard.
                    ui.ctx().request_repaint();
                }
                if let Some(text) = text_field(
                    ui,
                    Id::unique("plumbline-comment"),
                    &mut buffer,
                    &shown,
                    "Comment",
                    4,
                    &mut started,
                    focus,
                ) {
                    app.edit_meta(key(5), &move |a| {
                        a.meta.contents = (!text.is_empty()).then(|| text.clone());
                    });
                }
                app.markup.fields.comment = buffer;
            });
        }
        None => {
            if let Some(tool) = subject.tool {
                let mut buffer = std::mem::take(&mut app.markup.fields.subject);
                let shown = app.presets().current(tool).subject;
                if let Some(text) = text_field(
                    ui,
                    Id::unique("plumbline-subject"),
                    &mut buffer,
                    &shown,
                    "Subject",
                    1,
                    &mut started,
                    false,
                ) {
                    let mut preset = app.presets().current(tool);
                    preset.subject = text;
                    app.set_current_preset(preset);
                }
                app.markup.fields.subject = buffer;
            }
        }
    }
    if started {
        app.markup.fields.session += 1;
    }
    // A polygon or rectangle can name a space (a room) that markups inside it count to.
    crate::measure::space_properties(app, ui);
    ui.add_space(8.0);
}

fn font_label(font: FontStyle) -> &'static str {
    match font {
        FontStyle::Regular => "Regular",
        FontStyle::Bold => "Bold",
        FontStyle::Italic => "Italic",
        FontStyle::BoldItalic => "Bold italic",
    }
}

fn align_label(align: HAlign) -> &'static str {
    match align {
        HAlign::Left => "Left",
        HAlign::Centre => "Centre",
        HAlign::Right => "Right",
    }
}

/// The text of a text box: size, face, alignment and colour.
fn text_settings(app: &mut AppState, ui: &mut Ui, text: &FreeText) {
    heading(ui, "Text");
    ui.horizontal(|ui| {
        ui.label("Size");
        let mut size = text.size;
        let response = ui
            .add(
                egui::DragValue::new(&mut size)
                    .range(4.0..=144.0)
                    .speed(0.25)
                    .max_decimals(1)
                    .suffix(" pt"),
            )
            .accessible_name("Font size");
        if response.drag_started() || response.gained_focus() {
            app.markup.fields.session += 1;
        }
        if response.changed() && size.is_finite() {
            let session = app.markup.fields.session;
            app.edit_free_text(Some(session.wrapping_mul(16).wrapping_add(6)), &move |t| {
                t.size = size.clamp(4.0, 144.0)
            });
        }
        let mut picked = None;
        egui::ComboBox::from_id_salt("plumbline-font-style")
            .selected_text(font_label(text.font))
            .width(84.0)
            .show_ui(ui, |ui| {
                for font in FontStyle::ALL {
                    if ui
                        .selectable_label(text.font == font, font_label(font))
                        .clicked()
                    {
                        picked = Some(font);
                    }
                }
            })
            .response
            .accessible_name("Font style");
        if let Some(font) = picked {
            app.edit_free_text(None, &move |t| t.font = font);
        }
    });
    ui.horizontal(|ui| {
        ui.label("Align");
        let mut picked = None;
        egui::ComboBox::from_id_salt("plumbline-text-align")
            .selected_text(align_label(text.align))
            .width(84.0)
            .show_ui(ui, |ui| {
                for align in [HAlign::Left, HAlign::Centre, HAlign::Right] {
                    if ui
                        .selectable_label(text.align == align, align_label(align))
                        .clicked()
                    {
                        picked = Some(align);
                    }
                }
            })
            .response
            .accessible_name("Text alignment");
        if let Some(align) = picked {
            app.edit_free_text(None, &move |t| t.align = align);
        }
    });
    heading(ui, "Text colour");
    let mut hex = std::mem::take(&mut app.markup.fields.text_hex);
    if let Some(Some(color)) = color_row(ui, "text", Some(text.color), false, &mut hex) {
        app.edit_free_text(None, &move |t| t.color = color);
    }
    app.markup.fields.text_hex = hex;
}

/// The built-in stamps, and the way to a picture of the user's own.
fn stamp_palette(app: &mut AppState, ui: &mut Ui) {
    heading(ui, "Stamp");
    let current = app.stamp();
    let mut chosen: Option<StampSpec> = None;
    let mut pick = false;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(3.0, 3.0);
        for spec in StampSpec::standard_set() {
            let selected = current.as_ref().is_some_and(|c| c.name == spec.name);
            let text = RichText::new(&spec.label)
                .font(theme::bold(11.5))
                .color(color32(spec.scheme.ink()));
            let response = ui.add(Button::selectable(selected, text));
            let enabled = ui.is_enabled();
            let name = format!("Stamp {}", spec.label);
            response.widget_info(|| WidgetInfo::selected(Role::Button, enabled, selected, &name));
            if response
                .on_hover_text(format!("Place the {} stamp", spec.label))
                .clicked()
            {
                chosen = Some(spec);
            }
        }
    });
    pick |= ui
        .button("Image as stamp\u{2026}")
        .on_hover_text("Choose a picture file and place it like a stamp")
        .clicked();
    if let Some(spec) = chosen {
        app.set_stamp(spec);
    }
    if pick && app.pick_image() {
        app.set_tool(Tool::Image);
    }
}

/// The Image tool's picture.
fn picture_chooser(app: &mut AppState, ui: &mut Ui) {
    heading(ui, "Picture");
    if ui
        .button("Choose picture\u{2026}")
        .on_hover_text("Choose the PNG or JPEG file this tool places")
        .clicked()
    {
        app.pick_image();
    }
}

/// The author box: the name new markups are signed with, or the author of the selection.
fn author_field(app: &mut AppState, ui: &mut Ui) {
    let selected: Option<String> = app
        .first_selected()
        .map(|a| a.meta.author.clone().unwrap_or_default());
    let mut buffer = std::mem::take(&mut app.markup.fields.author);
    let shown = selected.clone().unwrap_or_else(|| app.author().to_owned());
    let mut started = false;
    let changed = text_field(
        ui,
        Id::unique("plumbline-author"),
        &mut buffer,
        &shown,
        "Author",
        1,
        &mut started,
        false,
    );
    app.markup.fields.author = buffer;
    if started {
        app.markup.fields.session += 1;
    }
    if let Some(text) = changed {
        if selected.is_some() {
            let key = app.markup.fields.session.wrapping_mul(16).wrapping_add(3);
            app.edit_meta(Some(key), &move |a| {
                a.meta.author = (!text.is_empty()).then(|| text.clone());
            });
        } else {
            app.set_author(&text);
        }
    }
}

/// The presets of the active tool: one button each, and a box to save the current style.
fn presets_strip(app: &mut AppState, ui: &mut Ui, tool: ToolKind) {
    heading(ui, "Presets");
    let current = app.presets().current(tool).name;
    let names: Vec<String> = app.presets().named(tool).map(|p| p.name.clone()).collect();
    let mut chosen = None;
    if !names.is_empty() {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(3.0, 3.0);
            for name in &names {
                let button = Button::selectable(*name == current, name.as_str());
                if ui
                    .add(button)
                    .on_hover_text(format!("Draw with the preset \"{name}\""))
                    .clicked()
                {
                    chosen = Some(name.clone());
                }
            }
        });
    }
    if let Some(name) = chosen {
        app.use_preset(&name);
    }
    let mut save = false;
    let mut remove = false;
    ui.horizontal(|ui| {
        let response = ui
            .add(
                TextEdit::singleline(&mut app.markup.fields.preset_name)
                    .id(Id::unique("plumbline-preset-name"))
                    .desired_width(92.0)
                    .hint_text("Name"),
            )
            .accessible_name("Preset name");
        save |= response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        save |= ui
            .button("Save preset")
            .on_hover_text("Save this tool's current style under the name")
            .clicked();
    });
    if !current.is_empty() {
        remove = ui
            .add(Button::new(format!("Remove \"{current}\"")).frame(false))
            .clicked();
    }
    if save {
        let name = app.markup.fields.preset_name.clone();
        if app.save_preset(&name) {
            app.markup.fields.preset_name.clear();
        }
    }
    if remove {
        app.remove_preset(&current);
    }
}

// ---- dialogs --------------------------------------------------------------------------------

/// "Save changes before closing?"
pub fn close_prompt(app: &mut AppState, ctx: &egui::Context) {
    let Some(prompt) = app.markup.close_prompt else {
        return;
    };
    let title = app
        .view(prompt.view)
        .map(|v| v.title.clone())
        .unwrap_or_default();
    let mut choice = None;
    let modal = egui::Modal::new(Id::unique("plumbline-close-prompt")).show(ctx, |ui| {
        ui.set_width(360.0);
        ui.label(RichText::new("Save changes?").font(theme::bold(15.0)));
        ui.add_space(4.0);
        ui.label(format!(
            "{title} has markups that have not been saved. Closing without saving loses them."
        ));
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("Save and close").clicked() {
                choice = Some(CloseChoice::Save);
            }
            if ui.button("Discard changes").clicked() {
                choice = Some(CloseChoice::Discard);
            }
            if ui.button("Cancel").clicked() {
                choice = Some(CloseChoice::Cancel);
            }
        });
    });
    if choice.is_none() && modal.should_close() {
        choice = Some(CloseChoice::Cancel);
    }
    if let Some(choice) = choice {
        app.answer_close_prompt(ctx, choice);
    }
}

/// "Unsaved markups from an earlier session were found."
pub fn recovery_prompt(app: &mut AppState, ctx: &egui::Context) {
    let Some(doc) = app.markup.recovery else {
        return;
    };
    let Ok(title) = app.session().document(doc).map(|d| d.title()) else {
        app.markup.recovery = None;
        return;
    };
    let mut answer = None;
    egui::Modal::new(Id::unique("plumbline-recovery-prompt")).show(ctx, |ui| {
        ui.set_width(380.0);
        ui.label(RichText::new("Recover unsaved markups?").font(theme::bold(15.0)));
        ui.add_space(4.0);
        ui.label(format!(
            "Plumbline closed unexpectedly while {title} had markups that were not saved. \
             They can be brought back now."
        ));
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("Recover").clicked() {
                answer = Some(true);
            }
            if ui.button("Discard them").clicked() {
                answer = Some(false);
            }
        });
    });
    if let Some(recover) = answer {
        app.answer_recovery_prompt(recover);
    }
}
