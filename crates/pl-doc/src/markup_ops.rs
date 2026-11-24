//! Commands for the things a markups list does to a markup: change its text, record a review
//! status, reply, tick it off, lock it, fill in a custom column, and give it a popup window.
//!
//! Each function here is pure: it looks at the annotations as they are and returns the
//! [`Command`] that makes the change, built from the ordinary add, modify and delete
//! commands. So every one of them is one undo step with an exact inverse, is journaled like
//! any other command, and replays to the same result (new annotations carry their ids and
//! dates inside the command). [`Document`](crate::Document) has a method of the same name for
//! each that builds the command and applies it.
//!
//! # What is written into the file
//!
//! * **Text** is `/Contents` (and, for a text box, its appearance and `/RC`).
//! * **Status** is a hidden Text annotation replying to the markup with `/StateModel (Review)`
//!   and `/State`, the way Acrobat records one ([`pl_annot::Annotation::review_status`]); the
//!   latest is the markup's status. Nothing is overwritten, so the history stays in the file.
//! * **Reply** is a Text annotation with `/IRT` and `/RT /R`.
//! * **Checkmark** is the same kind of status annotation under the standard `Marked` state
//!   model (`/State (Marked)` or `(Unmarked)`), as pl-takeoff's markups list reads it
//!   ([`pl_takeoff::checkmark_status`]).
//! * **Lock** is the standard Locked flag (bit 8 of `/F`).
//! * **Custom column values** have no standard key. They are private data under the
//!   annotation's `/PlumblineData /Columns`, in pl-takeoff's layout
//!   ([`pl_takeoff::set_custom_value`]), typed by the column's definition. A column the
//!   document defines in Bluebeam Revu's `/BSIAnnotColumns` keeps its values where Revu does,
//!   in the annotation's `/BSIColumnData` at the column's position ([`pl_takeoff::bluebeam`]).
//! * **Custom status sets** (Revu's) are status annotations like the review status, under the
//!   set's own `/StateModel` ([`set_status_named`]).

use pl_annot::{
    AnnotId, AnnotKind, AnnotSet, Annotation, Flags, Origin, PdfDate, ReviewState, Status,
    REVIEW_MODEL,
};
use pl_geom::Rect;
use pl_takeoff::{
    BluebeamColumns, ColumnError, ColumnType, CustomColumn, Value, MARKED, MARKED_MODEL,
};

use crate::annot_data::PdfValue;

use crate::command::{Command, EditError};

fn existing<'a>(set: &'a AnnotSet, id: &AnnotId) -> Result<&'a Annotation, EditError> {
    set.get(id).ok_or_else(|| EditError::NotFound(id.clone()))
}

/// The annotation `id` as it would be after `change`, stamped as modified at `when`, wrapped
/// in a modify command.
fn modified(
    set: &AnnotSet,
    id: &AnnotId,
    when: PdfDate,
    change: impl FnOnce(&mut Annotation) -> Result<(), EditError>,
) -> Result<Command, EditError> {
    let before = existing(set, id)?;
    let mut after = before.clone();
    change(&mut after)?;
    if after.detached() != before.detached() {
        after.touch(when);
    }
    // Data only, like every annotation inside a command: the tie to the file stays with the
    // annotation in the set.
    Ok(Command::modify(after.detached()))
}

/// As [`modified`], for a change that belongs to a whole count: a count another application
/// (Bluebeam Revu) writes as one annotation per marker, grouped under the first with `/IRT` +
/// `/RT /Group` ([`Annotation::count_group`]), is one markup in the list, so its subject,
/// lock, column values, ... are changed on every annotation of the group on its page, as Revu
/// keeps them. Anything else is `id` alone.
fn modified_group(
    set: &AnnotSet,
    id: &AnnotId,
    when: PdfDate,
    change: impl Fn(&mut Annotation) -> Result<(), EditError>,
) -> Result<Command, EditError> {
    let target = existing(set, id)?;
    let group = target
        .count_group()
        .filter(|_| !target.is_reply())
        .map(|g| (target.page, g));
    let Some(group) = group else {
        return modified(set, id, when, change);
    };
    let mut out = Vec::new();
    for before in set.iter() {
        let same = before.id == *id
            || (before.page == group.0
                && !before.is_reply()
                && before.count_group().as_ref() == Some(&group.1));
        if !same {
            continue;
        }
        let mut after = before.clone();
        change(&mut after)?;
        let changed = after.detached() != before.detached();
        if changed {
            after.touch(when);
        }
        if changed || before.id == *id {
            out.push(after.detached());
        }
    }
    Ok(Command::ModifyAnnotations(out))
}

/// A status annotation belongs to a markup: not to a reply, a link or a popup.
fn status_target<'a>(set: &'a AnnotSet, id: &AnnotId) -> Result<&'a Annotation, EditError> {
    let parent = existing(set, id)?;
    if !parent.kind.is_markup() || parent.meta.reply_to.is_some() {
        return Err(EditError::Unsupported {
            id: id.clone(),
            reason: "a status belongs to a markup, not to a reply, link or popup".into(),
        });
    }
    Ok(parent)
}

// ---- reading ---------------------------------------------------------------------------------

/// Whether the markup `id` is ticked off: the state of its latest `Marked` status annotation
/// ("latest" by `/M`, set order when the dates tie or are missing, exactly as pl-takeoff's
/// list decides it).
pub fn is_checked(set: &AnnotSet, id: &AnnotId) -> bool {
    let mut latest: Option<(Option<i64>, &str)> = None;
    for a in set.iter() {
        if a.meta.reply_to.as_ref() != Some(id)
            || a.meta.state_model.as_deref() != Some(MARKED_MODEL)
        {
            continue;
        }
        let Some(state) = a.meta.state.as_deref() else {
            continue;
        };
        let stamp = a.meta.modified.map(|d| d.to_unix());
        let newer = match latest {
            None => true,
            Some((old, _)) => !matches!((old, stamp), (Some(o), Some(n)) if n < o),
        };
        if newer {
            latest = Some((stamp, state));
        }
    }
    latest.is_some_and(|(_, state)| state == MARKED)
}

/// Whether the annotation carries the Locked flag.
pub fn is_locked(a: &Annotation) -> bool {
    a.meta.flags.has(Flags::LOCKED)
}

/// The annotation's value in a custom column ([`pl_takeoff::custom_value`]).
pub fn column_value(a: &Annotation, column: &CustomColumn) -> Value {
    pl_takeoff::custom_value(a, column)
}

// ---- commands --------------------------------------------------------------------------------

/// Sets a markup's text (`/Contents`): the comment of a shape, the words of a text box or
/// note, the text of a reply.
///
/// A text box written by another application is first taken over as an editable one
/// ([`Annotation::make_editable`]), because its appearance has to be drawn again from the new
/// text. Text a text box cannot show (a character outside WinAnsi) is refused by the editor
/// with [`EditError::Unsupported`] when the command is executed.
///
/// A measurement's text is its value, worked out from its geometry and scale and written as
/// its `/Contents` by every save, so it cannot be set: that is refused here.
pub fn set_text(
    set: &AnnotSet,
    id: &AnnotId,
    text: &str,
    when: PdfDate,
) -> Result<Command, EditError> {
    modified(set, id, when, |a| {
        let new = (!text.is_empty()).then(|| text.to_string());
        if a.meta.contents == new {
            return Ok(());
        }
        if a.measurement().is_some() {
            return Err(EditError::Unsupported {
                id: a.id.clone(),
                reason: "a measurement's text is its value and cannot be typed".into(),
            });
        }
        if a.kind == AnnotKind::FreeText {
            a.make_editable();
        }
        a.meta.contents = new;
        Ok(())
    })
}

/// Sets a markup's subject (`/Subj`); empty text clears it. Nothing changes, and the markup
/// is not stamped as modified, when the subject is what it already was.
pub fn set_subject(
    set: &AnnotSet,
    id: &AnnotId,
    subject: &str,
    when: PdfDate,
) -> Result<Command, EditError> {
    modified_group(set, id, when, |a| {
        a.meta.subject = (!subject.is_empty()).then(|| subject.to_string());
        Ok(())
    })
}

/// Records a review status for a markup, set by `origin.author` (see the module
/// documentation). Returns the command and the id of the status annotation it adds.
pub fn set_status(
    set: &AnnotSet,
    id: &AnnotId,
    state: ReviewState,
    origin: &Origin,
) -> Result<(Command, AnnotId), EditError> {
    let parent = status_target(set, id)?;
    // Later than any status the markup has, or it would not be the latest (see
    // [`AnnotSet::status_origin`]).
    let origin = &set.status_origin(id, REVIEW_MODEL, origin);
    let status = Annotation::review_status(parent, state, origin);
    let new = status.id.clone();
    Ok((Command::add(status), new))
}

/// Records a status of any state model for a markup: a standard review state under
/// `Review`, or a state of a custom status set under that set's own `/StateModel` (as
/// Bluebeam Revu writes them). Later than every status the markup has, of any model, so it is
/// the one shown. Returns the command and the id of the status annotation it adds.
pub fn set_status_named(
    set: &AnnotSet,
    id: &AnnotId,
    status: &Status,
    origin: &Origin,
) -> Result<(Command, AnnotId), EditError> {
    let parent = status_target(set, id)?;
    if status.model.trim().is_empty() || status.state.trim().is_empty() {
        return Err(EditError::Unsupported {
            id: id.clone(),
            reason: "a status needs a state model and a state".into(),
        });
    }
    let origin = &set.status_origin_any(id, origin);
    let a = Annotation::status_reply(parent, &status.model, &status.state, origin);
    let new = a.id.clone();
    Ok((Command::add(a), new))
}

/// Adds a column to Bluebeam Revu's custom columns of the document (`/BSIAnnotColumns`, in
/// Revu's own entry structure) and pads every `/BSIColumnData` the markups have to the new
/// number of columns, as one command. `columns` is the document's present definitions.
/// Returns the command and the new column (bound to its position).
pub fn add_bluebeam_column(
    set: &AnnotSet,
    columns: &BluebeamColumns,
    title: &str,
    kind: ColumnType,
    options: &[&str],
    multiline: bool,
) -> Result<(Command, CustomColumn), ColumnError> {
    let mut after = columns.clone();
    let column = after.add(title, kind, options, multiline, None)?;
    let width = after.entries.len();
    let mut commands = vec![Command::SetBluebeamColumns(
        after.entries.iter().map(PdfValue::from_object).collect(),
    )];
    let padded: Vec<Annotation> = set
        .iter()
        .filter_map(|a| {
            let mut b = a.detached();
            pl_takeoff::bluebeam::pad_column_data(&mut b, width).then_some(b)
        })
        .collect();
    if !padded.is_empty() {
        commands.push(Command::ModifyAnnotations(padded));
    }
    Ok((Command::Batch(commands), column))
}

/// Adds a reply to a markup or to another reply. Returns the command and the reply's id.
pub fn add_reply(
    set: &AnnotSet,
    id: &AnnotId,
    text: &str,
    origin: &Origin,
) -> Result<(Command, AnnotId), EditError> {
    let parent = existing(set, id)?;
    if !parent.kind.is_markup() {
        return Err(EditError::Unsupported {
            id: id.clone(),
            reason: "only a markup or a reply can be replied to".into(),
        });
    }
    let reply = Annotation::reply(parent, text, origin);
    let new = reply.id.clone();
    Ok((Command::add(reply), new))
}

/// Deletes a reply (or a status annotation) with the replies made to it. An annotation that
/// replies to nothing is refused: deleting a markup is [`Command::delete`].
pub fn delete_reply(set: &AnnotSet, id: &AnnotId) -> Result<Command, EditError> {
    let a = existing(set, id)?;
    if a.meta.reply_to.is_none() {
        return Err(EditError::Unsupported {
            id: id.clone(),
            reason: "it is not a reply".into(),
        });
    }
    Ok(Command::delete(id.clone()))
}

/// Ticks a markup off, or clears the tick, on behalf of `origin.author`: a status annotation
/// under the `Marked` model is added (see the module documentation). When the markup is in
/// that state already the command does nothing.
pub fn set_checked(
    set: &AnnotSet,
    id: &AnnotId,
    checked: bool,
    origin: &Origin,
) -> Result<Command, EditError> {
    let parent = status_target(set, id)?;
    if is_checked(set, id) == checked {
        return Ok(Command::Batch(Vec::new()));
    }
    let origin = &set.status_origin(id, MARKED_MODEL, origin);
    Ok(Command::add(pl_takeoff::checkmark_status(
        parent, checked, origin,
    )))
}

/// Ticks a markup off if it is not, clears the tick if it is.
pub fn toggle_checked(set: &AnnotSet, id: &AnnotId, origin: &Origin) -> Result<Command, EditError> {
    set_checked(set, id, !is_checked(set, id), origin)
}

/// Sets or clears the Locked flag. The flag is advice to user interfaces (this one and other
/// viewers) not to move, reshape or delete the markup; the editor itself applies whatever
/// command it is given.
pub fn set_locked(
    set: &AnnotSet,
    id: &AnnotId,
    locked: bool,
    when: PdfDate,
) -> Result<Command, EditError> {
    modified_group(set, id, when, |a| {
        a.meta.flags = a.meta.flags.with(Flags::LOCKED, locked);
        Ok(())
    })
}

/// Locks a markup that is not locked, unlocks one that is.
pub fn toggle_locked(set: &AnnotSet, id: &AnnotId, when: PdfDate) -> Result<Command, EditError> {
    let now = is_locked(existing(set, id)?);
    set_locked(set, id, !now, when)
}

/// Sets a markup's value in a custom column; [`Value::Empty`] clears it. The value is checked
/// against the column's type (a number for a number column, one of the options for a choice,
/// ...); one that does not fit is refused with [`EditError::Unsupported`] saying why.
pub fn set_column(
    set: &AnnotSet,
    id: &AnnotId,
    column: &CustomColumn,
    value: Value,
    when: PdfDate,
) -> Result<Command, EditError> {
    modified_group(set, id, when, |a| {
        pl_takeoff::set_custom_value(a, column, value.clone())
            .map(|_| ())
            .map_err(|e| EditError::Unsupported {
                id: id.clone(),
                reason: e.to_string(),
            })
    })
}

/// As [`set_column`] with the value as a user typed it ([`CustomColumn::parse`]).
pub fn set_column_text(
    set: &AnnotSet,
    id: &AnnotId,
    column: &CustomColumn,
    text: &str,
    when: PdfDate,
) -> Result<Command, EditError> {
    let value = column.parse(text).map_err(|e| EditError::Unsupported {
        id: id.clone(),
        reason: e.to_string(),
    })?;
    set_column(set, id, column, value, when)
}

/// Removes whatever a markup holds under a column id, also one no column definition is left
/// for.
pub fn clear_column(
    set: &AnnotSet,
    id: &AnnotId,
    column_id: &str,
    when: PdfDate,
) -> Result<Command, EditError> {
    modified_group(set, id, when, |a| {
        pl_takeoff::clear_custom_value(a, column_id);
        Ok(())
    })
}

/// Gives a markup a popup window beside it when it has none. Returns the popup's id and, when
/// one had to be made, the command that adds it and links the markup to it. `page_box` is the
/// page's visible box, which the popup is kept inside.
pub fn ensure_popup(
    set: &AnnotSet,
    id: &AnnotId,
    page_box: Rect,
    origin: &Origin,
) -> Result<(Option<Command>, AnnotId), EditError> {
    let parent = existing(set, id)?;
    if !parent.kind.is_markup() {
        return Err(EditError::Unsupported {
            id: id.clone(),
            reason: "only a markup has a popup window".into(),
        });
    }
    if let Some(popup) = parent.popup.clone().filter(|p| set.contains(p)) {
        return Ok((None, popup));
    }
    let rect = pl_annot::default_popup_rect(parent.geometry.bounds(), page_box);
    let mut linked = parent.clone();
    let popup = Annotation::popup_for(&mut linked, rect, origin);
    let new = popup.id.clone();
    Ok((
        Some(Command::Batch(vec![
            Command::add(popup),
            Command::modify(linked.detached()),
        ])),
        new,
    ))
}

/// Opens or closes a markup's popup window (`/Open` of its popup, or of the annotation itself
/// when it has none: a Text note carries the flag too). Naming the popup itself works as well.
pub fn set_popup_open(
    set: &AnnotSet,
    id: &AnnotId,
    open: bool,
    when: PdfDate,
) -> Result<Command, EditError> {
    let a = existing(set, id)?;
    let target = a
        .popup
        .clone()
        .filter(|p| set.contains(p))
        .unwrap_or_else(|| id.clone());
    modified(set, &target, when, |a| {
        a.open = open;
        Ok(())
    })
}

/// Moves or resizes a popup window. `id` is the popup or the markup it belongs to.
pub fn set_popup_rect(
    set: &AnnotSet,
    id: &AnnotId,
    rect: Rect,
    when: PdfDate,
) -> Result<Command, EditError> {
    let a = existing(set, id)?;
    let target = if a.kind == AnnotKind::Popup {
        id.clone()
    } else {
        a.popup
            .clone()
            .filter(|p| set.contains(p))
            .ok_or_else(|| EditError::Unsupported {
                id: id.clone(),
                reason: "it has no popup window".into(),
            })?
    };
    modified(set, &target, when, |a| {
        a.geometry = pl_annot::Geometry::Rect(rect.abs());
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::Editor;
    use pl_geom::Point;

    const PAGE: Rect = Rect::new(0.0, 0.0, 612.0, 792.0);

    fn origin(author: &str, at: i64) -> Origin {
        Origin::new(author, PdfDate::from_unix(at))
    }

    fn editor() -> (Editor, AnnotId, Origin) {
        let o = origin("Ada", 1_700_000_000);
        let mut ed = Editor::new(AnnotSet::new(), 2);
        let a = Annotation::square(1, Rect::new(100.0, 100.0, 200.0, 160.0), &o);
        let id = a.id.clone();
        ed.execute(Command::add(a)).unwrap();
        (ed, id, o)
    }

    /// `run` with the command built from the editor it is then run on.
    macro_rules! go {
        ($ed:ident, $command:expr) => {{
            let command = $command;
            run(&mut $ed, command)
        }};
    }

    /// Runs a command, checks that undo restores the state exactly and redo the result, and
    /// that the command survives JSON (the journal's form) unchanged.
    fn run(ed: &mut Editor, command: Command) {
        let before = ed.model();
        let json = serde_json::to_string(&command).unwrap();
        assert_eq!(serde_json::from_str::<Command>(&json).unwrap(), command);
        let out = ed.execute(command).unwrap();
        assert!(out.changed);
        let after = ed.model();
        assert_ne!(after, before);
        ed.undo().unwrap();
        assert_eq!(ed.model(), before);
        ed.redo().unwrap();
        assert_eq!(ed.model(), after);
    }

    #[test]
    fn text_status_and_replies() {
        let (mut ed, id, o) = editor();
        let later = PdfDate::from_unix(1_700_000_900);
        go!(
            ed,
            set_text(ed.set(), &id, "Check the fall", later).unwrap()
        );
        let a = ed.set().get(&id).unwrap();
        assert_eq!(a.meta.contents.as_deref(), Some("Check the fall"));
        assert_eq!(a.meta.modified, Some(later));
        // The same text again asks for nothing, and does not move the date.
        let again = set_text(
            ed.set(),
            &id,
            "Check the fall",
            PdfDate::from_unix(1_800_000_000),
        );
        assert!(!ed.execute(again.unwrap()).unwrap().changed);
        // Empty text clears it.
        go!(ed, set_text(ed.set(), &id, "", later).unwrap());
        assert_eq!(ed.set().get(&id).unwrap().meta.contents, None);

        let bo = origin("Bo", 1_700_001_000);
        let (command, status) = set_status(ed.set(), &id, ReviewState::Accepted, &bo).unwrap();
        go!(ed, command);
        assert_eq!(ed.set().review_state(&id), ReviewState::Accepted);
        let s = ed.set().get(&status).unwrap();
        assert_eq!(s.meta.reply_to.as_ref(), Some(&id));
        assert_eq!(s.meta.state_model.as_deref(), Some("Review"));
        assert!(s.meta.flags.has(Flags::HIDDEN));
        let cy = origin("Cy", 1_700_002_000);
        let (command, _) = set_status(ed.set(), &id, ReviewState::Rejected, &cy).unwrap();
        go!(ed, command);
        assert_eq!(ed.set().review_state(&id), ReviewState::Rejected);
        assert_eq!(ed.set().review_history(&id).len(), 2, "the history is kept");
        ed.undo().unwrap();
        assert_eq!(ed.set().review_state(&id), ReviewState::Accepted);
        ed.redo().unwrap();

        let (command, reply) = add_reply(ed.set(), &id, "Is this right?", &bo).unwrap();
        go!(ed, command);
        let (command, nested) = add_reply(ed.set(), &reply, "Yes.", &o).unwrap();
        go!(ed, command);
        assert_eq!(ed.set().replies(&id).len(), 1);
        assert_eq!(ed.set().thread(&id).len(), 4);
        // A status belongs to the markup, not to a reply.
        assert!(matches!(
            set_status(ed.set(), &reply, ReviewState::Completed, &bo),
            Err(EditError::Unsupported { .. })
        ));
        // The markup itself is not a reply.
        assert!(matches!(
            delete_reply(ed.set(), &id),
            Err(EditError::Unsupported { .. })
        ));
        // Deleting the reply takes the reply to it along, and undo brings both back in place.
        let full = ed.model();
        go!(ed, delete_reply(ed.set(), &reply).unwrap());
        assert!(!ed.set().contains(&reply) && !ed.set().contains(&nested));
        assert_eq!(ed.set().thread(&id).len(), 2, "the statuses stay");
        ed.undo().unwrap();
        assert_eq!(ed.model(), full);
        // A status can be deleted the same way.
        go!(ed, delete_reply(ed.set(), &status).unwrap());
        assert_eq!(ed.set().review_history(&id).len(), 1);

        let ghost = AnnotId::from("nope");
        assert_eq!(
            set_text(ed.set(), &ghost, "x", later).unwrap_err(),
            EditError::NotFound(ghost.clone())
        );
        assert!(add_reply(ed.set(), &ghost, "x", &o).is_err());
    }

    #[test]
    fn a_status_or_tick_set_now_counts_although_an_older_one_is_dated_later() {
        let (mut ed, id, _) = editor();
        let now = 1_700_000_500;
        let me = origin("Me", now);
        // Another reviewer's status and tick, both dated six hours ahead of this clock.
        let ahead = origin("Ahead", now + 6 * 3600);
        let parent = ed.set().get(&id).unwrap().clone();
        ed.execute(Command::add(Annotation::review_status(
            &parent,
            ReviewState::Accepted,
            &ahead,
        )))
        .unwrap();
        ed.execute(Command::add(pl_takeoff::checkmark_status(
            &parent, true, &ahead,
        )))
        .unwrap();
        assert_eq!(ed.set().review_state(&id), ReviewState::Accepted);
        assert!(is_checked(ed.set(), &id));

        let (command, new) = set_status(ed.set(), &id, ReviewState::Rejected, &me).unwrap();
        assert!(ed.execute(command).unwrap().changed);
        assert_eq!(ed.set().review_state(&id), ReviewState::Rejected);
        assert_eq!(
            ed.set().get(&new).unwrap().meta.modified.unwrap().to_unix(),
            now + 6 * 3600 + 1
        );
        let command = toggle_checked(ed.set(), &id, &me).unwrap();
        assert!(ed.execute(command).unwrap().changed);
        assert!(!is_checked(ed.set(), &id), "the tick is cleared");
        let command = toggle_checked(ed.set(), &id, &me).unwrap();
        ed.execute(command).unwrap();
        assert!(is_checked(ed.set(), &id), "and set again");
        // pl-takeoff's list reads the same.
        let table = pl_takeoff::Table::build(ed.set().iter(), &pl_takeoff::ListContext::default());
        assert!(table.rows[0].checked);
        // Each is one undo step back to what the other reviewer left.
        for _ in 0..3 {
            ed.undo().unwrap();
        }
        assert_eq!(ed.set().review_state(&id), ReviewState::Accepted);
        assert!(is_checked(ed.set(), &id));
    }

    #[test]
    fn a_subject_that_is_not_changed_changes_nothing() {
        let (mut ed, id, _) = editor();
        let when = PdfDate::from_unix(1_700_009_000);
        let before = ed.set().get(&id).unwrap().clone();
        let subject = before.meta.subject.clone().unwrap_or_default();
        let same = set_subject(ed.set(), &id, &subject, when).unwrap();
        assert!(!ed.execute(same).unwrap().changed);
        assert_eq!(
            ed.set().get(&id).unwrap().meta.modified,
            before.meta.modified
        );
        assert!(!ed.can_undo() || ed.undo_len() == 1);
        let steps = ed.undo_len();
        let other = set_subject(ed.set(), &id, "Door schedule", when).unwrap();
        assert!(ed.execute(other).unwrap().changed);
        let after = ed.set().get(&id).unwrap();
        assert_eq!(after.meta.subject.as_deref(), Some("Door schedule"));
        assert_eq!(after.meta.modified, Some(when));
        assert_eq!(ed.undo_len(), steps + 1);
        let cleared = set_subject(ed.set(), &id, "", when).unwrap();
        ed.execute(cleared).unwrap();
        assert_eq!(ed.set().get(&id).unwrap().meta.subject, None);
    }

    #[test]
    fn checkmark_lock_and_custom_columns() {
        use pl_takeoff::{ColumnType, Schema};
        let (mut ed, id, o) = editor();
        let when = PdfDate::from_unix(1_700_000_500);
        let get = |ed: &Editor| ed.set().get(&id).unwrap().clone();
        assert!(!is_checked(ed.set(), &id) && !is_locked(&get(&ed)));

        // The checkmark is a status annotation under the Marked model, as the list reads it.
        let bo = origin("Bo", 1_700_000_600);
        go!(ed, toggle_checked(ed.set(), &id, &bo).unwrap());
        assert!(is_checked(ed.set(), &id));
        let mark = ed.set().iter().last().unwrap().clone();
        assert_eq!(mark.meta.reply_to.as_ref(), Some(&id));
        assert_eq!(mark.meta.state_model.as_deref(), Some("Marked"));
        assert_eq!(mark.meta.state.as_deref(), Some("Marked"));
        assert!(mark.meta.flags.has(Flags::HIDDEN));
        assert_eq!(
            ed.set().review_state(&id),
            ReviewState::None,
            "not a review status"
        );
        // Asking for the state it has does nothing.
        let same = set_checked(ed.set(), &id, true, &bo).unwrap();
        assert!(!ed.execute(same).unwrap().changed);
        let cy = origin("Cy", 1_700_000_700);
        go!(ed, toggle_checked(ed.set(), &id, &cy).unwrap());
        assert!(!is_checked(ed.set(), &id));
        ed.undo().unwrap();
        assert!(is_checked(ed.set(), &id));
        // pl-takeoff's list agrees.
        let table = pl_takeoff::Table::build(ed.set().iter(), &pl_takeoff::ListContext::default());
        assert_eq!(table.rows.len(), 1);
        assert!(table.rows[0].checked);
        // A reply has no checkmark of its own.
        let (command, reply) = add_reply(ed.set(), &id, "ok", &o).unwrap();
        ed.execute(command).unwrap();
        assert!(matches!(
            set_checked(ed.set(), &reply, true, &o),
            Err(EditError::Unsupported { .. })
        ));

        go!(ed, toggle_locked(ed.set(), &id, when).unwrap());
        assert!(is_locked(&get(&ed)) && get(&ed).meta.flags.is_locked());
        assert!(
            get(&ed).meta.flags.has(Flags::PRINT),
            "other flags are kept"
        );
        assert_eq!(get(&ed).meta.modified, Some(when));

        let mut schema = Schema::new();
        let trade = schema.add("Trade", ColumnType::Text, &[]).unwrap();
        let cost = schema.add("Cost", ColumnType::Number, &[]).unwrap();
        let phase = schema
            .add("Phase", ColumnType::Choice, &["Enabling", "Main works"])
            .unwrap();
        let col = |id: &str| schema.get(id).unwrap().clone();
        go!(
            ed,
            set_column(ed.set(), &id, &col(&trade), Value::text("Drainage"), when).unwrap()
        );
        go!(
            ed,
            set_column_text(ed.set(), &id, &col(&cost), "1250,5", when).unwrap()
        );
        go!(
            ed,
            set_column_text(ed.set(), &id, &col(&phase), "main works", when).unwrap()
        );
        assert_eq!(
            column_value(&get(&ed), &col(&trade)),
            Value::text("Drainage")
        );
        assert_eq!(column_value(&get(&ed), &col(&cost)), Value::number(1250.5));
        assert_eq!(
            column_value(&get(&ed), &col(&phase)),
            Value::text("Main works")
        );
        // What does not fit the column is refused with the reason, and nothing changes.
        let before = ed.model();
        for bad in [
            set_column_text(ed.set(), &id, &col(&cost), "a lot", when),
            set_column_text(ed.set(), &id, &col(&phase), "Later", when),
            set_column(ed.set(), &id, &col(&trade), Value::number(3.0), when),
        ] {
            let Err(EditError::Unsupported { reason, .. }) = bad else {
                panic!("{bad:?}")
            };
            assert!(!reason.is_empty());
        }
        assert_eq!(ed.model(), before);
        // The same value again asks for nothing.
        let same = set_column(ed.set(), &id, &col(&trade), Value::text("Drainage"), when);
        assert!(!ed.execute(same.unwrap()).unwrap().changed);

        // Clearing everything leaves no private data behind.
        go!(
            ed,
            set_column(ed.set(), &id, &col(&trade), Value::Empty, when).unwrap()
        );
        go!(ed, clear_column(ed.set(), &id, &cost, when).unwrap());
        go!(
            ed,
            set_column_text(ed.set(), &id, &col(&phase), " ", when).unwrap()
        );
        assert_eq!(get(&ed).app_data, None);
        go!(ed, toggle_locked(ed.set(), &id, when).unwrap());
        assert!(!is_locked(&get(&ed)));
    }

    #[test]
    fn popups_are_made_once_linked_both_ways_and_undone_whole() {
        let (mut ed, id, o) = editor();
        let when = PdfDate::from_unix(1_700_000_500);
        let (command, popup) = ensure_popup(ed.set(), &id, PAGE, &o).unwrap();
        go!(ed, command.expect("the square has no popup yet"));
        assert_eq!(ed.set().get(&id).unwrap().popup.as_ref(), Some(&popup));
        let p = ed.set().get(&popup).unwrap();
        assert_eq!(
            (p.kind.clone(), p.parent.as_ref()),
            (AnnotKind::Popup, Some(&id))
        );
        assert_eq!(PAGE.union(p.geometry.bounds()), PAGE);
        assert_eq!(ed.undo_len(), 2, "one step for the popup and the link");
        // Asking again gives the same popup and nothing to do.
        assert_eq!(
            ensure_popup(ed.set(), &id, PAGE, &o).unwrap(),
            (None, popup.clone())
        );

        go!(ed, set_popup_open(ed.set(), &id, true, when).unwrap());
        assert!(ed.set().get(&popup).unwrap().open);
        assert!(!ed.set().get(&id).unwrap().open);
        let rect = Rect::new(300.0, 300.0, 420.0, 380.0);
        go!(ed, set_popup_rect(ed.set(), &id, rect, when).unwrap());
        assert_eq!(ed.set().get(&popup).unwrap().geometry.bounds(), rect);
        go!(ed, set_popup_open(ed.set(), &popup, false, when).unwrap());

        // Undoing the creation removes the popup and the link together.
        for _ in 0..4 {
            ed.undo().unwrap();
        }
        assert!(!ed.set().contains(&popup));
        assert_eq!(ed.set().get(&id).unwrap().popup, None);
        assert!(matches!(
            set_popup_rect(ed.set(), &id, rect, when),
            Err(EditError::Unsupported { .. })
        ));

        // A note without a popup carries /Open itself; a popup has no popup.
        let note = Annotation::note(0, Rect::new(10.0, 10.0, 28.0, 28.0), "n", &o);
        let note_id = note.id.clone();
        ed.execute(Command::add(note)).unwrap();
        go!(ed, set_popup_open(ed.set(), &note_id, true, when).unwrap());
        assert!(ed.set().get(&note_id).unwrap().open);
        let link = Annotation::link(
            0,
            Rect::new(0.0, 0.0, 9.0, 9.0),
            pl_annot::LinkTarget::Page {
                page: 1,
                at: Some(Point::new(0.0, 0.0)),
            },
            &o,
        );
        let link_id = link.id.clone();
        ed.execute(Command::add(link)).unwrap();
        assert!(matches!(
            ensure_popup(ed.set(), &link_id, PAGE, &o),
            Err(EditError::Unsupported { .. })
        ));
    }

    #[test]
    fn text_a_text_box_cannot_show_is_refused_by_name_and_nothing_changes() {
        let o = origin("Ada", 1_700_000_000);
        let mut ed = Editor::new(AnnotSet::new(), 1);
        let text_box = Annotation::free_text(0, Rect::new(50.0, 50.0, 250.0, 120.0), "Falls", &o);
        let id = text_box.id.clone();
        ed.execute(Command::add(text_box)).unwrap();
        let when = PdfDate::from_unix(1_700_000_500);
        go!(
            ed,
            set_text(ed.set(), &id, "Falls 1:60 min.", when).unwrap()
        );
        let before = ed.model();
        let command =
            set_text(ed.set(), &id, "Gef\u{00e4}lle \u{2192} 1:60 \u{4e2d}", when).unwrap();
        let err = ed.execute(command).unwrap_err();
        let EditError::Unsupported { id: named, reason } = &err else {
            panic!("{err:?}")
        };
        assert_eq!(named, &id);
        assert!(
            reason.contains('\u{2192}'),
            "the character is named: {reason}"
        );
        assert_eq!(ed.model(), before);
        // The same text is fine for a shape's comment, which is not drawn.
        let square = Annotation::square(0, Rect::new(0.0, 0.0, 9.0, 9.0), &o);
        let square_id = square.id.clone();
        ed.execute(Command::add(square)).unwrap();
        go!(
            ed,
            set_text(ed.set(), &square_id, "\u{2192} \u{4e2d}", when).unwrap()
        );
        // A new text box with such text is refused as well.
        let bad = Annotation::free_text(0, Rect::new(0.0, 0.0, 90.0, 30.0), "\u{4e2d}", &o);
        assert!(matches!(
            ed.execute(Command::add(bad)),
            Err(EditError::Unsupported { .. })
        ));
    }
}
