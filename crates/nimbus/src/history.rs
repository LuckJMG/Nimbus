use std::path::PathBuf;

use nimbus_ipc::Entry;

/// How far before its newest entry the tray asks. The daemon stamps a whole
/// second, so an entry can arrive in the second of the newest one that the tray
/// holds. The overlap returns that entry, and `merge` drops the repeats.
const OVERLAP: u64 = 2;

/// What the worker sends to the window: the list and what the rows need.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    /// The newest entry comes first.
    pub entries: Vec<Entry>,
    /// The synced folder with the tilde expanded. A row links to a folder
    /// inside it.
    pub local: PathBuf,
    pub count: usize,
}

/// The list of the tray. The worker owns it, and the window gets copies.
#[derive(Debug, Default)]
pub struct Watch {
    shown: bool,
    /// The newest entry comes first.
    held: Vec<Entry>,
    local: PathBuf,
    count: usize,
    /// False until the window got a snapshot since it opened.
    sent: bool,
}

impl Watch {
    pub fn is_shown(&self) -> bool {
        self.shown
    }

    /// A window that opens starts from zero. The daemon can have dropped an
    /// entry by age while the window was hidden, and the tray cannot see that.
    pub fn set_shown(&mut self, shown: bool) {
        self.shown = shown;
        self.held.clear();
        self.sent = false;
    }

    /// The time for the next `GetHistory` call.
    pub fn since(&self) -> u64 {
        self.held
            .first()
            .map_or(0, |newest| newest.time.saturating_sub(OVERLAP))
    }

    /// Takes the answer of the daemon. The result is `Some` when the window
    /// needs a new snapshot: the list moved, the settings moved, or the window
    /// holds nothing yet.
    pub fn update(&mut self, fresh: Vec<Entry>, local: PathBuf, count: usize) -> Option<Snapshot> {
        let moved = merge(&mut self.held, fresh, count)
            || local != self.local
            || count != self.count
            || !self.sent;
        self.local = local;
        self.count = count;
        self.sent = true;
        moved.then(|| Snapshot {
            entries: self.held.clone(),
            local: self.local.clone(),
            count,
        })
    }

    /// The daemon does not answer, so the window shows no history. The result
    /// is `Some` only when the window holds rows that must go.
    pub fn lost(&mut self) -> Option<Snapshot> {
        let had_rows = !self.held.is_empty();
        self.held.clear();
        self.sent = false;
        had_rows.then(Snapshot::default)
    }
}

/// Puts the answer of the daemon in front of the list. `fresh` is in file
/// order, oldest first, and it can hold entries that the list has already. The
/// list keeps the newest first and holds at most `count` entries. The result
/// tells whether the list changed.
fn merge(held: &mut Vec<Entry>, fresh: Vec<Entry>, count: usize) -> bool {
    let mut new: Vec<Entry> = fresh.into_iter().filter(|e| !held.contains(e)).collect();
    new.reverse();
    let added = !new.is_empty();
    // One splice moves the old rows once. An insert for each entry would move
    // them once for each entry.
    held.splice(0..0, new);
    let cut = held.len() > count;
    held.truncate(count);
    added || cut
}

#[cfg(test)]
mod tests {
    use nimbus_ipc::Kind;

    use super::*;

    fn entry(name: &str, time: u64) -> Entry {
        Entry {
            name: String::from(name),
            folder: String::new(),
            time,
            kind: Kind::Uploaded,
        }
    }

    fn names(held: &[Entry]) -> Vec<&str> {
        held.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn the_newest_entry_comes_first() {
        let mut held = Vec::new();
        let fresh = vec![entry("a", 1), entry("b", 2), entry("c", 3)];
        assert!(merge(&mut held, fresh, 10));
        assert_eq!(names(&held), ["c", "b", "a"]);
    }

    // The daemon returns the border entries again, because it compares with
    // "equal or newer". The list must hold each one once.
    #[test]
    fn a_repeat_changes_nothing() {
        let mut held = vec![entry("b", 2), entry("a", 1)];
        let fresh = vec![entry("a", 1), entry("b", 2)];
        assert!(!merge(&mut held, fresh, 10));
        assert_eq!(names(&held), ["b", "a"]);
    }

    #[test]
    fn a_new_entry_in_the_second_of_the_newest_is_kept() {
        let mut held = vec![entry("b", 5), entry("a", 4)];
        let fresh = vec![entry("b", 5), entry("c", 5)];
        assert!(merge(&mut held, fresh, 10));
        assert_eq!(names(&held), ["c", "b", "a"]);
    }

    #[test]
    fn the_oldest_entries_go_over_the_count() {
        let mut held = vec![entry("b", 2), entry("a", 1)];
        assert!(merge(&mut held, vec![entry("c", 3)], 2));
        assert_eq!(names(&held), ["c", "b"]);
    }

    // The count can fall while the window is open. The list must follow it with
    // no new entry.
    #[test]
    fn a_lower_count_cuts_the_list() {
        let mut held = vec![entry("c", 3), entry("b", 2), entry("a", 1)];
        assert!(merge(&mut held, Vec::new(), 1));
        assert_eq!(names(&held), ["c"]);
    }

    #[test]
    fn since_backs_off_from_the_newest_entry() {
        let mut watch = Watch::default();
        assert_eq!(watch.since(), 0);
        watch.update(vec![entry("a", 100)], PathBuf::new(), 10);
        assert_eq!(watch.since(), 100 - OVERLAP);
        let mut early = Watch::default();
        early.update(vec![entry("a", 1)], PathBuf::new(), 10);
        assert_eq!(early.since(), 0, "the time must not go below zero");
    }

    #[test]
    fn the_first_answer_after_an_open_always_goes_to_the_window() {
        let mut watch = Watch::default();
        watch.set_shown(true);
        // An empty history still needs a snapshot, so the window can say so.
        assert!(watch.update(Vec::new(), PathBuf::from("/x"), 10).is_some());
        assert!(watch.update(Vec::new(), PathBuf::from("/x"), 10).is_none());
    }

    #[test]
    fn a_moved_folder_or_count_goes_to_the_window() {
        let mut watch = Watch::default();
        watch.update(Vec::new(), PathBuf::from("/x"), 10);
        assert!(watch.update(Vec::new(), PathBuf::from("/y"), 10).is_some());
        assert!(watch.update(Vec::new(), PathBuf::from("/y"), 5).is_some());
    }

    #[test]
    fn an_open_starts_from_zero() {
        let mut watch = Watch::default();
        watch.update(vec![entry("a", 100)], PathBuf::new(), 10);
        watch.set_shown(true);
        assert_eq!(watch.since(), 0);
    }

    #[test]
    fn a_lost_daemon_empties_the_window_once() {
        let mut watch = Watch::default();
        watch.update(vec![entry("a", 100)], PathBuf::new(), 10);
        assert_eq!(watch.lost(), Some(Snapshot::default()));
        assert_eq!(watch.lost(), None);
        // The daemon is back, and the window needs its rows again.
        assert!(watch.update(Vec::new(), PathBuf::new(), 10).is_some());
    }
}
