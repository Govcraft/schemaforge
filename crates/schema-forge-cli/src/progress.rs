use std::time::Duration;

use indicatif::{ProgressBar, ProgressStyle};

/// Create a spinner for ongoing operations.
///
/// The spinner is sent to stderr and ticks every 80ms.
pub fn create_spinner(message: &str) -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.cyan} {msg}")
            .expect("valid spinner template"),
    );
    pb.set_message(message.to_string());
    pb.enable_steady_tick(Duration::from_millis(80));
    pb
}

/// Finish a spinner with a success message.
pub fn finish_spinner(pb: &ProgressBar, message: &str) {
    pb.finish_with_message(message.to_string());
}

/// Finish a spinner with an error message.
pub fn finish_spinner_error(pb: &ProgressBar, message: &str) {
    pb.finish_with_message(format!("ERROR: {message}"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_spinner_returns_progress_bar() {
        let pb = create_spinner("testing...");
        assert!(!pb.is_finished());
        pb.finish();
    }

    #[test]
    fn finish_spinner_completes() {
        let pb = create_spinner("working...");
        finish_spinner(&pb, "done");
        assert!(pb.is_finished());
    }

    #[test]
    fn finish_spinner_error_completes() {
        let pb = create_spinner("working...");
        finish_spinner_error(&pb, "failed");
        assert!(pb.is_finished());
    }
}
