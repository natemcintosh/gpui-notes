//! Journals mode: today's journal, then older ones beneath it, newest first,
//! loaded a few days at a time as the list scrolls (like Logseq's Journals
//! page).
//!
//! Each day is an `OutlineScope::Embedded` `OutlineView` over the journal's
//! `Page`, so blocks are edited in place and saved through the registry like
//! any other page. Only dates with a journal on disk are listed (plus today),
//! so scrolling never creates empty files for the days in between.

use std::io;

use chrono::NaiveDate;
use gpui::{
    div, point, px, AppContext, BorrowAppContext, Context, Entity, FontWeight, InteractiveElement,
    IntoElement, ParentElement, Pixels, Render, ScrollHandle, StatefulInteractiveElement, Styled,
    Subscription, Window,
};

use crate::errors;
use crate::journal;
use crate::outline_view::OutlineView;
use crate::page::Page;
use crate::registry::{CurrentPage, PageRegistry};
use crate::theme;

/// How many days each load adds to the bottom of the list.
pub const DAYS_PER_LOAD: usize = 3;

/// Load the next batch once less than this much content is left below the
/// viewport, so the next days are usually in place before they scroll in.
const LOAD_MORE_MARGIN: Pixels = px(600.);

/// Today's entry is at least this tall, leaving a large gap above yesterday
/// while today is still short.
const TODAY_MIN_HEIGHT: Pixels = px(400.);

struct Day {
    date: NaiveDate,
    page: Entity<Page>,
    view: Entity<OutlineView>,
}

pub struct JournalsView {
    /// The date the list was built for; `None` until journals mode is first
    /// shown. Rebuilt when journals mode is re-entered on a later day.
    today: Option<NaiveDate>,
    days: Vec<Day>,
    /// Dates still to load, oldest first so `pop` yields the next (newer) one.
    pending: Vec<NaiveDate>,
    scroll: ScrollHandle,
    /// A load or re-check is queued; see `load_more_if_near_end`.
    check_scheduled: bool,
    _current_sub: Subscription,
}

impl JournalsView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let current_sub = cx.observe_global::<CurrentPage>(Self::refresh_if_shown);
        let mut this = Self {
            today: None,
            days: Vec::new(),
            pending: Vec::new(),
            scroll: ScrollHandle::new(),
            check_scheduled: false,
            _current_sub: current_sub,
        };
        this.refresh_if_shown(cx);
        this
    }

    /// Builds the list the first time journals mode is shown, and again if the
    /// date has rolled over since, so today is always on top. Otherwise the
    /// loaded days and scroll position are kept for back/forward.
    fn refresh_if_shown(&mut self, cx: &mut Context<Self>) {
        let today = journal::today();
        if !cx.global::<CurrentPage>().is_journals() || self.today == Some(today) {
            return;
        }
        self.today = Some(today);
        let on_disk = cx
            .global::<PageRegistry>()
            .list_journals()
            .unwrap_or_else(|err| {
                errors::report(format!("list journals failed: {err}"), cx);
                Vec::new()
            });
        self.pending = journal::journal_dates_desc(on_disk, today);
        self.pending.reverse();
        self.days.clear();
        self.scroll.set_offset(point(px(0.), px(0.)));
        self.load_more(cx);
    }

    fn load_more(&mut self, cx: &mut Context<Self>) {
        for _ in 0..DAYS_PER_LOAD {
            let Some(date) = self.pending.pop() else {
                break;
            };
            let page = cx.update_global::<PageRegistry, io::Result<Entity<Page>>>(|reg, cx| {
                reg.open_or_create_journal(date, cx)
            });
            match page {
                Ok(page) => {
                    let view = cx.new(|cx| OutlineView::embedded(page.clone(), cx));
                    self.days.push(Day { date, page, view });
                }
                Err(err) => errors::report(format!("open journal {date} failed: {err}"), cx),
            }
        }
        cx.notify();
    }

    /// Queues the next batch once the bottom of the list nears the viewport.
    /// Called from `render`, which runs on every scroll (the scroll container
    /// notifies this view).
    fn load_more_if_near_end(&mut self, cx: &mut Context<Self>) {
        if self.pending.is_empty() || self.check_scheduled {
            return;
        }
        // The handle describes the last layout. Until that layout includes
        // every loaded day its extent is stale, so render again once this
        // frame is laid out; that is also what keeps loading while the days
        // fit on screen and there is nothing to scroll. A spawned task runs
        // after the current frame, unlike a deferred effect.
        let stale = self.scroll.children_count() != self.days.len();
        let near_end =
            !stale && self.scroll.max_offset().y + self.scroll.offset().y <= LOAD_MORE_MARGIN;
        if !stale && !near_end {
            return;
        }
        self.check_scheduled = true;
        cx.spawn(async move |this, cx| {
            this.update(cx, |this, cx| {
                this.check_scheduled = false;
                if near_end {
                    this.load_more(cx);
                } else {
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Scrolls back to the top and focuses today's first block. Returns
    /// `false` if there was no day to focus.
    pub fn focus_today(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        // Focus is parked in the same update that switched to journals mode,
        // before the `CurrentPage` observer has built the list.
        self.refresh_if_shown(cx);
        self.scroll.set_offset(point(px(0.), px(0.)));
        let Some(today) = self.days.first() else {
            return false;
        };
        today
            .view
            .update(cx, |view, cx| view.focus_first_block(window, cx));
        true
    }

    /// The loaded journals, newest first.
    pub fn pages(&self) -> impl Iterator<Item = (NaiveDate, &Entity<Page>)> {
        self.days.iter().map(|day| (day.date, &day.page))
    }

    /// See `OutlineView::debug_focus_status`.
    #[must_use]
    pub fn debug_focus_status(
        &self,
        window: &Window,
        cx: &gpui::App,
    ) -> (&'static str, &'static str) {
        self.days
            .iter()
            .map(|day| day.view.read(cx).debug_focus_status(window, cx))
            .find(|(target, _)| *target != "none")
            .unwrap_or(("none", "no block"))
    }
}

impl Render for JournalsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.load_more_if_near_end(cx);

        // Each day is one direct child, which `load_more_if_near_end` relies
        // on when comparing the handle's child count to `days`.
        let mut list = div()
            .id("journals")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll);
        for (i, day) in self.days.iter().enumerate() {
            let title = div()
                .text_size(px(32.))
                .font_weight(FontWeight::BOLD)
                .text_color(theme::fg())
                .child(day.date.format("%Y-%m-%d").to_string());
            let mut entry = div()
                .flex()
                .flex_col()
                .flex_none()
                .w_full()
                .max_w(theme::CONTENT_MAX_WIDTH)
                .mx_auto()
                .gap_3()
                .pb_8()
                .child(title)
                .child(day.view.clone());
            entry = if i == 0 {
                entry.min_h(TODAY_MIN_HEIGHT)
            } else {
                entry
                    .pt_8()
                    .border_t_1()
                    .border_color(theme::heading_rule())
            };
            list = list.child(entry);
        }
        list
    }
}
