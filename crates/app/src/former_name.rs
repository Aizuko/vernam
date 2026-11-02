//! The app was called Signal GPUI (bundle ID `org.signal-gpui`), then
//! Ziggy (`io.github.knpwrs.ziggy`), before it was Vernam. On first launch
//! the newest of their data directories moves to Vernam's; the database
//! key's Keychain item is copied by `keys`.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};

/// A name the app had before.
#[derive(Debug, PartialEq, Eq)]
pub struct Former {
    /// As people knew it.
    pub name: &'static str,
    /// Its `ProjectDirs` qualifier, organization and application.
    dirs: (&'static str, &'static str, &'static str),
    /// The Keychain item its database key was kept in.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub keychain_service: &'static str,
    /// Its executable, in a bundle or not, and its log files' prefix.
    executable: &'static str,
}

/// The app's former names, newest first.
pub const FORMER: &[Former] = &[
    Former { name: "Ziggy", dirs: ("io.github", "knpwrs", "Ziggy"), keychain_service: "ziggy", executable: "ziggy" },
    Former {
        name: "Signal GPUI",
        dirs: ("org", "signal-gpui", "Signal GPUI"),
        keychain_service: "signal-gpui",
        executable: "signal-gpui",
    },
];

impl Former {
    /// The data directory the app had under this name.
    pub fn data_dir(&self) -> Option<PathBuf> {
        let (qualifier, organization, application) = self.dirs;
        directories::ProjectDirs::from(qualifier, organization, application).map(|dirs| dirs.data_dir().to_path_buf())
    }

    /// Whether the app is running under this name.
    fn running(&self) -> bool {
        std::process::Command::new("pgrep")
            .args(["-x", self.executable])
            .output()
            .is_ok_and(|output| output.status.success())
    }
}

/// Written into a former data directory as it moves: the former name, for
/// `keys` to take its key, until the key is in place. A launch that moved
/// the data and then failed (a denied Keychain prompt) leaves it for the
/// next.
const MOVED_FROM: &str = "moved-from";

/// The former name whose data moved to `data_dir` and whose key hasn't
/// been loaded since.
pub fn moved_from(data_dir: &Path) -> Option<&'static Former> {
    let name = std::fs::read_to_string(data_dir.join(MOVED_FROM)).ok()?;
    FORMER.iter().find(|former| former.name == name.trim())
}

/// The key for data that moved in is in place: forgets where it came from.
pub fn key_settled(data_dir: &Path) {
    let _ = std::fs::remove_file(data_dir.join(MOVED_FROM));
}

/// Deletes logs written under a former name (`ziggy.2026-10-29.log`) once
/// `keep_days` newer days have logs, as the appender does for its own: it
/// only prunes files with the current prefix.
pub fn prune_former_logs(log_dir: &Path, keep_days: usize) {
    let Ok(entries) = std::fs::read_dir(log_dir) else { return };
    // `<prefix>.<YYYY-MM-DD>.log`
    let logs: Vec<(PathBuf, String, String)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let (prefix, rest) = name.split_once('.')?;
            let date = rest.strip_suffix(".log")?;
            let is_date = date.len() == 10
                && date.bytes().enumerate().all(|(i, b)| if i == 4 || i == 7 { b == b'-' } else { b.is_ascii_digit() });
            is_date.then(|| (entry.path(), prefix.to_owned(), date.to_owned()))
        })
        .collect();
    let mut days: Vec<&str> = logs.iter().map(|(_, _, date)| date.as_str()).collect();
    days.sort_unstable_by(|a, b| b.cmp(a));
    days.dedup();
    let Some(oldest_kept) = days.get(keep_days.saturating_sub(1)).map(|day| day.to_string()) else { return };
    for (path, prefix, date) in &logs {
        if FORMER.iter().any(|former| former.executable == prefix) && *date < oldest_kept {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// The data directories the app had, newest first.
pub fn former_data_dirs() -> Vec<PathBuf> {
    FORMER.iter().filter_map(Former::data_dir).collect()
}

/// What became of a former name's data, for the log once it's open.
#[derive(Debug, PartialEq, Eq)]
pub enum Moved {
    /// There was none.
    Nothing,
    /// It moved here from this directory.
    From(&'static Former, PathBuf),
    /// It's still in this directory: Vernam's existed already, so it was
    /// left for the user to sort out rather than merged.
    LeftIn(&'static Former, PathBuf),
    /// The app is running under that name, with its databases open.
    StillRunning(&'static Former),
}

/// Moves the newest former data directory there is to `data_dir`, when
/// `data_dir` doesn't exist yet. Refuses while the app runs under that
/// name, with an alert: moving its databases from under it would split the
/// data.
pub fn move_data(data_dir: &Path) -> Result<Moved> {
    let formers = FORMER.iter().filter_map(|former| Some((former, former.data_dir()?)));
    let moved = move_newest(formers, data_dir, Former::running)?;
    if let Moved::StillRunning(former) = moved {
        let name = former.name;
        alert(
            &format!("Quit {name} to open Vernam"),
            &format!(
                "{name} is now Vernam. Vernam moves your chats over the first time it opens, \
                 which it can't do while {name} is running. Quit {name}, then open Vernam again."
            ),
        );
        bail!("{name} is still running; quit it before opening Vernam");
    }
    Ok(moved)
}

fn move_newest(
    formers: impl IntoIterator<Item = (&'static Former, PathBuf)>,
    data_dir: &Path,
    running: impl Fn(&Former) -> bool,
) -> Result<Moved> {
    let Some((former, dir)) = formers.into_iter().find(|(_, dir)| dir.exists()) else {
        return Ok(Moved::Nothing);
    };
    if data_dir.exists() {
        return Ok(Moved::LeftIn(former, dir));
    }
    if running(former) {
        return Ok(Moved::StillRunning(former));
    }
    if let Some(parent) = data_dir.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    // Before the rename, so it moves with the data.
    std::fs::write(dir.join(MOVED_FROM), former.name).with_context(|| format!("marking {}", dir.display()))?;
    // All are in Application Support, on one volume: a rename, all at once.
    std::fs::rename(&dir, data_dir).with_context(|| format!("moving {} to {}", dir.display(), data_dir.display()))?;
    Ok(Moved::From(former, dir))
}

#[cfg(target_os = "macos")]
pub fn alert(message: &str, detail: &str) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSAlert, NSApplication};
    use objc2_foundation::NSString;

    let Some(mtm) = MainThreadMarker::new() else { return };
    let app = NSApplication::sharedApplication(mtm);
    // In front, as nothing else of ours is open yet. (`activate` needs
    // macOS 14; the app runs on 13.)
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(message));
    alert.setInformativeText(&NSString::from_str(detail));
    alert.runModal();
}

#[cfg(not(target_os = "macos"))]
pub fn alert(message: &str, detail: &str) {
    eprintln!("{message}: {detail}");
}

#[cfg(test)]
mod tests {
    use super::*;

    const ZIGGY: &Former = &FORMER[0];
    const SIGNAL_GPUI: &Former = &FORMER[1];

    #[test]
    fn the_newest_old_directory_moves_once() {
        let root = tempfile::tempdir().unwrap();
        let ziggy = root.path().join("io.github.knpwrs.Ziggy");
        let signal_gpui = root.path().join("org.signal-gpui.Signal-GPUI");
        let data_dir = root.path().join("new").join("io.github.knpwrs.Vernam");
        for (dir, chats) in [(&ziggy, "ziggy's"), (&signal_gpui, "signal gpui's")] {
            std::fs::create_dir_all(dir.join("logs")).unwrap();
            std::fs::write(dir.join("app.db"), chats).unwrap();
        }
        let formers = || [(ZIGGY, ziggy.clone()), (SIGNAL_GPUI, signal_gpui.clone())];

        assert_eq!(move_newest(formers(), &data_dir, |_| false).unwrap(), Moved::From(ZIGGY, ziggy.clone()));
        assert!(!ziggy.exists());
        // Until its key is in place, it's known where the data came from.
        assert_eq!(moved_from(&data_dir), Some(ZIGGY));
        key_settled(&data_dir);
        assert_eq!(moved_from(&data_dir), None);
        assert_eq!(std::fs::read_to_string(data_dir.join("app.db")).unwrap(), "ziggy's");
        assert!(data_dir.join("logs").is_dir());
        // Next launch: the older one isn't merged into what's there.
        assert_eq!(move_newest(formers(), &data_dir, |_| false).unwrap(), Moved::LeftIn(SIGNAL_GPUI, signal_gpui.clone()));
        assert_eq!(std::fs::read_to_string(data_dir.join("app.db")).unwrap(), "ziggy's");
    }

    #[test]
    fn an_older_name_moves_when_its_the_only_one() {
        let root = tempfile::tempdir().unwrap();
        let signal_gpui = root.path().join("org.signal-gpui.Signal-GPUI");
        let data_dir = root.path().join("new");
        std::fs::create_dir_all(&signal_gpui).unwrap();
        let formers = [(ZIGGY, root.path().join("missing")), (SIGNAL_GPUI, signal_gpui.clone())];
        assert_eq!(move_newest(formers, &data_dir, |_| false).unwrap(), Moved::From(SIGNAL_GPUI, signal_gpui));
        assert!(data_dir.is_dir());
    }

    #[test]
    fn nothing_moves_while_that_app_runs() {
        let root = tempfile::tempdir().unwrap();
        let ziggy = root.path().join("old");
        let data_dir = root.path().join("new");
        std::fs::create_dir_all(&ziggy).unwrap();
        std::fs::write(ziggy.join("app.db"), "chats").unwrap();

        // Only the app whose data would move matters.
        let running = |former: &Former| former == ZIGGY;
        assert_eq!(move_newest([(ZIGGY, ziggy.clone())], &data_dir, running).unwrap(), Moved::StillRunning(ZIGGY));
        assert!(ziggy.join("app.db").exists() && !data_dir.exists());
        let running = |former: &Former| former == SIGNAL_GPUI;
        assert_eq!(move_newest([(ZIGGY, ziggy.clone())], &data_dir, running).unwrap(), Moved::From(ZIGGY, ziggy));
    }

    #[test]
    fn logs_under_a_former_name_go_once_three_newer_days_have_logs() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "signal-gpui.2026-10-22.log",
            "ziggy.2026-10-28.log",
            "ziggy.2026-10-29.log",
            "vernam.2026-10-29.log",
            "vernam.2026-10-30.log",
            "vernam.2026-10-31.log",
            "notes.txt",
            // Not a day: doesn't move the cutoff.
            "crash.report.log",
        ] {
            std::fs::write(dir.path().join(name), "").unwrap();
        }
        prune_former_logs(dir.path(), 3);
        let mut left: Vec<String> =
            std::fs::read_dir(dir.path()).unwrap().flatten().map(|e| e.file_name().into_string().unwrap()).collect();
        left.sort();
        // The three newest days are the 29th to the 31st: Ziggy's log of the
        // 29th stays, older ones of former names go, the rest isn't touched.
        assert_eq!(left, ["crash.report.log", "notes.txt", "vernam.2026-10-29.log", "vernam.2026-10-30.log", "vernam.2026-10-31.log", "ziggy.2026-10-29.log"]);
    }
}
