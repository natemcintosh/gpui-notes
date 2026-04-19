use std::io;

use chrono::{Local, NaiveDate};
use gpui::{App, BorrowAppContext, Entity};

use crate::page::Page;
use crate::registry::{CurrentPage, PageRegistry};

#[must_use]
pub fn today() -> NaiveDate {
    Local::now().date_naive()
}

#[must_use]
pub fn is_journal_name(s: &str) -> bool {
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .ok()
        .is_some_and(|d| d.format("%Y-%m-%d").to_string() == s)
}

/// Opens (creating if necessary) the journal page for `date`, saves any
/// outgoing current page, and sets the journal as the current page.
///
/// # Errors
/// Returns any I/O error from saving the outgoing page or opening the journal.
pub fn open_for_date(date: NaiveDate, cx: &mut App) -> io::Result<Entity<Page>> {
    let outgoing = cx.global::<CurrentPage>().get().cloned();
    let page = cx.update_global::<PageRegistry, io::Result<Entity<Page>>>(|reg, cx| {
        if let Some(outgoing) = &outgoing {
            reg.save(outgoing, cx)?;
        }
        reg.open_or_create_journal(date, cx)
    })?;
    cx.update_global::<CurrentPage, ()>(|current, _| {
        current.set(Some(page.clone()));
    });
    Ok(page)
}

/// Opens today's journal. Always re-resolves the local date; do not cache the
/// returned entity at startup, or the app will stay on yesterday's page after
/// midnight.
///
/// # Errors
/// Returns any I/O error from saving the outgoing page or opening today's journal.
pub fn open_today(cx: &mut App) -> io::Result<Entity<Page>> {
    open_for_date(today(), cx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case::iso_date("2026-04-18", true)]
    #[case::single_digit_month("2026-4-18", false)]
    #[case::trailing_text("2026-04-18 notes", false)]
    #[case::empty("", false)]
    #[case::page_name("Welcome", false)]
    #[case::underscore_fmt("2026_04_18", false)]
    fn is_journal_name_cases(#[case] input: &str, #[case] expected: bool) {
        assert_eq!(is_journal_name(input), expected);
    }
}
