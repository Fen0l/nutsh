//! A test may pause the clock, or do real I/O. Never both.
//!
//! `tokio::time::pause` auto-advances the clock whenever the runtime has nothing to do, and a
//! runtime waiting on a socket has nothing to do. A paused test that talks to `MockPc` therefore
//! advances into `reqwest`'s own connect timeout and fails with `operation timed out` against
//! loopback — intermittently, because it is a race with the scheduler, and so it passes on one
//! machine and fails on a slower one.
//!
//! The rule is easy to break by copying a neighbouring test, and the failure it produces names a
//! timeout rather than a clock, so this reads the tests and says the rule out loud instead.

use std::path::Path;

/// Test files that pause the clock, and the names no such file may also use.
const PAUSED: &str = "start_paused = true";
const REAL_IO: [&str; 4] = [
    "MockPc",
    "common::client",
    "common::raw_client",
    "pc.requests",
];

/// Every `#[tokio::test(start_paused = true)]` body in a file, by test name.
fn paused_bodies(source: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    for block in source.split(PAUSED).skip(1) {
        // The body runs to the first line that closes at column 0, which is how every test in
        // this workspace is formatted.
        let body = block.split("\n}\n").next().unwrap_or(block);
        let name = body
            .split("async fn ")
            .nth(1)
            .and_then(|rest| rest.split('(').next())
            .unwrap_or("<unnamed>")
            .trim()
            .to_string();
        found.push((name, body.to_string()));
    }
    found
}

#[test]
fn no_test_pauses_the_clock_and_talks_to_a_socket() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut checked = 0;
    let mut offences = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let source =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        // This file names both on purpose.
        if source.contains("no_test_pauses_the_clock_and_talks_to_a_socket") {
            continue;
        }
        for (name, body) in paused_bodies(&source) {
            checked += 1;
            for needle in REAL_IO {
                if body.contains(needle) {
                    offences.push(format!(
                        "{}::{name} pauses the clock and uses `{needle}`",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    ));
                }
            }
        }
    }
    assert!(
        checked > 0,
        "the scan found no paused tests at all, so it is proving nothing"
    );
    assert!(
        offences.is_empty(),
        "a paused clock auto-advances into the request's own timeout; use `common::FakeSource`:\n{}",
        offences.join("\n")
    );
}

/// The scan reads a body the way it claims to, so a `start_paused` test that really did open a
/// socket could not slip past it.
#[test]
fn the_scan_reads_a_paused_body() {
    let source = "
#[tokio::test(start_paused = true)]
async fn offender() {
    let pc = MockPc::builder().start().await;
}

#[tokio::test(start_paused = true)]
async fn innocent() {
    let source = common::FakeSource::new();
}
";
    let bodies = paused_bodies(source);
    assert_eq!(bodies.len(), 2, "{bodies:?}");
    assert_eq!(bodies[0].0, "offender");
    assert!(bodies[0].1.contains("MockPc"));
    assert_eq!(bodies[1].0, "innocent");
    assert!(!bodies[1].1.contains("MockPc"));
}
