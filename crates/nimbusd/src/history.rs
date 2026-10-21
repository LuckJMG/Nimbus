use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;

use nimbus_ipc::{Entry, Kind};

/// The word for each kind in the file. The table is the only place that spells
/// them, and both directions read it.
const KINDS: [(Kind, &str); 3] = [
    (Kind::Uploaded, "uploaded"),
    (Kind::Downloaded, "downloaded"),
    (Kind::Deleted, "deleted"),
];

/// The files that the daemon synced, newest last.
///
/// The file holds one line for each entry: the time, the kind, the folder, and
/// the name, split by tabs. A line is complete only when it ends with a
/// newline. The daemon is the only writer.
pub struct History {
    file: PathBuf,
    entries: VecDeque<Entry>,
    /// The file, open for appends. A rewrite replaces the file, so the handle
    /// is dropped then and opened again by the next append.
    out: Option<File>,
    /// The last write failed. The daemon reports a failure once, so a full disk
    /// does not print one line for each entry.
    failed: bool,
}

impl History {
    /// Reads the file and applies both limits. The first bad line ends the
    /// usable part of the file, and the daemon drops the rest and rewrites the
    /// file. A missing file gives an empty history and creates nothing.
    pub fn open(file: PathBuf, age_hours: u64, count: u64) -> Self {
        let mut history = Self {
            file,
            entries: VecDeque::new(),
            out: None,
            failed: false,
        };
        let bytes = match fs::read(&history.file) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return history,
            Err(err) => {
                eprintln!(
                    "nimbusd: the daemon cannot read {}: {err}",
                    history.file.display()
                );
                return history;
            }
        };
        let mut cut = false;
        for chunk in bytes.split_inclusive(|byte| *byte == b'\n') {
            // A last line with no newline is a write that a crash stopped.
            let entry = chunk
                .strip_suffix(b"\n")
                .and_then(|line| std::str::from_utf8(line).ok())
                .and_then(parse);
            match entry {
                Some(entry) => history.entries.push_back(entry),
                None => {
                    cut = true;
                    break;
                }
            }
        }
        let read = history.entries.len();
        history.trim(age_hours, count);
        if cut || history.entries.len() != read {
            history.rewrite();
        }
        history
    }

    /// Adds one entry and writes its line. The list drops its oldest entry at
    /// once when it passes `count`. The file waits for the next prune.
    ///
    /// The tabs and the newlines in the name and in the folder become spaces,
    /// so the list and the file hold the same text, and the line keeps its
    /// four fields.
    pub fn append(&mut self, mut entry: Entry, count: u64) {
        entry.name = entry.name.replace(['\t', '\n'], " ");
        entry.folder = entry.folder.replace(['\t', '\n'], " ");
        let result = self.write_line(&line(&entry));
        self.report(result);
        self.entries.push_back(entry);
        self.trim_count(count);
    }

    /// Drops the entries that are too old or too many, and rewrites the file.
    /// The age counts from the newest entry, so a quiet day removes nothing.
    pub fn prune(&mut self, age_hours: u64, count: u64) {
        self.trim(age_hours, count);
        self.rewrite();
    }

    pub fn entries(&self) -> &VecDeque<Entry> {
        &self.entries
    }

    /// The entries with a time equal to or newer than `time`, in file order. The
    /// time has a resolution of one second, so a strict compare loses an entry
    /// that arrives in the same second as the newest entry that a client holds.
    /// The filter reads the whole list, so a clock step that left the list out
    /// of order does not hide an entry.
    pub fn since(&self, time: u64) -> Vec<Entry> {
        self.entries
            .iter()
            .filter(|entry| entry.time >= time)
            .cloned()
            .collect()
    }

    fn trim(&mut self, age_hours: u64, count: u64) {
        if let Some(newest) = self.entries.back().map(|entry| entry.time) {
            let oldest = newest.saturating_sub(age_hours.saturating_mul(3600));
            while self
                .entries
                .front()
                .is_some_and(|entry| entry.time < oldest)
            {
                self.entries.pop_front();
            }
        }
        self.trim_count(count);
    }

    fn trim_count(&mut self, count: u64) {
        while self.entries.len() as u64 > count {
            self.entries.pop_front();
        }
    }

    /// Sends the whole line in one call, so a crash leaves either the full
    /// line or a line with no newline.
    fn write_line(&mut self, line: &str) -> io::Result<()> {
        let out = match self.out.as_mut() {
            Some(out) => out,
            None => {
                if let Some(dir) = self.file.parent() {
                    fs::create_dir_all(dir)?;
                }
                let file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.file)?;
                self.out.insert(file)
            }
        };
        out.write_all(line.as_bytes())
    }

    /// Replaces the file with the list. A write into the file itself would
    /// empty it first, and a crash then loses the history. The function writes
    /// a second file, makes sure that the data is on the disk, and renames the
    /// second file over the first. A reader sees the old file or the new file.
    fn rewrite(&mut self) {
        let result = (|| {
            if let Some(dir) = self.file.parent() {
                fs::create_dir_all(dir)?;
            }
            let next = self.file.with_extension("tmp");
            let text: String = self.entries.iter().map(line).collect();
            let mut out = File::create(&next)?;
            out.write_all(text.as_bytes())?;
            out.sync_all()?;
            fs::rename(&next, &self.file)
        })();
        if result.is_ok() {
            // The open handle points to the file that the rename replaced.
            self.out = None;
        }
        self.report(result);
    }

    fn report(&mut self, result: io::Result<()>) {
        if let Err(err) = &result {
            // The next append opens the file again.
            self.out = None;
            if !self.failed {
                eprintln!(
                    "nimbusd: the daemon cannot write {}: {err}",
                    self.file.display()
                );
            }
        }
        self.failed = result.is_err();
    }
}

fn line(entry: &Entry) -> String {
    let word = KINDS
        .iter()
        .find_map(|(kind, word)| (*kind == entry.kind).then_some(*word))
        .expect("the table holds every kind");
    format!("{}\t{word}\t{}\t{}\n", entry.time, entry.folder, entry.name)
}

/// Reads one line without its newline. Returns nothing when the line has not
/// exactly four fields, when the time is not a number, or when the kind is
/// unknown.
fn parse(line: &str) -> Option<Entry> {
    let mut fields = line.split('\t');
    let (time, word, folder, name) = (
        fields.next()?,
        fields.next()?,
        fields.next()?,
        fields.next()?,
    );
    if fields.next().is_some() || name.is_empty() {
        return None;
    }
    let kind = KINDS
        .iter()
        .find_map(|(kind, spelled)| (*spelled == word).then_some(*kind))?;
    Some(Entry {
        name: String::from(name),
        folder: String::from(folder),
        time: time.parse().ok()?,
        kind,
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    const AGE: u64 = 24;
    const COUNT: u64 = 1000;

    /// A dir under the temp dir, so a test never touches the real state dir.
    fn temp_base(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nimbus-test-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("the test created the dir");
        dir
    }

    fn entry(time: u64, kind: Kind, folder: &str, name: &str) -> Entry {
        Entry {
            name: String::from(name),
            folder: String::from(folder),
            time,
            kind,
        }
    }

    fn text(file: &Path) -> String {
        fs::read_to_string(file).expect("the test read the file")
    }

    fn open(file: &Path) -> History {
        History::open(file.to_path_buf(), AGE, COUNT)
    }

    #[test]
    fn an_entry_survives_a_restart() {
        let base = temp_base("restart");
        let file = base.join("history");
        let mut history = open(&file);
        let want = [
            entry(100, Kind::Uploaded, "dir", "in.txt"),
            entry(101, Kind::Downloaded, "", "out.txt"),
            entry(102, Kind::Deleted, "a/b", "gone.txt"),
        ];
        for entry in want.clone() {
            history.append(entry, COUNT);
        }
        assert_eq!(open(&file).entries(), &want);
        let _ = fs::remove_dir_all(&base);
    }

    // The format is a contract with the next daemon, so a test pins it.
    #[test]
    fn a_line_has_four_fields_and_a_newline() {
        let base = temp_base("line");
        let file = base.join("history");
        open(&file).append(entry(100, Kind::Uploaded, "dir", "in.txt"), COUNT);
        assert_eq!(text(&file), "100\tuploaded\tdir\tin.txt\n");
        let _ = fs::remove_dir_all(&base);
    }

    // The list and the file must hold the same text, or an entry would change
    // at the next restart.
    #[test]
    fn a_tab_in_a_name_becomes_a_space_in_the_list_and_in_the_file() {
        let base = temp_base("tab");
        let file = base.join("history");
        let mut history = open(&file);
        history.append(entry(100, Kind::Uploaded, "a\tb", "c\td.txt"), COUNT);
        let want = || entry(100, Kind::Uploaded, "a b", "c d.txt");
        assert_eq!(history.entries(), &[want()]);
        assert_eq!(open(&file).entries(), &[want()]);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_missing_file_opens_empty_and_creates_nothing() {
        let base = temp_base("missing");
        let file = base.join("state").join("history");
        assert!(open(&file).entries().is_empty());
        assert!(!file.exists());
        assert!(!base.join("state").exists());
        let _ = fs::remove_dir_all(&base);
    }

    // The first bad line ends the usable part. The good line after it is lost
    // too, and the daemon rewrites the file so the bad line does not return.
    #[test]
    fn the_first_bad_line_ends_the_file() {
        let good = "100\tuploaded\tdir\tin.txt\n";
        let bad_lines: [&[u8]; 6] = [
            b"101\tuploaded\tdir\n",
            b"101\tuploaded\tdir\tin.txt\textra\n",
            b"101\tmoved\tdir\tin.txt\n",
            b"soon\tuploaded\tdir\tin.txt\n",
            b"101\tuploaded\tdir\t\n",
            b"101\tuploaded\tdir\t\xff.txt\n",
        ];
        for (n, bad) in bad_lines.iter().enumerate() {
            let base = temp_base(&format!("bad-{n}"));
            let file = base.join("history");
            let mut bytes = Vec::from(good);
            bytes.extend_from_slice(bad);
            bytes.extend_from_slice(b"102\tdeleted\t\tlater.txt\n");
            fs::write(&file, bytes).expect("the test wrote the file");
            let history = open(&file);
            assert_eq!(
                history.entries(),
                &[entry(100, Kind::Uploaded, "dir", "in.txt")],
                "bad line {n}"
            );
            assert_eq!(text(&file), good, "bad line {n}");
            let _ = fs::remove_dir_all(&base);
        }
    }

    // A crash can stop a write after any byte. The last line parses, but it has
    // no newline, so the reader cannot tell a cut name from a whole one.
    #[test]
    fn a_last_line_with_no_newline_is_dropped() {
        let base = temp_base("unterminated");
        let file = base.join("history");
        fs::write(&file, "100\tuploaded\t\ta.txt\n101\tuploaded\t\tb.txt")
            .expect("the test wrote the file");
        assert_eq!(
            open(&file).entries(),
            &[entry(100, Kind::Uploaded, "", "a.txt")]
        );
        assert_eq!(text(&file), "100\tuploaded\t\ta.txt\n");
        let _ = fs::remove_dir_all(&base);
    }

    // The list drops the oldest entry at once. The file keeps its lines until
    // the prune, so a big run cannot pass the count in the list.
    #[test]
    fn the_list_drops_the_oldest_entry_at_once_and_the_file_waits() {
        let base = temp_base("count");
        let file = base.join("history");
        let mut history = open(&file);
        for time in 1..=5 {
            history.append(entry(time, Kind::Uploaded, "", &format!("{time}.txt")), 3);
        }
        let times: Vec<u64> = history.entries().iter().map(|entry| entry.time).collect();
        assert_eq!(times, [3, 4, 5]);
        assert_eq!(text(&file).lines().count(), 5);
        history.prune(AGE, 3);
        assert_eq!(text(&file).lines().count(), 3);
        let _ = fs::remove_dir_all(&base);
    }

    // The age counts from the newest entry, and the entry on the border stays.
    #[test]
    fn the_age_counts_from_the_newest_entry() {
        let base = temp_base("age");
        let file = base.join("history");
        let mut history = open(&file);
        for time in [0, 3600, 7200] {
            history.append(entry(time, Kind::Uploaded, "", "a.txt"), COUNT);
        }
        history.prune(1, COUNT);
        let times: Vec<u64> = history.entries().iter().map(|entry| entry.time).collect();
        assert_eq!(times, [3600, 7200]);
        assert_eq!(text(&file).lines().count(), 2);
        // A quiet day adds no entry, so a prune later removes nothing more.
        history.prune(1, COUNT);
        assert_eq!(history.entries().len(), 2);
        let _ = fs::remove_dir_all(&base);
    }

    // The entry on the border is in the answer, so a client that asks with the
    // time of its newest entry loses nothing from the same second.
    #[test]
    fn since_keeps_the_entry_on_the_border_and_the_file_order() {
        let base = temp_base("since");
        let mut history = open(&base.join("history"));
        for time in [5, 7, 7, 9] {
            history.append(
                entry(time, Kind::Uploaded, "", &format!("{time}.txt")),
                COUNT,
            );
        }
        let times = |since| -> Vec<u64> {
            history
                .since(since)
                .iter()
                .map(|entry| entry.time)
                .collect()
        };
        assert_eq!(times(0), [5, 7, 7, 9]);
        assert_eq!(times(7), [7, 7, 9]);
        assert_eq!(times(8), [9]);
        assert!(times(10).is_empty());
        let _ = fs::remove_dir_all(&base);
    }

    // A clock that steps back leaves an old time after a newer one. The entry
    // must still reach a client that asks for the newer time.
    #[test]
    fn since_finds_an_entry_after_a_clock_step() {
        let base = temp_base("since-step");
        let mut history = open(&base.join("history"));
        for time in [10, 4, 12] {
            history.append(entry(time, Kind::Uploaded, "", "a.txt"), COUNT);
        }
        let times: Vec<u64> = history.since(10).iter().map(|entry| entry.time).collect();
        assert_eq!(times, [10, 12]);
        let _ = fs::remove_dir_all(&base);
    }

    // After the rename, the open handle points to the replaced file. An append
    // through it would write into a file that no dir holds.
    #[test]
    fn an_append_after_a_prune_lands_in_the_new_file() {
        let base = temp_base("handle");
        let file = base.join("history");
        let mut history = open(&file);
        history.append(entry(1, Kind::Uploaded, "", "a.txt"), COUNT);
        history.prune(AGE, COUNT);
        history.append(entry(2, Kind::Uploaded, "", "b.txt"), COUNT);
        let times: Vec<u64> = open(&file).entries().iter().map(|e| e.time).collect();
        assert_eq!(times, [1, 2]);
        let _ = fs::remove_dir_all(&base);
    }

    // A crash in a big run leaves an oversize file. The next start applies the
    // limits and rewrites it.
    #[test]
    fn open_applies_the_limits() {
        let base = temp_base("limits");
        let file = base.join("history");
        let lines: String = (1..=5)
            .map(|time| format!("{time}\tuploaded\t\t{time}.txt\n"))
            .collect();
        fs::write(&file, lines).expect("the test wrote the file");
        let history = History::open(file.clone(), AGE, 2);
        assert_eq!(history.entries().len(), 2);
        assert_eq!(text(&file).lines().count(), 2);
        let _ = fs::remove_dir_all(&base);
    }

    // A full disk must not stop the sync, so a failed write leaves the entry in
    // the list. The parent of the file is a regular file, so no dir can exist.
    #[test]
    fn a_failed_write_keeps_the_entry_in_the_list() {
        let base = temp_base("failed");
        let blocker = base.join("blocker");
        fs::write(&blocker, "").expect("the test wrote the file");
        let mut history = open(&blocker.join("history"));
        history.append(entry(1, Kind::Uploaded, "", "a.txt"), COUNT);
        assert_eq!(history.entries().len(), 1);
        let _ = fs::remove_dir_all(&base);
    }
}
