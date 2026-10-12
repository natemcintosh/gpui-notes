use chrono::NaiveDate;
use gpui::SharedString;

/// Which note a block lives in: a named page or a dated journal.
#[derive(Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub enum NoteSource {
    Page(SharedString),
    Journal(NaiveDate),
}

#[must_use]
pub fn source_label(source: &NoteSource) -> SharedString {
    match source {
        NoteSource::Page(name) => name.clone(),
        NoteSource::Journal(date) => SharedString::from(date.format("%Y-%m-%d").to_string()),
    }
}
