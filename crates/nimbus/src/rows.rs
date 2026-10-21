//! The rows of the history: the age text, the paths of a row, and the widget.

use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

use gtk::prelude::*;
use gtk::{Align, Box as GtkBox, Image, Label, LinkButton, Orientation, Window, gio, glib, pango};
use nimbus_ipc::{Entry, Kind};

/// The characters that the folder link shows before it cuts the text. The
/// limit keeps the natural width of a row below the width of the window, so a
/// long folder name cannot widen the window. The tooltip holds the full path.
const LINK_CHARS: i32 = 14;

pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

#[derive(Debug, PartialEq)]
enum Age {
    JustNow,
    Minutes(u64),
    Hours(u64),
    Yesterday,
    Date,
}

/// Picks the form of the age. `seconds` is the time since the entry, and
/// `days` counts the calendar days between the day of the entry and today.
/// Minutes come first, so an entry from 23:50 reads "20 minutes ago" at 00:10.
/// After an hour, the calendar decides.
fn bucket(seconds: u64, days: i64) -> Age {
    match seconds {
        0..=59 => Age::JustNow,
        60..=3599 => Age::Minutes(seconds / 60),
        _ => match days {
            ..=0 => Age::Hours(seconds / 3600),
            1 => Age::Yesterday,
            _ => Age::Date,
        },
    }
}

/// The number of days from 1970-01-01 to a day of the proleptic Gregorian
/// calendar. A difference of two results is a count of calendar days, and a
/// change of the clock for summer time cannot move it.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_from_march = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * month_from_march + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The local calendar day of a Unix time, as a day number.
fn local_day(unix: u64) -> Option<i64> {
    let when = glib::DateTime::from_unix_local(i64::try_from(unix).ok()?).ok()?;
    let (year, month, day) = when.ymd();
    Some(days_from_civil(year.into(), month.into(), day.into()))
}

fn plural(value: u64, unit: &str) -> String {
    let suffix = if value == 1 { "" } else { "s" };
    format!("{value} {unit}{suffix} ago")
}

/// States how long ago an entry happened. A clock that moved backwards gives
/// a difference of zero, which reads as "just now". The date takes the form
/// of the language of the desktop.
pub fn age(time: u64, now: u64) -> String {
    let days = match (local_day(now), local_day(time)) {
        (Some(today), Some(day)) => today - day,
        _ => 0,
    };
    match bucket(now.saturating_sub(time), days) {
        Age::JustNow => String::from("just now"),
        Age::Minutes(minutes) => plural(minutes, "minute"),
        Age::Hours(hours) => plural(hours, "hour"),
        Age::Yesterday => String::from("yesterday"),
        Age::Date => i64::try_from(time)
            .ok()
            .and_then(|unix| glib::DateTime::from_unix_local(unix).ok())
            .and_then(|when| when.format("%x").ok())
            .map_or_else(String::new, |text| text.to_string()),
    }
}

/// The folder of an entry in the synced folder. The daemon sends text from
/// rclone, so the path takes only plain parts. A part such as `..`, or a
/// leading slash, would take the path out of the synced folder.
pub fn folder_path(local: &Path, folder: &str) -> PathBuf {
    let mut path = local.to_path_buf();
    path.extend(
        Path::new(folder)
            .components()
            .filter_map(|part| match part {
                Component::Normal(name) => Some(name),
                _ => None,
            }),
    );
    path
}

fn file_path(local: &Path, entry: &Entry) -> PathBuf {
    let name = Path::new(&entry.name).file_name().unwrap_or_default();
    folder_path(local, &entry.folder).join(name)
}

/// The text of the link: the last part of the folder, or the name of the
/// synced folder for a file in its root.
fn folder_name(local: &Path, folder: &str) -> String {
    let last = Path::new(folder)
        .components()
        .rev()
        .find_map(|part| match part {
            Component::Normal(name) => Some(name),
            _ => None,
        });
    last.or_else(|| local.file_name()).map_or_else(
        || String::from("/"),
        |name| name.to_string_lossy().into_owned(),
    )
}

fn kind_text(kind: Kind) -> &'static str {
    match kind {
        Kind::Uploaded => "Uploaded to",
        Kind::Downloaded => "Downloaded to",
        Kind::Deleted => "Deleted from",
    }
}

/// A file from the remote can have any type. A click on a row must not start a
/// program, so an executable file, and a `.desktop` file, open the folder
/// instead. A file manager does not run a plain double click on them either.
fn is_risky(mode: u32, name: &str) -> bool {
    mode & 0o111 != 0 || name.ends_with(".desktop")
}

fn report(result: Result<(), glib::Error>) {
    if let Err(err) = result {
        eprintln!("nimbus: cannot open the file: {err}");
    }
}

fn launcher(path: &Path) -> gtk::FileLauncher {
    gtk::FileLauncher::new(Some(&gio::File::for_path(path)))
}

/// Opens a file with the default program. A file that is gone, as after a
/// delete, does nothing.
pub fn open_file(file: &Path, parent: &Window) {
    let Ok(meta) = file.metadata() else { return };
    let name = file.file_name().unwrap_or_default().to_string_lossy();
    if is_risky(meta.permissions().mode(), &name) {
        launcher(file).open_containing_folder(Some(parent), gio::Cancellable::NONE, report);
    } else {
        launcher(file).launch(Some(parent), gio::Cancellable::NONE, report);
    }
}

/// Opens the folder, and selects the file when it is still there.
fn open_in_folder(file: &Path, folder: &Path, parent: &Window) {
    if file.exists() {
        launcher(file).open_containing_folder(Some(parent), gio::Cancellable::NONE, report);
    } else {
        launcher(folder).launch(Some(parent), gio::Cancellable::NONE, report);
    }
}

/// The path that a click on the row opens. The window keeps the entries, so a
/// row index finds its entry, and this gives the path.
pub fn path_of(local: &Path, entry: &Entry) -> PathBuf {
    file_path(local, entry)
}

/// The number of new entries at the front of `new`, when `new` is `old` with
/// new entries in front and some entries cut from the end. The window then
/// adds that many rows at the top and keeps the rest. The result is `None`
/// when the lists do not fit, and the window must build every row again.
pub fn diff(old: &[Entry], new: &[Entry]) -> Option<usize> {
    let newest = old.first()?;
    let at = new.iter().position(|entry| entry == newest)?;
    let kept = &new[at..];
    // A list that grows past the old end holds entries that no row shows.
    (kept.len() <= old.len() && kept.iter().zip(old).all(|(a, b)| a == b)).then_some(at)
}

/// Builds one row, and returns the label of the age, because the age text must
/// change while the row stays. The folder link is gray and cannot be clicked
/// when `folder_exists` is false, because the window checks the disk once for
/// each folder when it builds the rows.
pub fn build(
    entry: &Entry,
    local: &Path,
    folder_exists: bool,
    now: u64,
    parent: &Window,
) -> (GtkBox, Label) {
    let (content_type, _) = gio::content_type_guess(Some(&entry.name), None::<&[u8]>);
    let icon = Image::from_gicon(&gio::content_type_get_icon(&content_type));
    icon.set_pixel_size(40);
    icon.set_valign(Align::Center);

    let name = Label::new(Some(&entry.name));
    name.set_xalign(0.0);
    name.set_ellipsize(pango::EllipsizeMode::Middle);
    // The natural width of a long name must not widen the window.
    name.set_max_width_chars(1);

    let folder = folder_path(local, &entry.folder);
    let link = LinkButton::with_label(
        &gio::File::for_path(&folder).uri(),
        &folder_name(local, &entry.folder),
    );
    link.add_css_class("nimbus-link");
    link.set_halign(Align::Start);
    link.set_tooltip_text(Some(&folder.to_string_lossy()));
    link.set_sensitive(folder_exists);
    // Breeze draws a link that cannot be clicked in the color of a link, so
    // the class sets the gray.
    if !folder_exists {
        link.add_css_class("nimbus-gone");
    }
    if let Some(text) = link.child().and_downcast::<Label>() {
        text.set_ellipsize(pango::EllipsizeMode::End);
        text.set_max_width_chars(LINK_CHARS);
    }
    let file = file_path(local, entry);
    let parent = parent.clone();
    link.connect_activate_link(move |_| {
        open_in_folder(&file, &folder, &parent);
        glib::Propagation::Stop
    });

    let place = GtkBox::new(Orientation::Horizontal, 4);
    place.add_css_class("nimbus-place");
    place.append(&Label::new(Some(kind_text(entry.kind))));
    place.append(&link);

    let when = Label::new(Some(&age(entry.time, now)));
    when.set_xalign(0.0);
    when.add_css_class("dim-label");
    when.add_css_class("nimbus-when");

    let texts = GtkBox::new(Orientation::Vertical, 0);
    texts.set_hexpand(true);
    texts.set_valign(Align::Center);
    texts.append(&name);
    texts.append(&place);
    texts.append(&when);

    let row = GtkBox::new(Orientation::Horizontal, 12);
    row.set_margin_top(6);
    row.set_margin_bottom(6);
    // The text lines up with the header, which has 12 pixels.
    row.set_margin_start(12);
    row.set_margin_end(12);
    row.append(&icon);
    row.append(&texts);
    (row, when)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_minute_is_just_now() {
        assert_eq!(bucket(0, 0), Age::JustNow);
        assert_eq!(bucket(59, 0), Age::JustNow);
    }

    #[test]
    fn up_to_an_hour_counts_minutes() {
        assert_eq!(bucket(60, 0), Age::Minutes(1));
        assert_eq!(bucket(3599, 0), Age::Minutes(59));
    }

    // An entry from 23:50, seen at 00:10, is from yesterday on the calendar.
    // Minutes come first, so it still reads as minutes.
    #[test]
    fn minutes_win_over_the_calendar() {
        assert_eq!(bucket(1200, 1), Age::Minutes(20));
    }

    #[test]
    fn the_same_day_counts_hours() {
        assert_eq!(bucket(3600, 0), Age::Hours(1));
        assert_eq!(bucket(7 * 3600, 0), Age::Hours(7));
    }

    // Yesterday is the day before on the calendar, not 24 hours.
    #[test]
    fn the_day_before_is_yesterday_and_older_is_a_date() {
        assert_eq!(bucket(3600, 1), Age::Yesterday);
        assert_eq!(bucket(30 * 3600, 1), Age::Yesterday);
        assert_eq!(bucket(30 * 3600, 2), Age::Date);
        assert_eq!(bucket(7 * 86_400, 7), Age::Date);
    }

    #[test]
    fn the_calendar_counts_days() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(1970, 1, 2), 1);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        // 2024 is a leap year.
        assert_eq!(
            days_from_civil(2024, 3, 1) - days_from_civil(2024, 2, 28),
            2
        );
        assert_eq!(
            days_from_civil(2025, 3, 1) - days_from_civil(2025, 2, 28),
            1
        );
        assert_eq!(
            days_from_civil(2026, 1, 1) - days_from_civil(2025, 12, 31),
            1
        );
    }

    #[test]
    fn plural_follows_the_count() {
        assert_eq!(plural(1, "hour"), "1 hour ago");
        assert_eq!(plural(2, "hour"), "2 hours ago");
    }

    // The folder comes from the text of rclone. No part of it may leave the
    // synced folder.
    #[test]
    fn a_folder_stays_inside_the_synced_folder() {
        let local = Path::new("/home/u/Cloud");
        assert_eq!(folder_path(local, ""), Path::new("/home/u/Cloud"));
        assert_eq!(folder_path(local, "a/b"), Path::new("/home/u/Cloud/a/b"));
        assert_eq!(folder_path(local, "../x"), Path::new("/home/u/Cloud/x"));
        assert_eq!(folder_path(local, "/etc"), Path::new("/home/u/Cloud/etc"));
        assert_eq!(folder_path(local, "a/./b"), Path::new("/home/u/Cloud/a/b"));
    }

    #[test]
    fn a_file_stays_inside_its_folder() {
        let local = Path::new("/home/u/Cloud");
        let entry = |folder: &str, name: &str| Entry {
            name: String::from(name),
            folder: String::from(folder),
            time: 0,
            kind: Kind::Uploaded,
        };
        assert_eq!(
            path_of(local, &entry("a", "f.txt")),
            Path::new("/home/u/Cloud/a/f.txt")
        );
        assert_eq!(
            path_of(local, &entry("a", "../f.txt")),
            Path::new("/home/u/Cloud/a/f.txt")
        );
    }

    #[test]
    fn the_link_names_the_last_folder() {
        let local = Path::new("/home/u/Cloud");
        assert_eq!(folder_name(local, "deep/er"), "er");
        assert_eq!(folder_name(local, "docs"), "docs");
        assert_eq!(folder_name(local, ""), "Cloud");
        assert_eq!(folder_name(Path::new("/"), ""), "/");
    }

    #[test]
    fn a_program_does_not_start_from_a_click() {
        assert!(is_risky(0o755, "run.sh"));
        assert!(is_risky(0o644, "app.desktop"));
        assert!(!is_risky(0o644, "report.pdf"));
    }

    fn entry(name: &str) -> Entry {
        Entry {
            name: String::from(name),
            folder: String::new(),
            time: 0,
            kind: Kind::Uploaded,
        }
    }

    fn list(names: &[&str]) -> Vec<Entry> {
        names.iter().map(|name| entry(name)).collect()
    }

    #[test]
    fn new_entries_at_the_front_are_counted() {
        assert_eq!(
            diff(&list(&["b", "a"]), &list(&["d", "c", "b", "a"])),
            Some(2)
        );
    }

    #[test]
    fn a_list_with_no_change_has_no_new_entries() {
        assert_eq!(diff(&list(&["b", "a"]), &list(&["b", "a"])), Some(0));
    }

    #[test]
    fn a_cut_tail_still_fits() {
        assert_eq!(diff(&list(&["c", "b", "a"]), &list(&["d", "c"])), Some(1));
    }

    #[test]
    fn a_list_that_does_not_continue_the_old_one_needs_a_rebuild() {
        // The old newest entry is gone.
        assert_eq!(diff(&list(&["b", "a"]), &list(&["d", "c"])), None);
        // The old newest entry is there, and the rest differs.
        assert_eq!(diff(&list(&["b", "a"]), &list(&["b", "x"])), None);
        // The list grew at the end.
        assert_eq!(diff(&list(&["b"]), &list(&["b", "a"])), None);
    }

    #[test]
    fn an_empty_old_list_needs_a_rebuild() {
        assert_eq!(diff(&[], &list(&["a"])), None);
        assert_eq!(diff(&[], &[]), None);
    }
}
