//! Progress bars (SPEC "progress indication"): drawn on stderr, only when
//! stderr is a terminal — pipes/tests/logs stay byte-clean.

use indicatif::{ProgressBar, ProgressStyle};
use std::io::IsTerminal;
use std::time::Duration;

pub fn progress(len: u64, msg: &'static str) -> ProgressBar {
    if !std::io::stderr().is_terminal() {
        return ProgressBar::hidden();
    }
    let pb = ProgressBar::new(len);
    if let Ok(style) = ProgressStyle::with_template("{wide_bar} {pos}/{len} {msg}") {
        pb.set_style(style.progress_chars("=>-"));
    }
    pb.set_message(msg);
    pb.enable_steady_tick(Duration::from_millis(120));
    pb
}
