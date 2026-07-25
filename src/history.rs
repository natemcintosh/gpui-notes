use std::io;

use gpui::{App, BorrowAppContext, Global, NavigationDirection};

use crate::journal;
use crate::registry::{set_current_page, CurrentPage, PageRegistry};
use crate::tags::TagSource;

/// Browser-style back/forward stacks over page identities.
///
/// Entries are `TagSource`, not `Entity<Page>`: the registry keys pages by
/// name-or-date, and a page literally named `2026-07-13` is a different page
/// from that date's journal (#40). Storing the identity keeps that distinction
/// through a round trip.
#[derive(Default)]
pub struct NavHistory {
    back: Vec<TagSource>,
    forward: Vec<TagSource>,
}

impl Global for NavHistory {}

impl NavHistory {
    /// Records a move to a new page: `outgoing` becomes the back target and the
    /// forward stack is dropped, since the branch it described is no longer
    /// reachable.
    pub fn record(&mut self, outgoing: Option<TagSource>) {
        if let Some(outgoing) = outgoing {
            self.back.push(outgoing);
        }
        self.forward.clear();
    }

    #[must_use]
    pub fn peek(&self, direction: NavigationDirection) -> Option<&TagSource> {
        self.stack(direction).last()
    }

    /// Completes a back/forward move: drops the entry that was just navigated
    /// to and pushes the page it replaced onto the opposite stack.
    pub fn commit(&mut self, direction: NavigationDirection, outgoing: Option<TagSource>) {
        self.stack_mut(direction).pop();
        if let Some(outgoing) = outgoing {
            self.stack_mut(opposite(direction)).push(outgoing);
        }
    }

    fn stack(&self, direction: NavigationDirection) -> &Vec<TagSource> {
        match direction {
            NavigationDirection::Back => &self.back,
            NavigationDirection::Forward => &self.forward,
        }
    }

    fn stack_mut(&mut self, direction: NavigationDirection) -> &mut Vec<TagSource> {
        match direction {
            NavigationDirection::Back => &mut self.back,
            NavigationDirection::Forward => &mut self.forward,
        }
    }
}

fn opposite(direction: NavigationDirection) -> NavigationDirection {
    match direction {
        NavigationDirection::Back => NavigationDirection::Forward,
        NavigationDirection::Forward => NavigationDirection::Back,
    }
}

/// Opens `target` and records the page it replaced on the back stack.
///
/// Every user-initiated page change goes through here so that history has a
/// single choke point. Startup is the deliberate exception: the first page is
/// not a history entry, so the back stack starts empty.
///
/// # Errors
/// Returns any I/O error from saving the outgoing page or opening `target`.
pub fn navigate_to(target: &TagSource, cx: &mut App) -> io::Result<()> {
    let outgoing = current_target(cx);
    // Opened unconditionally: re-opening the page you are already on still
    // flushes it to disk, and dropping that would silently lose the save.
    open(target, cx)?;
    if outgoing.as_ref() == Some(target) {
        return Ok(());
    }
    cx.update_global::<NavHistory, ()>(|history, _| history.record(outgoing));
    Ok(())
}

/// Moves one step back or forward through history. Returns `false` when that
/// stack is empty and nothing happened.
///
/// # Errors
/// Returns any I/O error from saving the outgoing page or opening the target.
pub fn go(direction: NavigationDirection, cx: &mut App) -> io::Result<bool> {
    // Peeked rather than popped: an open that fails must not consume the entry.
    let Some(target) = cx.global::<NavHistory>().peek(direction).cloned() else {
        return Ok(false);
    };
    let outgoing = current_target(cx);
    open(&target, cx)?;
    cx.update_global::<NavHistory, ()>(|history, _| history.commit(direction, outgoing));
    Ok(true)
}

/// The one place that maps a navigation target onto the two open primitives.
fn open(target: &TagSource, cx: &mut App) -> io::Result<()> {
    match target {
        TagSource::Page(name) => set_current_page(name.as_ref(), cx),
        TagSource::Journal(date) => journal::open_for_date(*date, cx).map(|_| ()),
    }
}

fn current_target(cx: &mut App) -> Option<TagSource> {
    let page = cx.global::<CurrentPage>().get().cloned()?;
    Some(cx.update_global::<PageRegistry, TagSource>(|reg, cx| reg.source_for_page(&page, cx)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use rstest::rstest;

    fn page(name: &str) -> TagSource {
        TagSource::Page(name.to_string().into())
    }

    #[test]
    fn peek_on_empty_history_is_none() {
        let history = NavHistory::default();
        assert!(history.peek(NavigationDirection::Back).is_none());
        assert!(history.peek(NavigationDirection::Forward).is_none());
    }

    #[test]
    fn record_stacks_outgoing_pages_in_visit_order() {
        let mut history = NavHistory::default();
        history.record(Some(page("Home")));
        history.record(Some(page("Tagged")));
        assert_eq!(
            history.peek(NavigationDirection::Back),
            Some(&page("Tagged"))
        );
    }

    #[test]
    fn record_without_an_outgoing_page_is_a_noop() {
        let mut history = NavHistory::default();
        history.record(None);
        assert!(history.peek(NavigationDirection::Back).is_none());
    }

    #[test]
    fn commit_moves_the_outgoing_page_to_the_opposite_stack() {
        let mut history = NavHistory::default();
        history.record(Some(page("Home")));

        // Back from Tagged to Home.
        history.commit(NavigationDirection::Back, Some(page("Tagged")));
        assert!(history.peek(NavigationDirection::Back).is_none());
        assert_eq!(
            history.peek(NavigationDirection::Forward),
            Some(&page("Tagged"))
        );

        // Forward again returns the stacks to their pre-back shape.
        history.commit(NavigationDirection::Forward, Some(page("Home")));
        assert_eq!(history.peek(NavigationDirection::Back), Some(&page("Home")));
        assert!(history.peek(NavigationDirection::Forward).is_none());
    }

    #[test]
    fn recording_a_new_page_clears_the_forward_stack() {
        let mut history = NavHistory::default();
        history.record(Some(page("Home")));
        history.commit(NavigationDirection::Back, Some(page("Tagged")));
        assert!(history.peek(NavigationDirection::Forward).is_some());

        history.record(Some(page("Home")));
        assert!(history.peek(NavigationDirection::Forward).is_none());
    }

    #[rstest]
    #[case::back(NavigationDirection::Back)]
    #[case::forward(NavigationDirection::Forward)]
    fn commit_on_an_empty_stack_still_records_the_outgoing_page(
        #[case] direction: NavigationDirection,
    ) {
        let mut history = NavHistory::default();
        history.commit(direction, Some(page("Home")));
        assert_eq!(history.peek(opposite(direction)), Some(&page("Home")));
    }

    #[test]
    fn journal_and_page_targets_are_distinct_entries() {
        let date = NaiveDate::from_ymd_opt(2026, 7, 13).unwrap();
        let mut history = NavHistory::default();
        history.record(Some(TagSource::Journal(date)));
        assert_ne!(
            history.peek(NavigationDirection::Back),
            Some(&page("2026-07-13"))
        );
    }
}
