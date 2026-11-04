//! Runs this test binary again as a child with its own temporary directory, to see what's left
//! there when it exits.

use std::path::Path;
use std::process::Command;

const CHILD: &str = "KAME_TEST_DIR_CHILD";

/// What the child runs as: writes some files, then does what it was told.
#[test]
fn child() {
    let Ok(how) = std::env::var(CHILD) else {
        return;
    };
    let folder = kame_test_dir::folder("child");
    std::fs::create_dir_all(folder.join("deep/er")).unwrap();
    std::fs::write(folder.join("deep/er/file"), "words").unwrap();
    assert_eq!(how, "pass", "this child was told to fail");
}

fn left_after(how: &str, temp: &Path) -> (bool, Vec<String>) {
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child", "--nocapture"])
        .env(CHILD, how)
        .env("TMPDIR", temp)
        .output()
        .unwrap()
        .status;
    let left = std::fs::read_dir(temp)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    (status.success(), left)
}

#[test]
fn a_passing_run_leaves_nothing() {
    let temp = kame_test_dir::folder("pass");
    assert_eq!(left_after("pass", &temp), (true, Vec::new()));
}

#[test]
fn a_failing_run_leaves_nothing() {
    let temp = kame_test_dir::folder("fail");
    assert_eq!(left_after("fail", &temp), (false, Vec::new()));
}

#[test]
fn a_killed_run_is_cleared_by_a_later_one_but_a_recent_one_is_left() {
    let temp = kame_test_dir::folder("killed");
    let old = temp.join("kame-tests-1-1");
    let recent = temp.join("kame-tests-2-2");
    let other = temp.join("someone-elses");
    for folder in [&old, &recent, &other] {
        std::fs::create_dir_all(folder).unwrap();
    }
    let hours_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 3600);
    for folder in [&old, &other] {
        std::fs::File::open(folder)
            .unwrap()
            .set_modified(hours_ago)
            .unwrap();
    }
    let (passed, mut left) = left_after("pass", &temp);
    left.sort();
    assert!(passed);
    assert_eq!(left, ["kame-tests-2-2", "someone-elses"]);
}
