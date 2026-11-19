//! TEMPORARY event-trace logger for the Windows Terminal clipboard fixture.
//! Enabled only when `CW_PASTE_DEBUG_LOG` points at a writable file path.

use std::io::Write;
use std::sync::OnceLock;
use std::time::Instant;

pub fn log(line: &str) {
    static START: OnceLock<Instant> = OnceLock::new();
    let Some(path) = std::env::var_os("CW_PASTE_DEBUG_LOG") else {
        return;
    };
    let start = START.get_or_init(Instant::now);
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{:>9} {}", start.elapsed().as_millis(), line);
    }
}
