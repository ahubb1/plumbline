//! Popups, replies and review status: the annotations that hang off a markup.
//!
//! * A **popup** is the window a viewer opens for a markup's text. The markup's `/Popup`
//!   refers to it and its `/Parent` refers back.
//! * A **reply** is a Text annotation whose `/IRT` names the markup (or the reply) it answers,
//!   with `/RT /R`.
//! * A **review status** is, as Acrobat writes it, a hidden Text annotation that replies to the
//!   markup and carries `/StateModel (Review)` and `/State (Accepted)` (or `Rejected`,
//!   `Cancelled`, `Completed`, `None`). The most recent one is the markup's status.
//!
//! None of the three is drawn on the page: [`Annotation::draw_list`] is empty for them, and a
//! reply or status is saved with an appearance stream that paints nothing so that viewers
//! which would otherwise invent a note icon for it show nothing either.

use pl_geom::Rect;

use crate::model::{AnnotId, AnnotKind, Annotation, Flags, Geometry, Origin};
use crate::objects::PdfDate;
use crate::set::AnnotSet;

/// The `/StateModel` of a review status.
pub const REVIEW_MODEL: &str = "Review";

/// The `/StateModel` of a checkmark (`Marked` / `Unmarked`): a status annotation, but not a
/// review status.
const MARKED_MODEL: &str = "Marked";

/// A status as a status annotation records it: its `/StateModel` and `/State`. The model is
/// `Review` for the standard review states, and another name for a custom status set (Bluebeam
/// Revu writes its custom sets' states under the set's own model name, such as
/// `BSI_A3TESTSET0001`). Any state name is kept, also one the model is not known to have.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Status {
    pub model: String,
    pub state: String,
}

impl Status {
    pub fn new(model: impl Into<String>, state: impl Into<String>) -> Status {
        Status {
            model: model.into(),
            state: state.into(),
        }
    }

    /// The state `None` of any model: the markup has no status.
    pub fn is_none(&self) -> bool {
        self.state.trim().is_empty() || self.state.eq_ignore_ascii_case("none")
    }
}

/// A status set: a `/StateModel` and the states it is known to have, in the order they were
/// first met.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusSet {
    pub model: String,
    pub states: Vec<String>,
}

impl StatusSet {
    /// The standard review states.
    pub fn review() -> StatusSet {
        StatusSet {
            model: REVIEW_MODEL.to_string(),
            states: ReviewState::ALL
                .iter()
                .map(|s| s.name().to_string())
                .collect(),
        }
    }
}

/// A review status (`/State` under `/StateModel (Review)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ReviewState {
    #[default]
    None,
    Accepted,
    Rejected,
    Cancelled,
    Completed,
}

impl ReviewState {
    pub const ALL: [ReviewState; 5] = [
        ReviewState::None,
        ReviewState::Accepted,
        ReviewState::Rejected,
        ReviewState::Cancelled,
        ReviewState::Completed,
    ];

    /// The `/State` string.
    pub fn name(self) -> &'static str {
        match self {
            ReviewState::None => "None",
            ReviewState::Accepted => "Accepted",
            ReviewState::Rejected => "Rejected",
            ReviewState::Cancelled => "Cancelled",
            ReviewState::Completed => "Completed",
        }
    }

    pub fn from_name(name: &str) -> Option<ReviewState> {
        ReviewState::ALL.into_iter().find(|s| s.name() == name)
    }
}

/// Size of a popup window a new popup gets, in page units.
const POPUP_SIZE: (f64, f64) = (180.0, 120.0);

/// A sensible `/Rect` for the popup of a markup that occupies `anchor`: beside it (to the
/// right, or to the left when there is no room), its top level with the markup's, and kept
/// inside `page_box`.
pub fn default_popup_rect(anchor: Rect, page_box: Rect) -> Rect {
    let page = page_box.abs();
    let (w, h) = (
        POPUP_SIZE.0.min(page.width()),
        POPUP_SIZE.1.min(page.height()),
    );
    let gap = 12.0;
    let mut x0 = anchor.x1 + gap;
    if x0 + w > page.x1 {
        x0 = anchor.x0 - gap - w;
    }
    let x0 = x0.clamp(page.x0, (page.x1 - w).max(page.x0));
    let y1 = anchor.y1.clamp((page.y0 + h).min(page.y1), page.y1);
    Rect::new(x0, y1 - h, x0 + w, y1)
}

/// The small rectangle a reply or status is given: the top-left corner of what it replies to.
fn reply_rect(parent: &Annotation) -> Rect {
    let b = parent.bounds();
    let side = 18.0;
    Rect::new(b.x0, b.y1 - side, b.x0 + side, b.y1)
}

impl Annotation {
    /// A reply to `parent` (a markup or another reply), on the same page: a Text annotation
    /// with `/IRT` and `/RT /R`. Add it to the set after its parent.
    pub fn reply(parent: &Annotation, text: &str, origin: &Origin) -> Annotation {
        let mut a = Annotation::new(
            AnnotKind::Text,
            parent.page,
            Geometry::Rect(reply_rect(parent)),
            origin,
        );
        a.meta.contents = Some(text.to_string());
        a.meta.reply_to = Some(parent.id.clone());
        a.meta.reply_type = Some("R".to_string());
        a.meta.subject = Some(match parent.meta.subject.as_deref() {
            Some(s) if s.starts_with("Re: ") => s.to_string(),
            Some(s) if !s.is_empty() => format!("Re: {s}"),
            _ => "Reply".to_string(),
        });
        a.icon = Some("Comment".to_string());
        a.meta.flags = Flags(Flags::PRINT | Flags::NO_ZOOM | Flags::NO_ROTATE);
        a
    }

    /// A review status for `parent`, set by `origin.author`: a hidden Text annotation that
    /// replies to it with `/StateModel (Review)` and `/State`, the way Acrobat records one.
    /// Add it to the set after its parent; the latest one added is the markup's status
    /// ([`AnnotSet::review_state`]).
    pub fn review_status(parent: &Annotation, state: ReviewState, origin: &Origin) -> Annotation {
        let mut a = Annotation::reply(
            parent,
            &format!("{} set by {}", state.name(), origin.author),
            origin,
        );
        a.meta.state = Some(state.name().to_string());
        a.meta.state_model = Some(REVIEW_MODEL.to_string());
        a.meta.subject = Some("Status".to_string());
        a.meta.flags = Flags(Flags::HIDDEN | Flags::PRINT | Flags::NO_ZOOM | Flags::NO_ROTATE);
        a
    }

    /// True for an annotation that answers another one (`/IRT` with `/RT` absent or `R`) and
    /// is not a status.
    pub fn is_reply(&self) -> bool {
        self.meta.reply_to.is_some()
            && self.meta.state.is_none()
            && self.meta.reply_type.as_deref().is_none_or(|t| t == "R")
    }

    /// A status annotation for `parent` under any state model (the review model or a custom
    /// status set's), set by `origin.author`: as [`Annotation::review_status`], with
    /// `/StateModel (model)` and `/State (state)`.
    pub fn status_reply(
        parent: &Annotation,
        model: &str,
        state: &str,
        origin: &Origin,
    ) -> Annotation {
        let mut a = Annotation::review_status(parent, ReviewState::None, origin);
        a.meta.state = Some(state.to_string());
        a.meta.state_model = Some(model.to_string());
        a.meta.contents = Some(format!("{state} set by {}", origin.author));
        a
    }

    /// The status this annotation records, when it is a status annotation (`/IRT` with
    /// `/State` and `/StateModel`) of any model but the checkmark's (`Marked`).
    pub fn status_entry(&self) -> Option<Status> {
        self.meta.reply_to.as_ref()?;
        let model = self.meta.state_model.as_deref()?;
        if model == MARKED_MODEL {
            return None;
        }
        Some(Status::new(model, self.meta.state.as_deref()?))
    }

    /// The review status this annotation records, when it is a status annotation of the
    /// Review model.
    pub fn review_state(&self) -> Option<ReviewState> {
        if self.meta.reply_to.is_none() || self.meta.state_model.as_deref() != Some(REVIEW_MODEL) {
            return None;
        }
        ReviewState::from_name(self.meta.state.as_deref()?)
    }
}

impl AnnotSet {
    /// The popup of a markup, created beside it (see [`default_popup_rect`]) when it has none.
    /// Returns the popup's id, or `None` when there is no such annotation or it is of a kind
    /// that has no popup (a Link, a Popup, an unknown subtype).
    pub fn ensure_popup(
        &mut self,
        id: &AnnotId,
        page_box: Rect,
        origin: &Origin,
    ) -> Option<AnnotId> {
        let parent = self.get(id)?;
        if !parent.kind.is_markup() {
            return None;
        }
        if let Some(existing) = parent.popup.clone().filter(|p| self.contains(p)) {
            return Some(existing);
        }
        let rect = default_popup_rect(parent.bounds(), page_box);
        let at = self.position(id)? + 1;
        let parent = self.get_mut(id)?;
        let popup = Annotation::popup_for(parent, rect, origin);
        self.insert(at, popup).ok()
    }

    /// The replies made directly to `id`, oldest first (set order). Status annotations are not
    /// replies.
    pub fn replies(&self, id: &AnnotId) -> Vec<&Annotation> {
        self.iter()
            .filter(|a| a.meta.reply_to.as_ref() == Some(id) && a.is_reply())
            .collect()
    }

    /// Everything that hangs off `id` through `/IRT`: replies, replies to replies and status
    /// annotations, each after what it answers.
    pub fn thread(&self, id: &AnnotId) -> Vec<&Annotation> {
        let mut out: Vec<&Annotation> = Vec::new();
        let mut stack = vec![id.clone()];
        let mut seen = std::collections::HashSet::new();
        while let Some(next) = stack.pop() {
            if !seen.insert(next.clone()) {
                continue;
            }
            let children: Vec<&Annotation> = self
                .iter()
                .filter(|a| a.meta.reply_to.as_ref() == Some(&next))
                .collect();
            for child in children.iter().rev() {
                stack.push(child.id.clone());
            }
            if next != *id {
                if let Some(a) = self.get(&next) {
                    out.push(a);
                }
            }
        }
        out
    }

    /// The status annotations of `id` under the Review model, oldest first: by `/M` when both
    /// have one, otherwise in set order.
    pub fn review_history(&self, id: &AnnotId) -> Vec<&Annotation> {
        let mut found: Vec<(usize, &Annotation)> = self
            .iter()
            .enumerate()
            .filter(|(_, a)| a.meta.reply_to.as_ref() == Some(id) && a.review_state().is_some())
            .collect();
        let stamp = |a: &Annotation| a.meta.modified.map(|d| d.to_unix());
        found.sort_by(|(i, a), (j, b)| match (stamp(a), stamp(b)) {
            (Some(x), Some(y)) if x != y => x.cmp(&y),
            _ => i.cmp(j),
        });
        found.into_iter().map(|(_, a)| a).collect()
    }

    /// The review status of `id`: that of its most recent status annotation,
    /// [`ReviewState::None`] when it has none.
    pub fn review_state(&self, id: &AnnotId) -> ReviewState {
        self.review_history(id)
            .last()
            .and_then(|a| a.review_state())
            .unwrap_or(ReviewState::None)
    }

    /// Every status annotation of `id` under any model but the checkmark's (the review model
    /// and custom status sets alike), oldest first: by `/M` when both have one, otherwise in
    /// set order. This is the markup's full status history.
    pub fn status_history(&self, id: &AnnotId) -> Vec<&Annotation> {
        let mut found: Vec<(usize, &Annotation)> = self
            .iter()
            .enumerate()
            .filter(|(_, a)| a.meta.reply_to.as_ref() == Some(id) && a.status_entry().is_some())
            .collect();
        let stamp = |a: &Annotation| a.meta.modified.map(|d| d.to_unix());
        found.sort_by(|(i, a), (j, b)| match (stamp(a), stamp(b)) {
            (Some(x), Some(y)) if x != y => x.cmp(&y),
            _ => i.cmp(j),
        });
        found.into_iter().map(|(_, a)| a).collect()
    }

    /// The status of `id` under any model: that of its most recent status annotation
    /// ([`AnnotSet::status_history`]); `None` when it has none.
    pub fn status(&self, id: &AnnotId) -> Option<Status> {
        self.status_history(id)
            .last()
            .and_then(|a| a.status_entry())
    }

    /// The custom status sets the document's status annotations use (every `/StateModel` but
    /// `Review` and `Marked`), each with the states met under it, in the order first met.
    pub fn status_sets(&self) -> Vec<StatusSet> {
        let mut sets: Vec<StatusSet> = Vec::new();
        for a in self.iter() {
            let Some(s) = a.status_entry() else {
                continue;
            };
            if s.model == REVIEW_MODEL {
                continue;
            }
            match sets.iter_mut().find(|t| t.model == s.model) {
                Some(set) => {
                    if !set.states.contains(&s.state) {
                        set.states.push(s.state);
                    }
                }
                None => sets.push(StatusSet {
                    model: s.model,
                    states: vec![s.state],
                }),
            }
        }
        sets
    }

    /// `origin` for a new status annotation of `id` under the state model `model`: the same
    /// author, at a time that is later than every status of that model the markup already
    /// has. The latest status is decided by `/M`, so a new one dated before an existing one
    /// (another reviewer's clock runs ahead, or a date written without its time zone) would
    /// not count and setting the status would seem to do nothing.
    pub fn status_origin(&self, id: &AnnotId, model: &str, origin: &Origin) -> Origin {
        let latest = self
            .iter()
            .filter(|a| {
                a.meta.reply_to.as_ref() == Some(id)
                    && a.meta.state.is_some()
                    && a.meta.state_model.as_deref() == Some(model)
            })
            .filter_map(|a| a.meta.modified.map(|d| d.to_unix()))
            .max();
        match latest {
            Some(latest) if latest > origin.when.to_unix() => Origin {
                author: origin.author.clone(),
                when: PdfDate::from_unix(latest.saturating_add(1)),
            },
            _ => origin.clone(),
        }
    }

    /// Records a review status for `id`, set by `origin.author`, and returns the new status
    /// annotation's id. `None` when there is no such annotation. The new status is the
    /// markup's status from then on, whatever the dates of the ones it had
    /// ([`AnnotSet::status_origin`]).
    pub fn set_review_state(
        &mut self,
        id: &AnnotId,
        state: ReviewState,
        origin: &Origin,
    ) -> Option<AnnotId> {
        let parent = self.get(id)?;
        let origin = &self.status_origin(id, REVIEW_MODEL, origin);
        let status = Annotation::review_status(parent, state, origin);
        self.add(status).ok()
    }

    /// Records a status of any model for `id` ([`Annotation::status_reply`]), later than
    /// every status it has, and returns the new status annotation's id.
    pub fn set_status(
        &mut self,
        id: &AnnotId,
        status: &Status,
        origin: &Origin,
    ) -> Option<AnnotId> {
        let parent = self.get(id)?;
        let origin = &self.status_origin_any(id, origin);
        let a = Annotation::status_reply(parent, &status.model, &status.state, origin);
        self.add(a).ok()
    }

    /// `origin` for a new status annotation of `id` that is to be its latest status of every
    /// model but the checkmark's (the status shown is the latest across models).
    pub fn status_origin_any(&self, id: &AnnotId, origin: &Origin) -> Origin {
        let latest = self
            .status_history(id)
            .iter()
            .filter_map(|a| a.meta.modified.map(|d| d.to_unix()))
            .max();
        match latest {
            Some(latest) if latest >= origin.when.to_unix() => Origin {
                author: origin.author.clone(),
                when: PdfDate::from_unix(latest.saturating_add(1)),
            },
            _ => origin.clone(),
        }
    }

    /// Adds a reply to `id` and returns the reply's id. `None` when there is no such
    /// annotation.
    pub fn add_reply(&mut self, id: &AnnotId, text: &str, origin: &Origin) -> Option<AnnotId> {
        let parent = self.get(id)?;
        let reply = Annotation::reply(parent, text, origin);
        self.add(reply).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::objects::PdfDate;
    use pl_geom::Point;

    fn origin(author: &str, at: i64) -> Origin {
        Origin::new(author, PdfDate::from_unix(at))
    }

    const PAGE: Rect = Rect::new(0.0, 0.0, 612.0, 792.0);

    #[test]
    fn a_popup_sits_beside_its_markup_inside_the_page() {
        let r = default_popup_rect(Rect::new(100.0, 600.0, 118.0, 618.0), PAGE);
        assert_eq!(r, Rect::new(130.0, 498.0, 310.0, 618.0));
        // No room on the right: it goes to the left.
        let r = default_popup_rect(Rect::new(560.0, 600.0, 578.0, 618.0), PAGE);
        assert_eq!(r, Rect::new(368.0, 498.0, 548.0, 618.0));
        // Near the bottom it is pushed up to stay on the page; on a tiny page it shrinks.
        let r = default_popup_rect(Rect::new(100.0, 10.0, 118.0, 28.0), PAGE);
        assert_eq!((r.y0, r.y1), (0.0, 120.0));
        let tiny = Rect::new(10.0, 10.0, 110.0, 60.0);
        let r = default_popup_rect(Rect::new(20.0, 20.0, 30.0, 30.0), tiny);
        assert_eq!(tiny.union(r), tiny);
        assert!(r.width() > 0.0 && r.height() > 0.0);
        // An offset page box is honoured.
        let offset = Rect::new(100.0, 200.0, 712.0, 992.0);
        let r = default_popup_rect(Rect::new(700.0, 980.0, 710.0, 990.0), offset);
        assert_eq!(offset.union(r), offset);
    }

    #[test]
    fn ensure_popup_links_both_ways_and_is_idempotent() {
        let o = origin("Ada", 1_700_000_000);
        let mut set = AnnotSet::new();
        let line = Annotation::line(0, Point::new(50.0, 50.0), Point::new(150.0, 90.0), &o);
        let id = set.add(line).unwrap();
        let other = set
            .add(Annotation::square(0, Rect::new(0.0, 0.0, 9.0, 9.0), &o))
            .unwrap();
        let popup = set.ensure_popup(&id, PAGE, &o).unwrap();
        assert_eq!(set.len(), 3);
        assert_eq!(set.get(&id).unwrap().popup.as_ref(), Some(&popup));
        let p = set.get(&popup).unwrap();
        assert_eq!(
            (p.kind.clone(), p.parent.as_ref()),
            (AnnotKind::Popup, Some(&id))
        );
        assert_eq!(set.position(&popup), Some(1), "right after its markup");
        assert_eq!(PAGE.union(p.bounds()), PAGE);
        assert_eq!(set.ensure_popup(&id, PAGE, &o), Some(popup.clone()));
        assert_eq!(set.len(), 3);
        // A popup has no popup; a missing id has none either.
        assert_eq!(set.ensure_popup(&popup, PAGE, &o), None);
        assert_eq!(set.ensure_popup(&AnnotId::from("nope"), PAGE, &o), None);
        assert!(set.get(&other).unwrap().popup.is_none());
    }

    #[test]
    fn a_new_status_counts_although_an_older_one_is_dated_later() {
        // A reviewer whose clock runs ahead (or a date written without its zone) left a
        // status "in the future".
        let now = 1_700_000_000;
        let me = origin("Me", now);
        let mut set = AnnotSet::new();
        let id = set
            .add(Annotation::square(0, Rect::new(0.0, 0.0, 50.0, 50.0), &me))
            .unwrap();
        let ahead = origin("Ahead", now + 6 * 3600);
        let parent = set.get(&id).unwrap().clone();
        set.add(Annotation::review_status(
            &parent,
            ReviewState::Accepted,
            &ahead,
        ))
        .unwrap();
        assert_eq!(set.review_state(&id), ReviewState::Accepted);

        let new = set
            .set_review_state(&id, ReviewState::Rejected, &me)
            .unwrap();
        assert_eq!(set.review_state(&id), ReviewState::Rejected);
        assert_eq!(set.review_history(&id).last().unwrap().id, new);
        let stamped = set.get(&new).unwrap().meta.modified.unwrap().to_unix();
        assert_eq!(stamped, now + 6 * 3600 + 1, "just after the latest one");
        // And again, twice: each is the latest in turn.
        set.set_review_state(&id, ReviewState::Completed, &me)
            .unwrap();
        assert_eq!(set.review_state(&id), ReviewState::Completed);
        set.set_review_state(&id, ReviewState::None, &me).unwrap();
        assert_eq!(set.review_state(&id), ReviewState::None);
        // Another state model's statuses do not move the clock of this one.
        let o = set.status_origin(&id, "Marked", &me);
        assert_eq!(o.when.to_unix(), now);
        // With nothing ahead, the time given is kept.
        let later = origin("Me", now + 24 * 3600);
        assert_eq!(set.status_origin(&id, REVIEW_MODEL, &later), later);
    }

    #[test]
    fn replies_thread_and_status_follows_the_latest() {
        let ada = origin("Ada", 1_700_000_000);
        let bo = origin("Bo", 1_700_000_600);
        let cy = origin("Cy", 1_700_001_200);
        let mut set = AnnotSet::new();
        let square = Annotation::square(1, Rect::new(100.0, 100.0, 200.0, 160.0), &ada);
        let id = set.add(square).unwrap();
        assert_eq!(set.review_state(&id), ReviewState::None);

        let r1 = set.add_reply(&id, "Is this right?", &bo).unwrap();
        let r2 = set.add_reply(&r1, "Yes.", &ada).unwrap();
        let r3 = set.add_reply(&id, "Noted", &cy).unwrap();
        let reply = set.get(&r1).unwrap();
        assert_eq!(reply.kind, AnnotKind::Text);
        assert_eq!(reply.page, 1);
        assert_eq!(reply.meta.reply_type.as_deref(), Some("R"));
        assert_eq!(reply.meta.subject.as_deref(), Some("Re: Rectangle"));
        assert_eq!(reply.meta.author.as_deref(), Some("Bo"));
        assert!(reply.is_reply() && reply.review_state().is_none());
        assert_eq!(
            set.get(&r2).unwrap().meta.subject.as_deref(),
            Some("Re: Rectangle"),
            "no Re: Re:"
        );

        let s1 = set
            .set_review_state(&id, ReviewState::Accepted, &bo)
            .unwrap();
        assert_eq!(set.review_state(&id), ReviewState::Accepted);
        let status = set.get(&s1).unwrap();
        assert_eq!(status.meta.state.as_deref(), Some("Accepted"));
        assert_eq!(status.meta.state_model.as_deref(), Some("Review"));
        assert_eq!(status.meta.contents.as_deref(), Some("Accepted set by Bo"));
        assert!(status.meta.flags.has(Flags::HIDDEN));
        assert!(!status.is_reply());
        set.set_review_state(&id, ReviewState::Rejected, &cy)
            .unwrap();
        assert_eq!(set.review_state(&id), ReviewState::Rejected);
        // As read from a file, an earlier date loses to a later one whatever the set order.
        let parent = set.get(&id).unwrap().clone();
        set.add(Annotation::review_status(
            &parent,
            ReviewState::Completed,
            &ada,
        ))
        .unwrap();
        assert_eq!(set.review_state(&id), ReviewState::Rejected);
        assert_eq!(set.review_history(&id).len(), 3);

        let direct: Vec<&AnnotId> = set.replies(&id).iter().map(|a| &a.id).collect();
        assert_eq!(direct, [&r1, &r3]);
        let thread: Vec<&AnnotId> = set.thread(&id).iter().map(|a| &a.id).collect();
        assert_eq!(thread.len(), 6);
        assert_eq!(
            &thread[..2],
            [&r1, &r2],
            "a reply is followed by its own replies"
        );

        // Removing the markup takes the whole thread with it.
        let removed = set.remove(&id);
        assert_eq!(removed.items.len(), 7);
        assert!(set.is_empty());
        assert_eq!(set.add_reply(&id, "too late", &ada), None);
        for state in ReviewState::ALL {
            assert_eq!(ReviewState::from_name(state.name()), Some(state));
        }
        assert_eq!(ReviewState::from_name("Marked"), None);
    }
}
