//! Fonts and the two colour themes.
//!
//! egui's own fonts are compiled out; the UI font is Liberation Sans (SIL OFL 1.1), embedded
//! from `vendor/fonts`. The look is deliberately plain: neutral greys, one muted accent, thin
//! separators, no shadows to speak of.

use std::sync::Arc;

use egui::{
    Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle, Theme,
    Visuals,
};

/// Liberation Sans Regular, the UI font.
pub const FONT_REGULAR: &[u8] = include_bytes!("../../../vendor/fonts/LiberationSans-Regular.ttf");
/// Liberation Sans Bold, used for the few headings.
pub const FONT_BOLD: &[u8] = include_bytes!("../../../vendor/fonts/LiberationSans-Bold.ttf");

/// The name of the bold font family.
pub const BOLD_FAMILY: &str = "plumbline-bold";

/// Installs the fonts, both themes and the spacing on a context. Call it once, before the
/// first frame: with the default fonts compiled out, egui has nothing to draw text with until
/// this has run.
pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::empty();
    fonts.font_data.insert(
        "liberation-sans".to_owned(),
        Arc::new(FontData::from_static(FONT_REGULAR)),
    );
    fonts.font_data.insert(
        "liberation-sans-bold".to_owned(),
        Arc::new(FontData::from_static(FONT_BOLD)),
    );
    let regular = vec!["liberation-sans".to_owned()];
    fonts
        .families
        .insert(FontFamily::Proportional, regular.clone());
    fonts.families.insert(FontFamily::Monospace, regular);
    fonts.families.insert(
        FontFamily::Name(BOLD_FAMILY.into()),
        vec![
            "liberation-sans-bold".to_owned(),
            "liberation-sans".to_owned(),
        ],
    );
    ctx.set_fonts(fonts);

    ctx.set_visuals_of(Theme::Light, light_visuals());
    ctx.set_visuals_of(Theme::Dark, dark_visuals());
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(6.0, 5.0);
        style.spacing.button_padding = egui::vec2(7.0, 3.0);
        style.spacing.interact_size.y = 24.0;
        style.text_styles = [
            (TextStyle::Small, FontId::proportional(11.5)),
            (TextStyle::Body, FontId::proportional(13.5)),
            (TextStyle::Button, FontId::proportional(13.5)),
            (TextStyle::Monospace, FontId::monospace(13.0)),
            (TextStyle::Heading, FontId::proportional(19.0)),
        ]
        .into();
    });
    // Ctrl +/- zoom the document, not the whole interface.
    ctx.options_mut(|options| options.zoom_with_keyboard = false);
}

/// The bold face at a size.
pub fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(BOLD_FAMILY.into()))
}

fn grey(v: u8) -> Color32 {
    Color32::from_gray(v)
}

fn base(mut v: Visuals, p: &Scheme) -> Visuals {
    let radius = CornerRadius::same(3);
    v.panel_fill = p.panel;
    v.window_fill = p.raised;
    v.extreme_bg_color = p.field;
    v.faint_bg_color = p.faint;
    v.code_bg_color = p.faint;
    v.window_stroke = Stroke::new(1.0, p.line);
    v.window_corner_radius = CornerRadius::same(5);
    v.menu_corner_radius = CornerRadius::same(4);
    v.window_shadow = egui::Shadow {
        offset: [0, 2],
        blur: 10,
        spread: 0,
        color: Color32::from_black_alpha(p.shadow),
    };
    v.popup_shadow = egui::Shadow {
        offset: [0, 1],
        blur: 6,
        spread: 0,
        color: Color32::from_black_alpha(p.shadow),
    };
    v.selection.bg_fill = p.selection;
    v.selection.stroke = Stroke::new(1.0, p.accent);
    v.hyperlink_color = p.accent;

    v.widgets.noninteractive.bg_fill = p.panel;
    v.widgets.noninteractive.weak_bg_fill = p.panel;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.line);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    v.widgets.noninteractive.corner_radius = radius;

    v.widgets.inactive.bg_fill = p.control;
    v.widgets.inactive.weak_bg_fill = p.control;
    v.widgets.inactive.bg_stroke = Stroke::NONE;
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, p.text);
    v.widgets.inactive.corner_radius = radius;

    v.widgets.hovered.bg_fill = p.control_hover;
    v.widgets.hovered.weak_bg_fill = p.control_hover;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, p.line_strong);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, p.text_strong);
    v.widgets.hovered.corner_radius = radius;
    v.widgets.hovered.expansion = 0.0;

    v.widgets.active.bg_fill = p.control_active;
    v.widgets.active.weak_bg_fill = p.control_active;
    v.widgets.active.bg_stroke = Stroke::new(1.0, p.accent);
    v.widgets.active.fg_stroke = Stroke::new(1.0, p.text_strong);
    v.widgets.active.corner_radius = radius;
    v.widgets.active.expansion = 0.0;

    v.widgets.open.bg_fill = p.control_hover;
    v.widgets.open.weak_bg_fill = p.control_hover;
    v.widgets.open.bg_stroke = Stroke::new(1.0, p.line_strong);
    v.widgets.open.fg_stroke = Stroke::new(1.0, p.text_strong);
    v.widgets.open.corner_radius = radius;
    v
}

struct Scheme {
    panel: Color32,
    raised: Color32,
    field: Color32,
    faint: Color32,
    line: Color32,
    line_strong: Color32,
    text: Color32,
    text_strong: Color32,
    control: Color32,
    control_hover: Color32,
    control_active: Color32,
    selection: Color32,
    accent: Color32,
    shadow: u8,
}

/// The light theme.
pub fn light_visuals() -> Visuals {
    base(
        Visuals::light(),
        &Scheme {
            panel: Color32::from_rgb(0xF5, 0xF5, 0xF2),
            raised: Color32::from_rgb(0xFC, 0xFC, 0xFA),
            field: Color32::WHITE,
            faint: Color32::from_rgb(0xEE, 0xEE, 0xEA),
            line: Color32::from_rgb(0xD8, 0xD8, 0xD3),
            line_strong: Color32::from_rgb(0xB7, 0xB9, 0xB3),
            text: Color32::from_rgb(0x2E, 0x30, 0x33),
            text_strong: Color32::from_rgb(0x14, 0x15, 0x17),
            control: Color32::from_rgb(0xE9, 0xE9, 0xE5),
            control_hover: Color32::from_rgb(0xDD, 0xDE, 0xD9),
            control_active: Color32::from_rgb(0xD0, 0xD3, 0xCE),
            selection: Color32::from_rgb(0xC6, 0xDB, 0xE3),
            accent: Color32::from_rgb(0x3A, 0x69, 0x7C),
            shadow: 28,
        },
    )
}

/// The dark theme.
pub fn dark_visuals() -> Visuals {
    base(
        Visuals::dark(),
        &Scheme {
            panel: Color32::from_rgb(0x20, 0x22, 0x24),
            raised: Color32::from_rgb(0x28, 0x2A, 0x2D),
            field: Color32::from_rgb(0x17, 0x18, 0x1A),
            faint: Color32::from_rgb(0x27, 0x29, 0x2B),
            line: Color32::from_rgb(0x36, 0x39, 0x3C),
            line_strong: Color32::from_rgb(0x50, 0x54, 0x58),
            text: Color32::from_rgb(0xD4, 0xD6, 0xD2),
            text_strong: Color32::from_rgb(0xF0, 0xF1, 0xEE),
            control: Color32::from_rgb(0x2F, 0x32, 0x35),
            control_hover: Color32::from_rgb(0x3A, 0x3E, 0x42),
            control_active: Color32::from_rgb(0x46, 0x4B, 0x50),
            selection: Color32::from_rgb(0x2C, 0x4C, 0x59),
            accent: Color32::from_rgb(0x82, 0xB4, 0xC6),
            shadow: 90,
        },
    )
}

/// Gives the scroll bars egui draws for a scroll area an accessible name (they have none of
/// their own). `area` is the scroll area's id, as its output reports it; call this after the
/// area has been shown.
pub fn name_scroll_bars(ctx: &egui::Context, area: egui::Id, what: &str) {
    for (axis, name) in ["horizontal", "vertical"].into_iter().enumerate() {
        let bar = area.with(axis);
        if ctx.has_accesskit_node(bar) {
            ctx.accesskit_node_builder(bar, |node| {
                node.set_label(format!("{what} {name} scroll bar"));
            });
        }
    }
}

/// Colours for what the app paints itself (the canvas and its overlays), by theme.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    /// Behind the pages.
    pub canvas: Color32,
    pub page_edge: Color32,
    /// A page whose pixels have not arrived yet. Paper is white in both themes.
    pub paper: Color32,
    pub hit: Color32,
    pub hit_current: Color32,
    pub hit_current_edge: Color32,
    pub selection: Color32,
    pub accent: Color32,
    pub scroll_thumb: Color32,
    pub muted_text: Color32,
    /// A message that something was refused.
    pub warning: Color32,
    /// A selected row of a list.
    pub row_selected: Color32,
    /// The heading line of a group in a list.
    pub row_group: Color32,
}

impl Palette {
    pub fn of(visuals: &Visuals) -> Palette {
        if visuals.dark_mode {
            Palette {
                canvas: Color32::from_rgb(0x15, 0x16, 0x17),
                page_edge: Color32::from_rgb(0x3C, 0x3F, 0x42),
                paper: Color32::WHITE,
                hit: Color32::from_rgba_unmultiplied(0xF2, 0xC1, 0x3C, 80),
                hit_current: Color32::from_rgba_unmultiplied(0xF0, 0x8A, 0x1C, 120),
                hit_current_edge: Color32::from_rgb(0xC9, 0x6A, 0x0A),
                selection: Color32::from_rgba_unmultiplied(0x3A, 0x7E, 0x9C, 90),
                accent: Color32::from_rgb(0x82, 0xB4, 0xC6),
                scroll_thumb: Color32::from_rgba_unmultiplied(0xC8, 0xCA, 0xC6, 90),
                muted_text: grey(0x8E),
                warning: Color32::from_rgb(0xE2, 0xA2, 0x5A),
                row_selected: Color32::from_rgba_unmultiplied(0x3A, 0x7E, 0x9C, 70),
                row_group: Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 10),
            }
        } else {
            Palette {
                canvas: Color32::from_rgb(0xE2, 0xE3, 0xDF),
                page_edge: Color32::from_rgb(0xC2, 0xC4, 0xBE),
                paper: Color32::WHITE,
                hit: Color32::from_rgba_unmultiplied(0xF2, 0xC1, 0x3C, 80),
                hit_current: Color32::from_rgba_unmultiplied(0xF0, 0x8A, 0x1C, 120),
                hit_current_edge: Color32::from_rgb(0xC9, 0x6A, 0x0A),
                selection: Color32::from_rgba_unmultiplied(0x3A, 0x7E, 0x9C, 90),
                accent: Color32::from_rgb(0x3A, 0x69, 0x7C),
                scroll_thumb: Color32::from_rgba_unmultiplied(0x40, 0x43, 0x46, 90),
                muted_text: grey(0x70),
                warning: Color32::from_rgb(0x9A, 0x45, 0x0C),
                row_selected: Color32::from_rgba_unmultiplied(0x3A, 0x7E, 0x9C, 56),
                row_group: Color32::from_rgba_unmultiplied(0x00, 0x00, 0x00, 12),
            }
        }
    }
}
