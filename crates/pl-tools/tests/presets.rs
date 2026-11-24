//! Tool presets: built-in styles, the stored JSON form, and the library of named presets.

use pl_annot::{AnnotKind, BlendMode, LineEnding, Rgb, Style};
use pl_tools::{PresetLibrary, ToolKind, ToolPreset};

fn custom() -> ToolPreset {
    ToolPreset {
        name: "Demolition".into(),
        tool: ToolKind::Cloud,
        subject: "Demo".into(),
        style: Style {
            stroke: Some(Rgb::new(0.0, 0.3, 0.9)),
            fill: Some(Rgb::new(0.9, 0.95, 1.0)),
            width: 3.5,
            dash: Some(vec![6.0, 3.0]),
            opacity: 0.6,
            line_start: LineEnding::Circle,
            line_end: LineEnding::ClosedArrow,
            cloud: Some(2.0),
            blend: BlendMode::Multiply,
        },
        ..ToolPreset::default_for(ToolKind::Cloud)
    }
}

#[test]
fn built_in_styles() {
    for tool in ToolKind::ALL {
        let p = ToolPreset::default_for(tool);
        assert_eq!(p.tool, tool);
        assert_eq!(p.name, "");
        assert_eq!(p.subject, tool.label());
        assert_eq!(p.style.opacity, 1.0);
        // Typewriter text has no border; everything else draws with a line.
        assert_eq!(
            p.style.width > 0.0,
            tool != ToolKind::Typewriter,
            "{tool:?}"
        );
        assert_eq!(p.stamp.is_some(), tool == ToolKind::Stamp, "{tool:?}");
        if let Some(kind) = tool.annot_kind() {
            // A FreeText or a Stamp is modelled by what it holds, not by its kind.
            assert!(
                kind.is_modelled() || matches!(kind, AnnotKind::FreeText | AnnotKind::Stamp),
                "{tool:?} draws something we can write"
            );
            // A dimension line has closed arrows filled with its colour.
            let dimension = matches!(
                tool,
                ToolKind::Length | ToolKind::Radius | ToolKind::Diameter
            );
            let special = dimension
                || matches!(
                    tool,
                    ToolKind::Arrow
                        | ToolKind::Cloud
                        | ToolKind::Pen
                        | ToolKind::Highlighter
                        | ToolKind::Typewriter
                        | ToolKind::Callout
                        | ToolKind::CloudBox
                        | ToolKind::CloudCallout
                        | ToolKind::Stamp
                        | ToolKind::Image
                );
            assert_eq!(p.style == Style::default_for(&kind), !special, "{tool:?}");
        }
    }
    let style = |t| ToolPreset::default_for(t).style;
    assert_eq!(ToolKind::Select.annot_kind(), None);
    assert_eq!(ToolKind::Eraser.annot_kind(), None);
    assert_eq!(ToolKind::ALL.len(), 35);
    assert_eq!(style(ToolKind::Callout).line_end, LineEnding::OpenArrow);
    assert_eq!(style(ToolKind::CloudBox).cloud, Some(1.0));
    assert_eq!(style(ToolKind::CloudCallout).cloud, Some(1.0));
    assert_eq!(style(ToolKind::Typewriter).stroke, None);
    assert_eq!(style(ToolKind::Stamp).stroke, None);
    assert_eq!(style(ToolKind::Image).stroke, None);
    assert_eq!(
        style(ToolKind::TextHighlight).blend,
        Style::default_for(&AnnotKind::Highlight).blend
    );
    assert_eq!(
        ToolPreset::default_for(ToolKind::Typewriter).text.padding,
        1.0
    );
    assert_eq!(ToolPreset::default_for(ToolKind::TextBox).text.padding, 3.0);
    assert_eq!(ToolPreset::default_for(ToolKind::TextBox).text.size, 12.0);
    assert_eq!(
        ToolPreset::default_for(ToolKind::Stamp).stamp,
        Some(pl_annot::StampSpec::approved())
    );
    assert_eq!(ToolKind::Cloud.annot_kind(), Some(AnnotKind::Polygon));
    assert_eq!(ToolKind::Highlighter.annot_kind(), Some(AnnotKind::Ink));
    assert_eq!(style(ToolKind::Arrow).line_end, LineEnding::OpenArrow);
    assert_eq!(style(ToolKind::Arrow).line_start, LineEnding::None);
    assert_eq!(style(ToolKind::Cloud).cloud, Some(1.0));
    assert_eq!(style(ToolKind::Polygon).cloud, None);
    assert_eq!(style(ToolKind::Pen).width, 2.0);
    assert_eq!(style(ToolKind::Highlighter).blend, BlendMode::Multiply);
    assert_eq!(style(ToolKind::Highlighter).width, 12.0);
    assert_eq!(style(ToolKind::Pen).blend, BlendMode::Normal);
}

#[test]
fn the_stored_form_is_stable() {
    let json = serde_json::to_string(&ToolPreset::default_for(ToolKind::Arrow)).unwrap();
    assert_eq!(
        json,
        r#"{"name":"","tool":"Arrow","subject":"Arrow","stroke":[0.8,0.0,0.0],"fill":null,"width":1.0,"dash":null,"opacity":1.0,"line_start":"None","line_end":"OpenArrow","cloud":null,"blend":"Normal"}"#
    );
    let json = serde_json::to_string(&custom()).unwrap();
    assert_eq!(
        json,
        r#"{"name":"Demolition","tool":"Cloud","subject":"Demo","stroke":[0.0,0.3,0.9],"fill":[0.9,0.95,1.0],"width":3.5,"dash":[6.0,3.0],"opacity":0.6,"line_start":"Circle","line_end":"ClosedArrow","cloud":2.0,"blend":"Multiply"}"#
    );
}

#[test]
fn presets_round_trip_exactly() {
    let mut all: Vec<ToolPreset> = ToolKind::ALL
        .iter()
        .map(|t| ToolPreset::default_for(*t))
        .collect();
    all.push(custom());
    for ending in LineEnding::ALL {
        let mut p = ToolPreset::default_for(ToolKind::Line);
        p.style.line_start = ending;
        p.style.line_end = ending;
        // A colour that is not a round number in binary or decimal.
        p.style.stroke = Some(Rgb::new(1.0 / 3.0, 0.1, 0.7));
        p.style.width = 0.1 + 0.2;
        all.push(p);
    }
    for p in all {
        let json = serde_json::to_string_pretty(&p).unwrap();
        let back: ToolPreset = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p, "{json}");
    }
}

#[test]
fn a_sparse_record_gets_defaults_and_a_bad_one_is_refused() {
    let p: ToolPreset = serde_json::from_str(r#"{"tool":"Pen","width":3}"#).unwrap();
    assert_eq!(p.tool, ToolKind::Pen);
    assert_eq!(p.name, "");
    assert_eq!(p.subject, "Pen");
    assert_eq!(
        p.style,
        Style {
            stroke: None,
            fill: None,
            width: 3.0,
            dash: None,
            opacity: 1.0,
            line_start: LineEnding::None,
            line_end: LineEnding::None,
            cloud: None,
            blend: BlendMode::Normal,
        }
    );
    // Out-of-range colours and opacity are clamped; an empty dash is no dash.
    let p: ToolPreset = serde_json::from_str(
        r#"{"tool":"Line","width":1,"stroke":[2.0,-1.0,0.5],"opacity":7,"dash":[]}"#,
    )
    .unwrap();
    assert_eq!(p.style.stroke, Some(Rgb::new(1.0, 0.0, 0.5)));
    assert_eq!(p.style.opacity, 1.0);
    assert_eq!(p.style.dash, None);

    let refuse = |json: &str, why: &str| {
        let err = serde_json::from_str::<ToolPreset>(json).expect_err(json);
        assert!(err.to_string().contains(why), "{err}");
    };
    refuse(
        r#"{"tool":"Line","width":1,"line_end":"Fancy"}"#,
        "unknown line ending",
    );
    refuse(
        r#"{"tool":"Line","width":1,"blend":"Screen"}"#,
        "unknown blend mode",
    );
    refuse(r#"{"tool":"Line","width":-1}"#, "width");
    refuse(r#"{"tool":"Line","width":1,"dash":[3,-2]}"#, "dash");
    refuse(r#"{"tool":"Cloud","width":1,"cloud":0}"#, "cloud");
    refuse(r#"{"tool":"Lasso","width":1}"#, "unknown variant");
    refuse(r#"{"tool":"Line"}"#, "width");
}

#[test]
fn the_library_keeps_a_current_style_per_tool_and_named_presets() {
    let mut lib = PresetLibrary::new();
    for tool in ToolKind::ALL {
        assert_eq!(lib.current(tool), ToolPreset::default_for(tool));
        assert_eq!(lib.named(tool).count(), 0);
    }
    // A current style replaces the built-in one, for its tool only.
    let mut thick = ToolPreset::default_for(ToolKind::Rectangle);
    thick.style.width = 4.0;
    lib.set_current(thick.clone());
    assert_eq!(lib.current(ToolKind::Rectangle), thick);
    assert_eq!(
        lib.current(ToolKind::Ellipse),
        ToolPreset::default_for(ToolKind::Ellipse)
    );
    let mut thicker = thick.clone();
    thicker.style.width = 6.0;
    lib.set_current(thicker.clone());
    assert_eq!(lib.current(ToolKind::Rectangle), thicker);

    // Named presets: unique by tool and name, kept in the order saved.
    assert!(
        !lib.save(thick.clone()),
        "a preset needs a name to be saved"
    );
    assert!(lib.save(custom()));
    let hatch = ToolPreset::named(
        "Hatch",
        ToolKind::Cloud,
        Style::default_for(&AnnotKind::Polygon),
    );
    assert!(lib.save(hatch.clone()));
    let same_name_other_tool =
        ToolPreset::named("Demolition", ToolKind::Rectangle, thick.style.clone());
    assert!(lib.save(same_name_other_tool.clone()));
    let names: Vec<&str> = lib
        .named(ToolKind::Cloud)
        .map(|p| p.name.as_str())
        .collect();
    assert_eq!(names, ["Demolition", "Hatch"]);
    assert_eq!(lib.named(ToolKind::Rectangle).count(), 1);
    // Saving under a name again replaces it in place.
    let mut revised = custom();
    revised.style.width = 1.0;
    assert!(lib.save(revised.clone()));
    assert_eq!(lib.named(ToolKind::Cloud).count(), 2);
    assert_eq!(lib.find(ToolKind::Cloud, "Demolition"), Some(&revised));
    assert_eq!(
        lib.find(ToolKind::Rectangle, "Demolition"),
        Some(&same_name_other_tool)
    );
    assert_eq!(lib.find(ToolKind::Line, "Demolition"), None);

    // Activating a named preset makes it the tool's current style.
    assert_eq!(lib.activate(ToolKind::Cloud, "Hatch"), Some(hatch.clone()));
    assert_eq!(lib.current(ToolKind::Cloud), hatch);
    assert_eq!(lib.activate(ToolKind::Cloud, "Missing"), None);
    assert_eq!(lib.current(ToolKind::Cloud), hatch);

    // The whole library round-trips.
    let json = serde_json::to_string(&lib).unwrap();
    let back: PresetLibrary = serde_json::from_str(&json).unwrap();
    assert_eq!(back, lib);
    assert_eq!(
        serde_json::from_str::<PresetLibrary>("{}").unwrap(),
        PresetLibrary::new()
    );

    assert!(lib.remove(ToolKind::Cloud, "Demolition"));
    assert!(!lib.remove(ToolKind::Cloud, "Demolition"));
    assert_eq!(lib.named(ToolKind::Cloud).count(), 1);
    assert_eq!(
        lib.named(ToolKind::Rectangle).count(),
        1,
        "the other tool's stays"
    );
    lib.reset(ToolKind::Rectangle);
    assert_eq!(
        lib.current(ToolKind::Rectangle),
        ToolPreset::default_for(ToolKind::Rectangle)
    );
}

#[test]
fn a_preset_styles_an_annotation_and_names_its_subject() {
    let origin = pl_annot::Origin::new("T", pl_annot::PdfDate::from_unix(1_700_000_000));
    let mut a = pl_annot::Annotation::polygon(0, vec![], &origin);
    custom().apply_to(&mut a);
    assert_eq!(a.style, custom().style);
    assert_eq!(a.meta.subject.as_deref(), Some("Demo"));
    // A link is not a markup and has no subject to set.
    let mut link = pl_annot::Annotation::link(
        0,
        pl_geom::Rect::new(0.0, 0.0, 1.0, 1.0),
        pl_annot::LinkTarget::Uri("https://example.com".into()),
        &origin,
    );
    custom().apply_to(&mut link);
    assert_eq!(link.meta.subject, None);
}

#[test]
fn text_and_stamp_presets_round_trip_and_bad_ones_are_refused() {
    use pl_annot::{FontStyle, HAlign, StampScheme, StampSpec, VAlign};
    use pl_tools::{PresetError, TextPreset};

    let mut text = ToolPreset::default_for(ToolKind::TextBox);
    text.name = "Site note".into();
    text.style.fill = Some(Rgb::new(1.0, 1.0, 0.8));
    text.style.width = 0.5;
    text.text = TextPreset {
        font: FontStyle::BoldItalic,
        size: 9.5,
        color: Rgb::new(0.0, 0.25, 0.5),
        align: HAlign::Centre,
        v_align: VAlign::Middle,
        line_spacing: 1.5,
        padding: 2.0,
    };
    let json = serde_json::to_string(&text).unwrap();
    assert!(
        json.ends_with(
            r#""text":{"font":"BoldItalic","size":9.5,"color":[0.0,0.25,0.5],"align":"Centre","v_align":"Middle","line_spacing":1.5,"padding":2.0}}"#
        ),
        "{json}"
    );
    assert_eq!(serde_json::from_str::<ToolPreset>(&json).unwrap(), text);

    // Every tool's built-in preset survives the round trip, text and stamp included.
    for tool in ToolKind::ALL {
        let p = ToolPreset::default_for(tool);
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(
            serde_json::from_str::<ToolPreset>(&json).unwrap(),
            p,
            "{tool:?}"
        );
        assert_eq!(json.contains("\"text\""), tool.is_text(), "{tool:?}");
        assert_eq!(
            json.contains("\"stamp\""),
            tool == ToolKind::Stamp,
            "{tool:?}"
        );
    }

    let stamp = ToolPreset::for_stamp(
        StampSpec::new("Checked", "CHECKED", StampScheme::Blue).with_detail("by survey"),
    );
    let json = serde_json::to_string(&stamp).unwrap();
    assert!(
        json.ends_with(
            r#""stamp":{"name":"Checked","label":"CHECKED","detail":"by survey","scheme":"Blue"}}"#
        ),
        "{json}"
    );
    assert_eq!(serde_json::from_str::<ToolPreset>(&json).unwrap(), stamp);

    // A text tool stored before it had text settings reads with the tool's defaults.
    let sparse: ToolPreset = serde_json::from_str(r#"{"tool":"Typewriter","width":0.0}"#).unwrap();
    assert_eq!(sparse.text, TextPreset::default_for(ToolKind::Typewriter));
    let sparse: ToolPreset = serde_json::from_str(r#"{"tool":"Stamp","width":1.0}"#).unwrap();
    assert_eq!(sparse.stamp, Some(StampSpec::approved()));

    let bad = |text: &str| {
        let json = format!(r#"{{"tool":"TextBox","width":1.0,"text":{text}}}"#);
        serde_json::from_str::<ToolPreset>(&json)
            .unwrap_err()
            .to_string()
    };
    let unknown_font = bad(r#"{"font":"Gothic","size":12.0,"color":[0,0,0]}"#);
    assert!(
        unknown_font.contains(&PresetError::UnknownFont("Gothic".into()).to_string()),
        "{unknown_font}"
    );
    assert!(bad(r#"{"size":0.0,"color":[0,0,0]}"#).contains("\"size\""));
    assert!(bad(r#"{"size":12.0,"color":[0,0,0],"align":"Justify"}"#).contains("Justify"));
    assert!(bad(r#"{"size":12.0,"color":[0,0,0],"padding":-1.0}"#).contains("\"padding\""));
    let bad_stamp = serde_json::from_str::<ToolPreset>(
        r#"{"tool":"Stamp","width":1.0,"stamp":{"name":"X","label":"X","scheme":"Pink"}}"#,
    )
    .unwrap_err()
    .to_string();
    assert!(bad_stamp.contains("Pink"), "{bad_stamp}");
}

#[test]
fn a_text_preset_sets_the_text_of_a_free_text_and_nothing_else() {
    use pl_annot::{Annotation, FontStyle, HAlign, Origin, PdfDate};
    use pl_geom::{Point, Rect};

    let origin = Origin::new("Tester", PdfDate::from_unix(1_700_000_000));
    let rect = Rect::new(10.0, 10.0, 110.0, 60.0);
    let mut a = Annotation::callout(0, rect, Point::new(200.0, 200.0), "hello", &origin);
    a.set_rotation(90);
    let leader = a.free_text.as_ref().unwrap().callout.clone();
    let mut p = ToolPreset::default_for(ToolKind::Callout);
    p.text.font = FontStyle::Bold;
    p.text.size = 18.0;
    p.text.align = HAlign::Right;
    p.text.color = Rgb::new(0.1, 0.2, 0.3);
    p.apply_to(&mut a);
    let t = a.free_text.as_ref().unwrap();
    assert_eq!(
        (t.font, t.size, t.align, t.color),
        (
            FontStyle::Bold,
            18.0,
            HAlign::Right,
            Rgb::new(0.1, 0.2, 0.3)
        )
    );
    assert_eq!((t.callout.clone(), t.rotation), (leader, 90));
    assert_eq!(a.style, p.style);
    assert_eq!(a.meta.subject.as_deref(), Some("Callout"));
    assert_eq!(a.meta.contents.as_deref(), Some("hello"));
}
