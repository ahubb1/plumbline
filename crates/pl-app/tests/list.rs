//! Headless tests of the markups list (M5): rows, sort, group, filter, in-list editing, reply
//! rows, selection both ways, export and import, persistence, and that a very long list costs
//! a frame no more than a short one.

mod support;

use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui::Key;
use egui_kittest::kittest::Queryable;
use pl_app::{
    Command, ExportKind, ListLineView, ScriptedDialogs, Tool, LIST_FILE_NAME, ROW_HEIGHT,
};
use pl_doc::pl_annot::{AnnotId, Annotation, Origin, PdfDate, ReviewState};
use pl_doc::pl_geom::{Point, Rect};
use pl_doc::pl_takeoff::{
    custom_value, import_xfdf, Builtin, ColumnId, ColumnType, Condition, GroupKey, SortKey, Value,
};
use support::*;

fn origin(author: &str) -> Origin {
    Origin::new(author, PdfDate::from_unix(1_700_000_000))
}

fn square(page: usize, x: f64, y: f64, subject: &str, comment: &str, author: &str) -> Annotation {
    let mut a = Annotation::square(page, Rect::new(x, y, x + 60.0, y + 40.0), &origin(author));
    a.meta.subject = Some(subject.to_owned());
    a.meta.contents = (!comment.is_empty()).then(|| comment.to_owned());
    a
}

/// Four markups of three kinds by two authors, in this order.
fn sample() -> Vec<Annotation> {
    let mut circle = Annotation::circle(0, Rect::new(200.0, 600.0, 260.0, 660.0), &origin("Ben"));
    circle.meta.subject = Some("Column".to_owned());
    circle.meta.contents = Some("Column C1 grid 4".to_owned());
    let mut line = Annotation::line(
        0,
        Point::new(300.0, 600.0),
        Point::new(380.0, 650.0),
        &origin("Ada"),
    );
    line.meta.subject = Some("Setout".to_owned());
    vec![
        square(0, 100.0, 600.0, "Beam 10", "Beam B10 over door", "Ada"),
        circle,
        line,
        square(0, 100.0, 500.0, "Beam 2", "", "Ben"),
    ]
}

/// Shows only these built-in columns (and every custom one), in this order.
fn show_columns(h: &mut App, columns: &[Builtin]) {
    let wanted: Vec<ColumnId> = columns.iter().map(|b| ColumnId::Builtin(*b)).collect();
    h.state_mut().edit_list_layout(|layout| {
        for state in &mut layout.columns {
            state.visible =
                wanted.contains(&state.column) || matches!(state.column, ColumnId::Custom(_));
        }
        for column in wanted.iter().rev() {
            let first = layout.columns.first().map(|c| c.column.clone());
            layout.move_column(column, first.as_ref());
        }
    });
    h.run();
}

/// A mock app with the list open on `markups`.
fn list_app(pages: usize, dt: f32, markups: Vec<Annotation>) -> App {
    let mut h = mock_app_with(pages, dt, None, ScriptedDialogs::default());
    assert!(h.state_mut().add_markups(markups));
    h.state_mut().set_markups_list_open(true);
    settle(&mut h);
    h
}

fn rows(h: &mut App) -> Vec<ListLineView> {
    h.state_mut().list_lines()
}

/// The text of one column down the list, group headings and totals left out.
fn column(h: &mut App, column: Builtin) -> Vec<String> {
    let at = h
        .state()
        .list_layout()
        .visible()
        .iter()
        .position(|c| *c == ColumnId::Builtin(column))
        .expect("the column is visible");
    rows(h)
        .into_iter()
        .filter(|line| line.id.is_some())
        .map(|line| line.cells[at].clone())
        .collect()
}

fn id_of(h: &App, subject: &str) -> AnnotId {
    h.state()
        .markups()
        .into_iter()
        .find(|a| a.meta.subject.as_deref() == Some(subject))
        .unwrap_or_else(|| panic!("no markup called {subject}"))
        .id
}

fn cell_centre(h: &App, name: &str) -> egui::Pos2 {
    h.get_by_label(name).rect().center()
}

#[test]
fn the_list_has_a_row_per_markup_and_sorts_groups_and_filters() {
    let mut h = list_app(1, 0.25, sample());
    show_columns(
        &mut h,
        &[
            Builtin::Subject,
            Builtin::Kind,
            Builtin::Author,
            Builtin::Comment,
        ],
    );
    // One row per markup, in the document's order, each cell on screen under its heading.
    assert_eq!(
        column(&mut h, Builtin::Subject),
        ["Beam 10", "Column", "Setout", "Beam 2"]
    );
    assert_eq!(
        column(&mut h, Builtin::Kind),
        ["Rectangle", "Ellipse", "Line", "Rectangle"]
    );
    assert_eq!(
        column(&mut h, Builtin::Author),
        ["Ada", "Ben", "Ada", "Ben"]
    );
    h.get_by_label("Subject Beam 10, row 1");
    h.get_by_label("Type Ellipse, row 2");
    h.get_by_label("Comment Column C1 grid 4, row 2");
    h.get_by_label("Author Ben, row 4");
    assert_all_named(&h, "the list");

    // A click on a heading sorts by it (natural order: Beam 2 before Beam 10); another turns
    // it round; a third takes the sort off.
    click(&mut h, "Sort by Subject");
    assert_eq!(
        column(&mut h, Builtin::Subject),
        ["Beam 2", "Beam 10", "Column", "Setout"]
    );
    assert_eq!(
        h.state().list_layout().sort,
        [SortKey::asc(Builtin::Subject)]
    );
    click(&mut h, "Sort by Subject, now ascending");
    assert_eq!(
        column(&mut h, Builtin::Subject),
        ["Setout", "Column", "Beam 10", "Beam 2"]
    );
    click(&mut h, "Sort by Subject, now descending");
    assert!(h.state().list_layout().sort.is_empty());
    assert_eq!(
        column(&mut h, Builtin::Subject),
        ["Beam 10", "Column", "Setout", "Beam 2"]
    );
    // Shift adds a second key: by author, then by subject.
    click(&mut h, "Sort by Author");
    let heading = cell_centre(&h, "Sort by Subject");
    click_at_with(&mut h, heading, SHIFT);
    h.remove_cursor();
    h.run();
    assert_eq!(h.state().list_layout().sort.len(), 2);
    assert_eq!(
        column(&mut h, Builtin::Subject),
        ["Beam 10", "Setout", "Beam 2", "Column"]
    );
    // A plain click on a heading makes it the only key again.
    click(&mut h, "Sort by Author, now ascending");
    assert_eq!(
        h.state().list_layout().sort,
        [SortKey::asc(Builtin::Author)]
    );
    click(&mut h, "Sort by Author, now ascending");
    click(&mut h, "Sort by Author, now descending");
    assert!(h.state().list_layout().sort.is_empty());

    // Group by type: a heading line per group with its count, the rows under it.
    click_label(&mut h, "Group by");
    click(&mut h, "Type");
    assert_eq!(
        h.state().list_layout().group,
        [GroupKey::asc(Builtin::Kind)]
    );
    let lines = rows(&mut h);
    let shape: Vec<(Option<&str>, usize)> = lines
        .iter()
        .map(|l| (l.group.as_deref(), l.count))
        .collect();
    assert_eq!(
        shape,
        [
            (Some("Type: Ellipse"), 1),
            (None, 1),
            (Some("Type: Line"), 1),
            (None, 1),
            (Some("Type: Rectangle"), 2),
            (None, 1),
            (None, 1),
        ]
    );
    // A click on a heading line closes the group; another opens it.
    click_label(&mut h, "Group Type: Rectangle, 2 markups, open");
    assert_eq!(rows(&mut h).len(), 5);
    assert_eq!(h.state().list_layout().collapsed.len(), 1);
    h.get_by_label("Group Type: Rectangle, 2 markups, closed");
    click_label(&mut h, "Group Type: Rectangle, 2 markups, closed");
    assert_eq!(rows(&mut h).len(), 7);
    click_label(&mut h, "Group by");
    click(&mut h, "No grouping");
    assert!(h.state().list_layout().group.is_empty());

    // The search box looks in every column; the count says how many are left.
    h.get_by_label("Search markups").focus();
    h.run();
    type_into(&mut h, "Search markups", "beam");
    assert_eq!(column(&mut h, Builtin::Subject), ["Beam 10", "Beam 2"]);
    h.get_by_label("2 of 4");
    type_into(&mut h, "Search markups", " door");
    assert_eq!(column(&mut h, Builtin::Subject), ["Beam 10"]);
    h.state_mut()
        .edit_list_layout(|layout| layout.filter.search.clear());
    h.run();

    // The filter row filters one column each.
    click(&mut h, "Filter row");
    h.get_by_label("Filter Author").focus();
    h.run();
    type_into(&mut h, "Filter Author", "ben");
    assert_eq!(column(&mut h, Builtin::Subject), ["Column", "Beam 2"]);
    h.get_by_label("Filter Type").focus();
    h.run();
    type_into(&mut h, "Filter Type", "rect");
    assert_eq!(column(&mut h, Builtin::Subject), ["Beam 2"]);
    let conditions = &h.state().list_layout().filter.conditions;
    assert_eq!(conditions.len(), 2);
    assert!(conditions
        .iter()
        .any(|c| c.condition == Condition::Contains("ben".to_owned())));
    assert_all_named(&h, "the list with its filter row");
}

#[test]
fn columns_are_shown_hidden_moved_and_resized() {
    let mut h = list_app(1, 0.25, sample());
    show_columns(&mut h, &[Builtin::Subject, Builtin::Kind, Builtin::Author]);
    let visible = |h: &App| -> Vec<ColumnId> { h.state().list_layout().visible() };
    let b = ColumnId::Builtin;
    assert_eq!(
        visible(&h),
        [b(Builtin::Subject), b(Builtin::Kind), b(Builtin::Author)]
    );

    // The Columns menu ticks columns on and off and moves them.
    click(&mut h, "Columns");
    assert_all_named(&h, "the Columns menu");
    h.get_by_role_and_label(Role::CheckBox, "Author").click();
    h.run();
    assert_eq!(visible(&h), [b(Builtin::Subject), b(Builtin::Kind)]);
    h.get_by_role_and_label(Role::CheckBox, "Page").click();
    h.run();
    assert!(visible(&h).contains(&b(Builtin::Page)));
    click(&mut h, "Move Type earlier");
    assert_eq!(visible(&h)[..2], [b(Builtin::Kind), b(Builtin::Subject)]);
    click(&mut h, "Move Type later");
    assert_eq!(visible(&h)[..2], [b(Builtin::Subject), b(Builtin::Kind)]);
    h.key_press(Key::Escape);
    h.run();
    assert_eq!(
        column(&mut h, Builtin::Kind),
        ["Rectangle", "Ellipse", "Line", "Rectangle"]
    );

    // Dragging a heading's right edge resizes its column, and the layout keeps the width.
    let before = h
        .state()
        .list_layout()
        .state(&b(Builtin::Subject))
        .unwrap()
        .width;
    let heading = h.get_by_label("Sort by Subject").rect();
    let table_left = h.get_by_label("Subject Beam 10, row 1").rect().left();
    let edge = egui::pos2(table_left + before, heading.center().y);
    drag(&mut h, edge, edge + egui::vec2(60.0, 0.0));
    h.run();
    let after = h
        .state()
        .list_layout()
        .state(&b(Builtin::Subject))
        .unwrap()
        .width;
    assert!((after - before - 60.0).abs() < 3.0, "{before} -> {after}");
}

#[test]
fn status_checkmark_comment_and_custom_columns_are_edited_in_the_list_and_undo() {
    let mut h = list_app(1, 0.04, sample());
    show_columns(
        &mut h,
        &[
            Builtin::Subject,
            Builtin::Comment,
            Builtin::Status,
            Builtin::Checkmark,
        ],
    );
    let beam = id_of(&h, "Beam 10");

    // Status: a drop-down in the row.
    click_label(&mut h, "Status, row 1");
    click(&mut h, "Accepted");
    let doc = h.state().active_document().unwrap();
    assert_eq!(doc.review_state(&beam), ReviewState::Accepted);
    assert_eq!(column(&mut h, Builtin::Status)[0], "Accepted");
    // It is a status annotation in the document, not a row of its own.
    assert_eq!(rows(&mut h).len(), 4);
    h.key_press_modifiers(CTRL, Key::Z);
    h.run();
    let doc = h.state().active_document().unwrap();
    assert_eq!(doc.review_state(&beam), ReviewState::None);
    assert_eq!(column(&mut h, Builtin::Status)[0], "");

    // Checkmark: a box in the row.
    h.get_by_role_and_label(Role::CheckBox, "Checked, row 1")
        .click();
    h.run();
    assert!(h.state().active_document().unwrap().is_checked(&beam));
    h.key_press_modifiers(CTRL, Key::Z);
    h.run();
    assert!(!h.state().active_document().unwrap().is_checked(&beam));

    // Comment: a double-click edits the cell in place; Enter keeps it.
    let cell = cell_centre(&h, "Comment Beam B10 over door, row 1");
    double_click_at(&mut h, cell);
    h.run();
    assert!(h.get_by_label("Edit Comment, row 1").is_focused());
    type_into(&mut h, "Edit Comment, row 1", ", checked");
    h.key_press(Key::Enter);
    h.run();
    h.run();
    let text = |h: &App| {
        h.state()
            .markups()
            .into_iter()
            .find(|a| a.id == beam)
            .unwrap()
            .meta
            .contents
    };
    assert_eq!(text(&h).as_deref(), Some("Beam B10 over door, checked"));
    h.key_press_modifiers(CTRL, Key::Z);
    h.run();
    assert_eq!(text(&h).as_deref(), Some("Beam B10 over door"));
    // Escape abandons an edit.
    let cell = cell_centre(&h, "Comment Beam B10 over door, row 1");
    double_click_at(&mut h, cell);
    h.run();
    type_into(&mut h, "Edit Comment, row 1", " nope");
    h.key_press(Key::Escape);
    h.run();
    assert!(h.query_by_label("Edit Comment, row 1").is_none());
    assert_eq!(text(&h).as_deref(), Some("Beam B10 over door"));

    // The custom columns dialog: a number column and a choice column.
    click(&mut h, "Columns");
    click_starting(&mut h, "Custom columns");
    h.get_by_label("Custom columns");
    assert_all_named(&h, "the custom columns dialog");
    h.get_by_label("New column name").focus();
    h.run();
    type_into(&mut h, "New column name", "Cost");
    click_label(&mut h, "New column type");
    click(&mut h, "Number");
    click(&mut h, "Add column");
    h.get_by_label("New column name").focus();
    h.run();
    type_into(&mut h, "New column name", "Trade");
    click_label(&mut h, "New column type");
    click(&mut h, "Choice");
    h.get_by_label("New column choices").focus();
    h.run();
    type_into(&mut h, "New column choices", "Civil, Drainage");
    click(&mut h, "Add column");
    // A second column of the same name is refused, with the reason in the dialog.
    h.get_by_label("New column name").focus();
    h.run();
    type_into(&mut h, "New column name", "cost");
    click(&mut h, "Add column");
    h.get_by_label("there is already a column called \"cost\"");
    assert_all_named(&h, "the custom columns dialog with columns");
    click(&mut h, "Close custom columns");
    let schema = h.state().list_schema().clone();
    let titles: Vec<(&str, ColumnType)> = schema
        .columns
        .iter()
        .map(|c| (c.title.as_str(), c.kind))
        .collect();
    assert_eq!(
        titles,
        [("Cost", ColumnType::Number), ("Trade", ColumnType::Choice)]
    );
    let cost = schema.columns[0].clone();
    let trade = schema.columns[1].clone();
    assert_eq!(trade.options, ["Civil", "Drainage"]);

    // A number is typed into the cell; it is stored in the markup.
    let value = |h: &App, column: &pl_doc::pl_takeoff::CustomColumn| {
        let a = h
            .state()
            .markups()
            .into_iter()
            .find(|a| a.id == beam)
            .unwrap();
        custom_value(&a, column)
    };
    let cell = cell_centre(&h, "Cost , row 1");
    double_click_at(&mut h, cell);
    h.run();
    type_into(&mut h, "Edit Cost, row 1", "125.5");
    h.key_press(Key::Enter);
    h.run();
    h.run();
    assert_eq!(value(&h, &cost), Value::Number(125.5));
    h.get_by_label("Cost 125.5, row 1");
    // Something that is not a number is refused and says so.
    let cell = cell_centre(&h, "Cost 125.5, row 1");
    double_click_at(&mut h, cell);
    h.run();
    type_into(&mut h, "Edit Cost, row 1", " each");
    h.key_press(Key::Enter);
    h.run();
    h.run();
    assert_eq!(value(&h, &cost), Value::Number(125.5));
    assert!(
        h.state()
            .status()
            .is_some_and(|s| s.contains("not taken") && s.contains("Cost")),
        "{:?}",
        h.state().status()
    );
    // A choice is picked from the cell's drop-down.
    click_label(&mut h, "Trade, row 1");
    click(&mut h, "Drainage");
    assert_eq!(value(&h, &trade), Value::text("Drainage"));
    // Each is one undo step.
    h.key_press_modifiers(CTRL, Key::Z);
    h.run();
    assert_eq!(value(&h, &trade), Value::Empty);
    assert_eq!(value(&h, &cost), Value::Number(125.5));
    h.key_press_modifiers(CTRL, Key::Z);
    h.run();
    assert_eq!(value(&h, &cost), Value::Empty);
    h.key_press_modifiers(CTRL, Key::Y);
    h.run();
    assert_eq!(value(&h, &cost), Value::Number(125.5));

    // Grouped, a number column shows a subtotal per group and a total.
    let second = cell_centre(&h, "Cost , row 4");
    double_click_at(&mut h, second);
    h.run();
    type_into(&mut h, "Edit Cost, row 4", "74.5");
    h.key_press(Key::Enter);
    h.run();
    h.run();
    h.state_mut().edit_list_layout(|layout| {
        layout.set_visible(&ColumnId::Builtin(Builtin::Kind), true);
        layout.group = vec![GroupKey::asc(Builtin::Kind)];
    });
    h.run();
    let at = h
        .state()
        .list_layout()
        .visible()
        .iter()
        .position(|c| *c == cost.column_id())
        .unwrap();
    let lines = rows(&mut h);
    let rectangles = lines
        .iter()
        .find(|l| l.group.as_deref() == Some("Type: Rectangle"))
        .expect("the group of rectangles");
    assert_eq!(rectangles.cells[at], "200");
    let total = lines.last().unwrap();
    assert!(total.id.is_none() && total.group.is_none());
    assert_eq!((total.count, total.cells[at].as_str()), (4, "200"));
    h.get_by_label("Total of Cost: 200");
}

#[test]
fn replies_are_child_rows_and_are_added_and_deleted_from_the_list() {
    let mut h = list_app(1, 0.25, sample());
    show_columns(
        &mut h,
        &[
            Builtin::Subject,
            Builtin::Kind,
            Builtin::Comment,
            Builtin::Replies,
        ],
    );
    let beam = id_of(&h, "Beam 10");
    click_label(&mut h, "Subject Beam 10, row 1");
    click(&mut h, "Add reply");
    h.run();
    assert!(h.get_by_label("Reply to the selected row").is_focused());
    assert_all_named(&h, "the reply strip");
    type_into(
        &mut h,
        "Reply to the selected row",
        "Is this the B10 on grid 4?",
    );
    click(&mut h, "Send reply");
    let lines = rows(&mut h);
    assert_eq!(lines.len(), 5);
    assert_eq!(lines[0].id.as_ref(), Some(&beam));
    assert_eq!(lines[1].reply_depth, 1, "the reply sits under its markup");
    assert_eq!(
        lines[1].cells,
        ["Re: Beam 10", "Reply", "Is this the B10 on grid 4?", "0"]
    );
    assert_eq!(lines[0].cells[3], "1", "the markup counts its replies");
    let doc = h.state().active_document().unwrap();
    let replies = doc.editor().unwrap().set().replies(&beam);
    assert_eq!(replies.len(), 1);
    let reply = replies[0].id.clone();
    assert_eq!(h.state().markups().len(), 5);

    // A reply to the reply nests one deeper.
    click_label(&mut h, "Comment Is this the B10 on grid 4?, row 2");
    click(&mut h, "Add reply");
    h.run();
    type_into(&mut h, "Reply to the selected row", "Yes");
    h.key_press(Key::Enter);
    h.run();
    h.run();
    let lines = rows(&mut h);
    assert_eq!(lines.len(), 6);
    assert_eq!(
        (lines[2].reply_depth, lines[2].cells[2].as_str()),
        (2, "Yes")
    );

    // The Replies switch hides them; the markup still counts them.
    click(&mut h, "Replies");
    assert!(!h.state().list_layout().show_replies);
    let lines = rows(&mut h);
    assert_eq!(lines.len(), 4);
    assert_eq!(lines[0].cells[3], "2");
    click(&mut h, "Replies");
    assert_eq!(rows(&mut h).len(), 6);

    // Deleting a selected reply row takes its own replies with it; undo brings them back.
    click_label(&mut h, "Comment Is this the B10 on grid 4?, row 2");
    assert!(rows(&mut h)[1].selected);
    click(&mut h, "Delete rows");
    let lines = rows(&mut h);
    assert_eq!(lines.len(), 4, "{lines:#?}");
    assert!(h.state().markups().iter().all(|a| a.id != reply));
    h.key_press_modifiers(CTRL, Key::Z);
    h.run();
    assert_eq!(rows(&mut h).len(), 6);
}

#[test]
fn selection_goes_both_ways_between_the_list_and_the_page() {
    // Two pages; enough markups on the second that the list has to scroll to reach the last.
    let mut markups = vec![
        square(0, 100.0, 600.0, "First", "", "Ada"),
        square(0, 300.0, 600.0, "Second", "", "Ada"),
    ];
    for i in 0..40 {
        let (x, y) = (60.0 + 70.0 * (i % 6) as f64, 700.0 - 50.0 * (i / 6) as f64);
        markups.push(square(1, x, y, &format!("Item {}", i + 1), "", "Ben"));
    }
    let mut h = list_app(2, 0.25, markups);
    show_columns(&mut h, &[Builtin::Subject, Builtin::Page, Builtin::Author]);
    assert_eq!(rows(&mut h).len(), 42);
    assert!(
        h.query_by_label("Subject Item 40, row 42").is_none(),
        "only the rows on screen are laid out"
    );
    let first = id_of(&h, "First");
    let second = id_of(&h, "Second");

    // List to page: a click on a row selects the markup, with the Select tool, and brings
    // it into view.
    set_tool(&mut h, Tool::Rectangle);
    click_label(&mut h, "Subject Second, row 2");
    assert_eq!(selected_ids(&h), [second.as_str()]);
    assert_eq!(h.state().tool(), Tool::Select);
    assert!(rows(&mut h)[1].selected && !rows(&mut h)[0].selected);
    // Ctrl adds a row; Shift takes the range.
    let cell = cell_centre(&h, "Subject First, row 1");
    click_at_with(&mut h, cell, CTRL);
    h.remove_cursor();
    h.run();
    assert_eq!(selected_ids(&h), [second.as_str(), first.as_str()]);
    click_label(&mut h, "Subject First, row 1");
    assert_eq!(selected_ids(&h), [first.as_str()]);
    let cell = cell_centre(&h, "Subject Item 2, row 4");
    click_at_with(&mut h, cell, SHIFT);
    h.remove_cursor();
    h.run();
    assert_eq!(selected_ids(&h).len(), 4);
    assert!(rows(&mut h)[..4].iter().all(|l| l.selected));
    // A markup on another page: the view goes there.
    assert_eq!(h.state().active_view().unwrap().shown_page(), 0);
    click_label(&mut h, "Subject Item 3, row 5");
    settle(&mut h);
    let item = id_of(&h, "Item 3");
    assert_eq!(selected_ids(&h), [item.as_str()]);
    let dv = h.state().active_view().unwrap();
    let bounds = Rect::new(200.0, 700.0, 260.0, 740.0);
    let on_view = dv
        .view
        .page_to_view(1, bounds.center())
        .expect("page 2 is laid out");
    let viewport = dv.view.viewport();
    assert!(
        on_view.x > 0.0
            && on_view.x < viewport.width
            && on_view.y > 0.0
            && on_view.y < viewport.height,
        "the markup is in view: {on_view:?} in {viewport:?}"
    );

    // Page to list: clicking a markup on the page marks its row, and the list scrolls to it.
    // The markup clicked is the last one of the page that is on screen: far down the list.
    let (number, at) = {
        let dv = h.state().active_view().unwrap();
        (0..40)
            .rev()
            .find_map(|i| {
                // On its outline: an unfilled rectangle is not hit in its middle.
                let edge = Point::new(
                    60.0 + 70.0 * (i % 6) as f64,
                    700.0 - 50.0 * (i / 6) as f64 + 20.0,
                );
                let p = dv.view.page_to_view(1, edge)?;
                let pos = egui::pos2(dv.rect.min.x + p.x as f32, dv.rect.min.y + p.y as f32);
                dv.rect.shrink(12.0).contains(pos).then_some((i + 1, pos))
            })
            .expect("some markup of page 2 is on screen")
    };
    assert!(
        number > 12,
        "Item {number} is beyond the first screenful of rows"
    );
    let subject = format!("Item {number}");
    let name = format!("Subject {subject}, row {}", number + 2);
    assert!(
        h.query_by_label(&name).is_none(),
        "{name} is not laid out yet"
    );
    let target = id_of(&h, &subject);
    click_at(&mut h, at);
    h.remove_cursor();
    settle(&mut h);
    assert_eq!(selected_ids(&h), [target.as_str()]);
    let lines = rows(&mut h);
    assert!(lines[number + 1].selected);
    assert_eq!(lines.iter().filter(|l| l.selected).count(), 1);
    h.get_by_label(&name);
    assert!(
        h.query_by_label("Subject First, row 1").is_none(),
        "the list scrolled down to the selected row"
    );

    // Several selected rows are deleted together, and come back together.
    click_label(&mut h, &name);
    let cell = cell_centre(&h, &format!("Subject Item {}, row {}", number - 2, number));
    click_at_with(&mut h, cell, SHIFT);
    h.remove_cursor();
    h.run();
    assert_eq!(selected_ids(&h).len(), 3);
    click(&mut h, "Delete rows");
    assert_eq!(h.state().markups().len(), 39);
    assert_eq!(rows(&mut h).len(), 39);
    h.key_press_modifiers(CTRL, Key::Z);
    h.run();
    assert_eq!(rows(&mut h).len(), 42);
}

#[test]
fn exports_write_files_that_read_back_and_xfdf_is_imported() {
    let dir = tempfile::tempdir().unwrap();
    let csv = dir.path().join("out.csv");
    let xlsx = dir.path().join("out.xlsx");
    let xfdf = dir.path().join("out.xfdf");
    let dialogs = ScriptedDialogs::default()
        .with_exports([csv.clone(), xlsx.clone(), xfdf.clone()])
        .with_imports([xfdf.clone(), xfdf.clone()]);
    let mut h = mock_app_with(1, 0.04, None, dialogs);
    assert!(h.state_mut().add_markups(sample()));
    h.state_mut().set_markups_list_open(true);
    let cost = h
        .state_mut()
        .add_list_column("Cost", ColumnType::Number, &[])
        .unwrap();
    settle(&mut h);
    show_columns(
        &mut h,
        &[
            Builtin::Subject,
            Builtin::Kind,
            Builtin::Author,
            Builtin::Comment,
        ],
    );
    // Two costs, typed into the list.
    for (row, text) in [(1, "125.5"), (4, "74.5")] {
        let cell = cell_centre(&h, &format!("Cost , row {row}"));
        double_click_at(&mut h, cell);
        h.run();
        type_into(&mut h, &format!("Edit Cost, row {row}"), text);
        h.key_press(Key::Enter);
        h.run();
        h.run();
    }
    let cost_column = h.state().list_schema().get(&cost).unwrap().clone();
    let valued = h
        .state()
        .markups()
        .iter()
        .filter(|a| custom_value(a, &cost_column) != Value::Empty)
        .count();
    assert_eq!(valued, 2);
    h.state_mut().edit_list_layout(|layout| {
        layout.sort = vec![SortKey::asc(Builtin::Subject)];
    });
    h.run();

    // CSV: what the list shows, in its order, headings first.
    click(&mut h, "Export");
    assert_all_named(&h, "the Export menu");
    click_starting(&mut h, "Export as CSV");
    assert!(csv.is_file(), "{:?}", h.state().status());
    assert!(h.state().status().unwrap().contains("Exported 4 markup(s)"));
    let text = std::fs::read_to_string(&csv).unwrap();
    let text = text.strip_prefix('\u{feff}').expect("a byte-order mark");
    let records: Vec<Vec<&str>> = text
        .split("\r\n")
        .filter(|line| !line.is_empty())
        .map(|line| line.split(',').collect())
        .collect();
    assert_eq!(records[0], ["Subject", "Type", "Author", "Comment", "Cost"]);
    assert_eq!(records.len(), 5);
    assert_eq!(records[1], ["Beam 2", "Rectangle", "Ben", "", "74.5"]);
    assert_eq!(
        records[2],
        ["Beam 10", "Rectangle", "Ada", "Beam B10 over door", "125.5"]
    );
    assert_eq!(records[4][0], "Setout");

    // XLSX: the same rows as typed cells, with a SUM under the number column.
    run_command(&mut h, Command::ExportMarkups(ExportKind::Xlsx));
    assert!(xlsx.is_file(), "{:?}", h.state().status());
    {
        use calamine::{Data, Reader};
        let mut book: calamine::Xlsx<_> = calamine::open_workbook(&xlsx).expect("a workbook");
        let sheet = book.worksheet_range("Markups").expect("the Markups sheet");
        let cell = |r: u32, c: u32| sheet.get_value((r, c)).cloned().unwrap_or(Data::Empty);
        assert_eq!(cell(0, 0), Data::String("Subject".into()));
        assert_eq!(cell(0, 4), Data::String("Cost".into()));
        assert_eq!(cell(1, 0), Data::String("Beam 2".into()));
        assert_eq!(cell(1, 4), Data::Float(74.5));
        assert_eq!(cell(2, 4), Data::Float(125.5));
        assert_eq!(cell(4, 0), Data::String("Setout".into()));
        let formulas = book.worksheet_formula("Markups").expect("formulas");
        // After one empty row, the Total row.
        assert_eq!(cell(6, 0), Data::String("Total".into()));
        let sum = formulas.get_value((6, 4)).cloned().unwrap_or_default();
        assert!(
            sum.contains("SUM(E2:E5)"),
            "the total row sums the column: {sum:?}"
        );
        assert_eq!(cell(6, 4), Data::Float(200.0), "with its cached result");
    }

    // XFDF: the markups themselves; reading it back gives the same model.
    run_command(&mut h, Command::ExportMarkups(ExportKind::Xfdf));
    assert!(xfdf.is_file(), "{:?}", h.state().status());
    let read = import_xfdf(&std::fs::read_to_string(&xfdf).unwrap()).expect("XFDF");
    let ours = h.state().markups();
    assert_eq!(read.annotations.len(), ours.len());
    assert!(read.skipped.is_empty(), "{:?}", read.skipped);
    for (a, b) in read.annotations.iter().zip(&ours) {
        assert_eq!(a.detached(), b.detached());
    }

    // Importing it into the document it came from adds nothing...
    click(&mut h, "Export");
    click_starting(&mut h, "Import XFDF");
    assert_eq!(h.state().markups().len(), 4);
    assert!(
        h.state()
            .status()
            .unwrap()
            .contains("no markups that are new"),
        "{:?}",
        h.state().status()
    );
    // ... and into an empty one, everything, as one undo step.
    h.state_mut().select_all_markups();
    assert!(h.state_mut().delete_selection());
    assert!(h.state().markups().is_empty());
    assert_eq!(h.state_mut().import_xfdf(), 4);
    h.run();
    let back = h.state().markups();
    assert_eq!(back.len(), 4);
    for (a, b) in back.iter().zip(&ours) {
        assert_eq!(a.detached(), b.detached());
    }
    assert_eq!(column(&mut h, Builtin::Subject).len(), 4);
    h.key_press_modifiers(CTRL, Key::Z);
    h.run();
    assert!(h.state().markups().is_empty());

    // A cancelled dialog writes nothing and says nothing.
    assert!(h.state_mut().export_markups(ExportKind::Csv).is_none());
}

#[test]
fn the_layout_and_the_custom_columns_persist_in_the_data_directory() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let file = data.join(LIST_FILE_NAME);
    let (layout, schema) = {
        let mut h = mock_app_with(1, 0.25, Some(data.clone()), ScriptedDialogs::default());
        assert!(!h.state().markups_list_open(), "closed until asked for");
        assert!(h.state_mut().add_markups(sample()));
        h.key_press_modifiers(CTRL, Key::L);
        h.run();
        assert!(h.state().markups_list_open());
        h.state_mut()
            .add_list_column("Trade", ColumnType::Choice, &["Civil", "Drainage"])
            .unwrap();
        show_columns(&mut h, &[Builtin::Subject, Builtin::Author, Builtin::Kind]);
        click(&mut h, "Sort by Author");
        click(&mut h, "Sort by Author, now ascending");
        click_label(&mut h, "Group by");
        click(&mut h, "Type");
        click_label(&mut h, "Group Type: Line, 1 markups, open");
        click(&mut h, "Filter row");
        h.state_mut().edit_list_layout(|layout| {
            layout.set_width(&ColumnId::Builtin(Builtin::Subject), 212.0);
        });
        // The file is written by itself once the changes have settled: a few frames later.
        for _ in 0..4 {
            h.run();
        }
        assert!(
            file.is_file(),
            "the list file was written without being asked"
        );
        let layout = h.state().list_layout().clone();
        assert_eq!(layout.sort, [SortKey::desc(Builtin::Author)]);
        assert_eq!(layout.collapsed.len(), 1);
        (layout, h.state().list_schema().clone())
    };
    let json = std::fs::read_to_string(&file).unwrap();
    assert!(json.contains("\"Trade\"") && json.contains("Drainage"));

    // The next run starts where this one stopped.
    let mut h = mock_app_with(1, 0.25, Some(data.clone()), ScriptedDialogs::default());
    assert!(h.state().markups_list_open());
    assert_eq!(h.state().list_layout(), &layout);
    assert_eq!(h.state().list_schema(), &schema);
    assert!(h.state_mut().add_markups(sample()));
    settle(&mut h);
    h.get_by_label("Filter Author");
    h.get_by_label("Group Type: Line, 1 markups, closed");
    let width = h.get_by_label("Subject Column, row 2").rect().width();
    assert!((width - 212.0).abs() < 1.0, "{width}");
    assert_eq!(
        column(&mut h, Builtin::Author),
        ["Ben", "Ben", "Ada"],
        "grouped by type, the line's group closed, authors descending"
    );
    // Closing the list is remembered too.
    click(&mut h, "Close markups list");
    assert!(!h.state().markups_list_open());
    h.state_mut().shutdown();
    drop(h);
    let h = mock_app_with(1, 0.25, Some(data), ScriptedDialogs::default());
    assert!(!h.state().markups_list_open());
    assert_eq!(h.state().list_schema(), &schema);

    // A damaged file is ignored: the list starts as new.
    let other = dir.path().join("other");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join(LIST_FILE_NAME), "{ not json").unwrap();
    let h = mock_app_with(1, 0.25, Some(other), ScriptedDialogs::default());
    assert!(h.state().list_schema().columns.is_empty());
    assert!(!h.state().markups_list_open());
}

#[test]
fn fifty_thousand_rows_cost_a_frame_no_more_than_a_screenful() {
    const ROWS: usize = 50_000;
    const PAGES: usize = 100;
    let mut h = mock_app_with(PAGES, 0.25, None, ScriptedDialogs::default());
    let when = origin("Ada");
    let markups: Vec<Annotation> = (0..ROWS)
        .map(|i| {
            let page = i % PAGES;
            let k = i / PAGES;
            let (x, y) = (20.0 + 11.0 * (k % 50) as f64, 20.0 + 70.0 * (k / 50) as f64);
            let mut a = Annotation::square(page, Rect::new(x, y, x + 9.0, y + 9.0), &when);
            // Subjects sort in the reverse of the document's order.
            a.meta.subject = Some(format!("Item {:05}", ROWS - i));
            a
        })
        .collect();
    let started = Instant::now();
    assert!(h.state_mut().add_markups(markups));
    eprintln!(
        "50,000-row list: adding the markups took {:?}",
        started.elapsed()
    );
    h.state_mut().set_markups_list_open(true);
    let started = Instant::now();
    settle(&mut h);
    show_columns(&mut h, &[Builtin::Subject, Builtin::Page, Builtin::Kind]);
    settle(&mut h);
    eprintln!(
        "50,000-row list: the first frames took {:?}",
        started.elapsed()
    );

    let stats = h.state().list_stats();
    assert_eq!(stats.lines, ROWS);
    let panel = h.get_by_label("Subject Item 50000, row 1").rect();
    assert!((panel.height() - ROW_HEIGHT).abs() < 0.5);
    // What a screenful is: the panel is a few hundred pixels tall.
    let screenful = (720.0 / ROW_HEIGHT) as usize;
    let check = |h: &App, what: &str| {
        let stats = h.state().list_stats();
        assert!(
            stats.rows >= 3 && stats.rows <= screenful,
            "{what}: {} rows laid out",
            stats.rows
        );
        assert!(
            stats.cells <= stats.rows * 3,
            "{what}: {} cells for {} rows of 3 columns",
            stats.cells,
            stats.rows
        );
    };
    check(&h, "at the top");

    // Frames with nothing changed: the rows are not built again, and a frame is short.
    let builds = stats.builds;
    let mut longest = Duration::ZERO;
    for _ in 0..10 {
        let started = Instant::now();
        h.step();
        longest = longest.max(started.elapsed());
    }
    assert_eq!(h.state().list_stats().builds, builds, "nothing changed");
    assert!(
        longest < Duration::from_millis(150),
        "a frame of the 50,000-row list took {longest:?}"
    );
    check(&h, "idle");
    eprintln!(
        "50,000-row list: longest idle frame {longest:?}, {} cells laid out for {} rows",
        h.state().list_stats().cells,
        h.state().list_stats().rows
    );

    // Scrolled far down, it is still a screenful.
    let over = panel.center() + egui::vec2(0.0, 40.0);
    h.hover_at(over);
    h.run();
    for _ in 0..12 {
        h.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Page,
            delta: egui::vec2(0.0, -40.0),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
            modifiers: egui::Modifiers::NONE,
        });
        h.run();
    }
    h.remove_cursor();
    h.run();
    assert!(
        h.query_by_label("Subject Item 50000, row 1").is_none(),
        "scrolled away from the top"
    );
    check(&h, "scrolled");
    assert_eq!(
        h.state().list_stats().builds,
        builds,
        "scrolling builds nothing"
    );

    // Sorting 50,000 rows: one build, the right order, and the same small frame after it.
    let started = Instant::now();
    h.state_mut().edit_list_layout(|layout| {
        layout.sort = vec![SortKey::asc(Builtin::Subject)];
    });
    h.run();
    let sort_took = started.elapsed();
    eprintln!("50,000-row list: sort, rebuild and one frame took {sort_took:?}");
    assert_eq!(h.state().list_stats().builds, builds + 1);
    assert!(
        sort_took < Duration::from_secs(5),
        "sorting took {sort_took:?}"
    );
    let lines = rows(&mut h);
    assert_eq!(lines.len(), ROWS);
    assert_eq!(lines[0].cells[0], "Item 00001");
    assert_eq!(lines[ROWS - 1].cells[0], "Item 50000");
    assert!(lines.windows(2).all(|w| w[0].cells[0] < w[1].cells[0]));
    // Grouping and filtering it.
    h.state_mut().edit_list_layout(|layout| {
        layout.group = vec![GroupKey::asc(Builtin::Page)];
        layout.filter.search = "0004".to_owned();
    });
    h.run();
    let lines = rows(&mut h);
    let groups = lines.iter().filter(|l| l.group.is_some()).count();
    let shown = lines.iter().filter(|l| l.id.is_some()).count();
    let expected = (1..=ROWS)
        .filter(|n| format!("{n:05}").contains("0004"))
        .count();
    assert_eq!(shown, expected, "the items with 0004 in their number");
    assert!(groups > 1 && groups <= expected, "{groups} groups");
    assert_eq!(
        lines
            .iter()
            .filter(|l| l.group.is_some())
            .map(|l| l.count)
            .sum::<usize>(),
        shown
    );
    let mut longest = Duration::ZERO;
    for _ in 0..5 {
        let started = Instant::now();
        h.step();
        longest = longest.max(started.elapsed());
    }
    assert!(longest < Duration::from_millis(150), "{longest:?}");
    let stats = h.state().list_stats();
    assert!(stats.rows <= screenful && stats.cells <= stats.rows.max(1) * 3);
}

// ---- snapshot of the interface (goldens depend on the GPU adapter) -------------------------

#[test]
fn snapshot_markups_list_light() {
    let _gpu = gpu();
    let mut h = list_app(2, 0.04, sample());
    h.state_mut()
        .add_list_column("Cost", ColumnType::Number, &[])
        .unwrap();
    h.state_mut()
        .add_list_column("Trade", ColumnType::Choice, &["Civil", "Drainage"])
        .unwrap();
    show_columns(
        &mut h,
        &[
            Builtin::Subject,
            Builtin::Kind,
            Builtin::Author,
            Builtin::Comment,
            Builtin::Status,
            Builtin::Checkmark,
        ],
    );
    for (row, text) in [(1, "125.5"), (4, "74.5")] {
        let cell = cell_centre(&h, &format!("Cost , row {row}"));
        double_click_at(&mut h, cell);
        h.run();
        type_into(&mut h, &format!("Edit Cost, row {row}"), text);
        h.key_press(Key::Enter);
        h.run();
        h.run();
    }
    click_label(&mut h, "Status, row 2");
    click(&mut h, "Accepted");
    click_label(&mut h, "Trade, row 1");
    click(&mut h, "Drainage");
    h.get_by_role_and_label(Role::CheckBox, "Checked, row 4")
        .click();
    h.run();
    click_label(&mut h, "Subject Beam 10, row 1");
    click(&mut h, "Add reply");
    h.run();
    type_into(
        &mut h,
        "Reply to the selected row",
        "Is this the B10 on grid 4?",
    );
    click(&mut h, "Send reply");
    click_label(&mut h, "Group by");
    click(&mut h, "Type");
    click(&mut h, "Sort by Subject");
    click_label(&mut h, "Subject Beam 2, row 6");
    h.remove_cursor();
    settle(&mut h);
    h.snapshot("markups_list_light");
}

/// redcolumn's A3-07 (`samples/external/redcolumn`, local only; skipped when absent): the
/// document's Bluebeam columns and its custom status set appear in the list as it opens, a
/// value typed into a Bluebeam cell lands in `/BSIColumnData` and is one undo step, a state of
/// the custom set is picked from the Status cell, and the custom columns dialog adds a column
/// to the PDF's own Bluebeam columns.
#[test]
fn bluebeam_columns_and_custom_statuses_show_and_edit() {
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("samples")
        .join("external")
        .join("redcolumn")
        .join("A3-07-columns-statuses-metadata.pdf");
    if !source.is_file() {
        eprintln!("REDCOLUMN SAMPLE SKIPPED: A3-07-columns-statuses-metadata.pdf is not present");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a3-07.pdf");
    std::fs::copy(&source, &path).unwrap();
    // The file's values hold characters the interface font has no glyph for (①): shown as
    // boxes, as the window does.
    let mut state = state_with(pdfium(), None, ScriptedDialogs::default());
    state.open_path(&path).expect("the file opens");
    let mut h = harness_opts(state, 0.04, true);
    settle(&mut h);
    h.state_mut().set_markups_list_open(true);
    settle(&mut h);
    rows(&mut h);
    settle(&mut h);

    // The columns arrive with the document, in Bluebeam's order, after the built-in ones.
    let titles: Vec<String> = h
        .state()
        .document_columns()
        .iter()
        .map(|c| c.title.clone())
        .collect();
    assert_eq!(titles.len(), 11, "{titles:?}");
    assert_eq!(titles[0], "Discipline");
    let ref_no = h
        .state()
        .document_columns()
        .iter()
        .find(|c| c.title == "Ref No.")
        .unwrap()
        .clone();
    // Only Subject, Status and the Bluebeam columns, Ref No. first, so they fit the window.
    h.state_mut().edit_list_layout(|layout| {
        for state in &mut layout.columns {
            state.visible = matches!(
                state.column,
                ColumnId::Builtin(Builtin::Subject | Builtin::Status) | ColumnId::Custom(_)
            );
        }
        let first = layout.columns.first().map(|c| c.column.clone());
        layout.move_column(&ref_no.column_id(), first.as_ref());
        let first = layout.columns.first().map(|c| c.column.clone());
        layout.move_column(&ColumnId::Builtin(Builtin::Subject), first.as_ref());
    });
    h.run();
    let visible = h.state().list_layout().visible();
    let at = |id: &ColumnId| visible.iter().position(|c| c == id).unwrap();
    let lines = rows(&mut h);
    let discipline = h.state().document_columns()[0].column_id();
    assert!(lines
        .iter()
        .any(|l| l.cells[at(&discipline)] == "Structural"));
    // The custom set's state shows in Status.
    assert!(lines
        .iter()
        .any(|l| l.cells[at(&ColumnId::Builtin(Builtin::Status))] == "In Progress"));

    // A value typed into a Bluebeam cell.
    let target = id_of(&h, "CC02 every column filled");
    let row = lines
        .iter()
        .position(|l| l.id.as_ref() == Some(&target))
        .unwrap()
        + 1;
    let stored = |h: &App| {
        let a = h
            .state()
            .markups()
            .into_iter()
            .find(|a| a.id == target)
            .unwrap();
        pl_doc::pl_takeoff::bluebeam::stored_text(&a, ref_no.bluebeam.as_ref().unwrap().index)
    };
    assert_eq!(stored(&h).as_deref(), Some("REF-0001"));
    let cell = cell_centre(&h, &format!("Ref No. REF-0001, row {row}"));
    double_click_at(&mut h, cell);
    h.run();
    type_into(&mut h, &format!("Edit Ref No., row {row}"), "-B");
    h.key_press(Key::Enter);
    h.run();
    h.run();
    assert_eq!(stored(&h).as_deref(), Some("REF-0001-B"));
    h.get_by_label(&format!("Ref No. REF-0001-B, row {row}"));
    h.key_press_modifiers(CTRL, Key::Z);
    h.run();
    assert_eq!(stored(&h).as_deref(), Some("REF-0001"));
    h.key_press_modifiers(CTRL, Key::Y);
    h.run();
    assert_eq!(stored(&h).as_deref(), Some("REF-0001-B"));

    // A state of the custom set, picked from the Status cell.
    click_label(&mut h, &format!("Status, row {row}"));
    click_label(&mut h, "Closed - No Action");
    let status = h
        .state()
        .active_document()
        .unwrap()
        .status(&target)
        .unwrap_or_else(|| panic!("no status; {:?}", h.state().status()));
    assert_eq!(
        (status.model.as_str(), status.state.as_str()),
        ("BSI_A3TESTSET0001", "Closed - No Action")
    );
    h.key_press_modifiers(CTRL, Key::Z);
    h.run();
    assert_eq!(h.state().active_document().unwrap().status(&target), None);

    // The custom columns dialog adds a column to the PDF's Bluebeam columns (the box is
    // ticked for a document that has them).
    click(&mut h, "Columns");
    click_starting(&mut h, "Custom columns");
    h.get_by_label("In this PDF (Bluebeam columns)");
    h.get_by_label("New column name").focus();
    h.run();
    type_into(&mut h, "New column name", "Phase");
    click_label(&mut h, "New column type");
    click(&mut h, "Multiline text");
    click(&mut h, "Add column");
    settle(&mut h);
    rows(&mut h);
    let added = h
        .state()
        .document_columns()
        .iter()
        .find(|c| c.title == "Phase")
        .cloned()
        .expect("the new Bluebeam column");
    assert!(added.multiline);
    assert_eq!(added.bluebeam.as_ref().unwrap().index, 11);
    assert!(
        h.state().list_schema().columns.is_empty(),
        "nothing in the list's own file"
    );
    click(&mut h, "Close custom columns");
    h.key_press_modifiers(CTRL, Key::Z);
    h.run();
    rows(&mut h);
    assert_eq!(h.state().document_columns().len(), 11);
}
