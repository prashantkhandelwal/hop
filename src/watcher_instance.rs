use std::fs::{File, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use windows::Win32::Foundation::ERROR_SHARING_VIOLATION;

pub struct WatcherInstance {
    _lock: File,
}

impl WatcherInstance {
    pub fn acquire() -> Result<Option<Self>, String> {
        let base = std::env::var_os("LOCALAPPDATA")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .ok_or("LOCALAPPDATA is not set; cannot lock the Hop watcher.")?;
        if !base.is_absolute() {
            return Err("The watcher lock directory must be an absolute path.".into());
        }
        Self::acquire_at(&base.join("hop").join("watcher.lock"))
    }

    fn acquire_at(path: &Path) -> Result<Option<Self>, String> {
        let parent = path
            .parent()
            .ok_or("Watcher lock has no parent directory.")?;
        std::fs::create_dir_all(parent).map_err(|error| {
            format!(
                "Cannot create watcher lock directory {}: {error}",
                parent.display()
            )
        })?;
        // Keep the file in place: deleting it would let another process lock
        // a different file while the first watcher still holds its handle.
        match OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(0)
            .open(path)
        {
            Ok(lock) => Ok(Some(Self { _lock: lock })),
            Err(error) if error.raw_os_error() == Some(ERROR_SHARING_VIOLATION.0 as i32) => {
                Ok(None)
            }
            Err(error) => Err(format!(
                "Cannot acquire watcher lock {}: {error}",
                path.display()
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::sync::{Arc, Barrier};

    #[test]
    fn watcher_lock_is_exclusive_and_reusable_after_exit() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("hop").join("watcher.lock");
        let first = WatcherInstance::acquire_at(&path).unwrap().unwrap();
        assert!(WatcherInstance::acquire_at(&path).unwrap().is_none());
        drop(first);
        assert!(path.exists());
        assert!(WatcherInstance::acquire_at(&path).unwrap().is_some());
    }

    #[test]
    fn simultaneous_watcher_launches_have_one_winner() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("watcher.lock");
        let start = Arc::new(Barrier::new(8));
        let acquired = Arc::new(Barrier::new(8));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let path = path.clone();
                let start = Arc::clone(&start);
                let acquired = Arc::clone(&acquired);
                std::thread::spawn(move || {
                    start.wait();
                    let instance = WatcherInstance::acquire_at(&path);
                    acquired.wait();
                    instance.unwrap().is_some()
                })
            })
            .collect();
        let winners = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .filter(|won| *won)
            .count();
        assert_eq!(winners, 1);
    }

    #[test]
    fn watcher_lock_is_shared_across_processes_and_recovers_after_abrupt_exit() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("watcher.lock");
        let first = WatcherInstance::acquire_at(&path).unwrap().unwrap();
        let probe = |expected| {
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "watcher_instance::tests::watcher_lock_subprocess_probe",
                    "--ignored",
                    "--nocapture",
                ])
                .env("HOP_TEST_WATCHER_LOCK", &path)
                .env("HOP_TEST_EXPECT_LOCK", expected)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "Lock probe failed: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        };
        probe("blocked");
        drop(first);
        probe("acquired");
        assert!(WatcherInstance::acquire_at(&path).unwrap().is_some());
    }

    #[test]
    fn watcher_lock_errors_are_not_reported_as_another_instance() {
        let directory = tempfile::tempdir().unwrap();
        let parent = directory.path().join("not-a-directory");
        std::fs::write(&parent, b"fixture").unwrap();
        assert!(WatcherInstance::acquire_at(&parent.join("watcher.lock")).is_err());
        assert!(WatcherInstance::acquire_at(directory.path()).is_err());
    }

    #[test]
    #[ignore = "subprocess helper invoked with a private lock path by the parent regression test"]
    fn watcher_lock_subprocess_probe() {
        let path =
            std::env::var_os("HOP_TEST_WATCHER_LOCK").expect("Parent must provide a lock path");
        let instance = WatcherInstance::acquire_at(Path::new(&path)).unwrap();
        match std::env::var("HOP_TEST_EXPECT_LOCK").unwrap().as_str() {
            "blocked" => assert!(instance.is_none()),
            "acquired" => {
                assert!(instance.is_some());
                // Bypass Drop to verify that Windows releases the handle.
                std::process::exit(0);
            }
            expected => panic!("Unknown lock probe expectation: {expected}"),
        }
    }
}
