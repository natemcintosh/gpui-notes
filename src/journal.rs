use std::io;

use chrono::{Local, NaiveDate};
use gpui::{App, BorrowAppContext, Entity};

use crate::page::Page;
use crate::registry::{save_outgoing, CurrentPage, PageRegistry};

#[must_use]
pub fn today() -> NaiveDate {
    Local::now().date_naive()
}

#[must_use]
pub fn is_journal_name(s: &str) -> bool {
    parse_journal_name(s).is_some()
}

/// The date a journal title names, if `s` is exactly a journal title
/// (`YYYY-MM-DD`, matching Logseq's `:journal/page-title-format "yyyy-MM-dd"`).
#[must_use]
pub fn parse_journal_name(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .ok()
        .filter(|d| d.format("%Y-%m-%d").to_string() == s)
}

/// Opens (creating if necessary) the journal page for `date`, saves any
/// outgoing current page, and sets the journal as the current page.
///
/// # Errors
/// Returns any I/O error from saving the outgoing page or opening the journal.
pub fn open_for_date(date: NaiveDate, cx: &mut App) -> io::Result<Entity<Page>> {
    save_outgoing(cx)?;
    let page = cx.update_global::<PageRegistry, io::Result<Entity<Page>>>(|reg, cx| {
        reg.open_or_create_journal(date, cx)
    })?;
    cx.update_global::<CurrentPage, ()>(|current, _| {
        current.set(Some(page.clone()));
    });
    Ok(page)
}

/// Switches to journals mode (today's journal, then older ones beneath it),
/// saving whatever was shown before.
///
/// # Errors
/// Returns any I/O error from saving the outgoing page.
pub fn open_journals(cx: &mut App) -> io::Result<()> {
    save_outgoing(cx)?;
    cx.update_global::<CurrentPage, ()>(|current, _| current.show_journals());
    Ok(())
}

/// The dates journals mode lists, newest first: every journal on disk, plus
/// `today` even before its file exists. Days without a journal are skipped
/// rather than shown (and created) empty, and future-dated journals are left
/// out so today stays on top.
#[must_use]
pub fn journal_dates_desc(mut on_disk: Vec<NaiveDate>, today: NaiveDate) -> Vec<NaiveDate> {
    on_disk.retain(|d| *d < today);
    on_disk.push(today);
    on_disk.sort_unstable_by(|a, b| b.cmp(a));
    on_disk.dedup();
    on_disk
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[test]
    fn journal_dates_desc_puts_today_first_and_skips_gaps_and_future() {
        let d = |day| NaiveDate::from_ymd_opt(2026, 10, day).unwrap();
        let on_disk = vec![d(5), d(13), d(9), d(12), d(10)];
        assert_eq!(
            journal_dates_desc(on_disk, d(12)),
            vec![d(12), d(10), d(9), d(5)],
        );
    }

    #[test]
    fn journal_dates_desc_includes_today_without_a_file() {
        let d = |day| NaiveDate::from_ymd_opt(2026, 10, day).unwrap();
        assert_eq!(journal_dates_desc(vec![d(1)], d(12)), vec![d(12), d(1)]);
    }

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
