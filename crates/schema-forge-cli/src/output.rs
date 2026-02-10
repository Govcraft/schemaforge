use console::Term;

use crate::cli::GlobalOpts;
use crate::error::CliError;

/// Output format mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    Human,
    Json,
    Plain,
}

/// Output context derived from global flags.
///
/// Provides methods for printing success, warning, error, and JSON
/// messages respecting the chosen output mode and color settings.
pub struct OutputContext {
    pub mode: OutputMode,
    #[allow(dead_code)]
    pub verbose: u8,
    pub quiet: bool,
    pub use_color: bool,
}

impl OutputContext {
    /// Construct from global CLI options.
    pub fn from_global(global: &GlobalOpts) -> Self {
        let mode = match global.format.as_str() {
            "json" => OutputMode::Json,
            "plain" => OutputMode::Plain,
            _ => OutputMode::Human,
        };

        let use_color = !global.no_color
            && std::env::var("TERM").map_or(true, |t| t != "dumb")
            && Term::stderr().is_term();

        Self {
            mode,
            verbose: global.verbose,
            quiet: global.quiet,
            use_color,
        }
    }

    /// Print a success message to stderr (human mode only, not in quiet mode).
    pub fn success(&self, msg: &str) {
        if self.quiet || self.mode != OutputMode::Human {
            return;
        }
        if self.use_color {
            let style = console::Style::new().green().bold();
            eprintln!("{} {}", style.apply_to("ok"), msg);
        } else {
            eprintln!("ok {msg}");
        }
    }

    /// Print a warning to stderr (not in quiet mode).
    pub fn warn(&self, msg: &str) {
        if self.quiet {
            return;
        }
        match self.mode {
            OutputMode::Human => {
                if self.use_color {
                    let style = console::Style::new().yellow().bold();
                    eprintln!("{} {}", style.apply_to("warning:"), msg);
                } else {
                    eprintln!("warning: {msg}");
                }
            }
            OutputMode::Json => {
                let json = serde_json::json!({ "warning": msg });
                eprintln!("{json}");
            }
            OutputMode::Plain => {
                eprintln!("warning\t{msg}");
            }
        }
    }

    /// Print an error using the appropriate output mode.
    pub fn print_error(&self, err: &CliError) {
        match self.mode {
            OutputMode::Human => {
                if self.use_color {
                    let style = console::Style::new().red().bold();
                    eprintln!("{} {}", style.apply_to("error:"), err);
                } else {
                    eprintln!("error: {err}");
                }
            }
            OutputMode::Json => {
                let json = err.to_json();
                eprintln!("{json}");
            }
            OutputMode::Plain => {
                eprintln!("error\t{err}");
            }
        }
    }

    /// Print JSON data to stdout.
    pub fn print_json(&self, value: &serde_json::Value) {
        if let Ok(s) = serde_json::to_string_pretty(value) {
            println!("{s}");
        }
    }

    /// Print a status message to stderr (human mode only, not in quiet mode).
    pub fn status(&self, msg: &str) {
        if self.quiet || self.mode != OutputMode::Human {
            return;
        }
        eprintln!("{msg}");
    }

    /// Whether to show progress spinners.
    pub fn show_progress(&self) -> bool {
        !self.quiet && self.mode == OutputMode::Human && Term::stderr().is_term()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_global(format: &str, verbose: u8, quiet: bool, no_color: bool) -> GlobalOpts {
        GlobalOpts {
            config: None,
            format: format.into(),
            verbose,
            quiet,
            no_color,
            db_url: None,
            db_ns: None,
            db_name: None,
        }
    }

    #[test]
    fn from_global_human_mode() {
        let global = make_global("human", 0, false, false);
        let ctx = OutputContext::from_global(&global);
        assert_eq!(ctx.mode, OutputMode::Human);
        assert!(!ctx.quiet);
        assert_eq!(ctx.verbose, 0);
    }

    #[test]
    fn from_global_json_mode() {
        let global = make_global("json", 0, false, false);
        let ctx = OutputContext::from_global(&global);
        assert_eq!(ctx.mode, OutputMode::Json);
    }

    #[test]
    fn from_global_plain_mode() {
        let global = make_global("plain", 0, false, false);
        let ctx = OutputContext::from_global(&global);
        assert_eq!(ctx.mode, OutputMode::Plain);
    }

    #[test]
    fn from_global_no_color_disables_color() {
        let global = make_global("human", 0, false, true);
        let ctx = OutputContext::from_global(&global);
        assert!(!ctx.use_color);
    }

    #[test]
    fn from_global_quiet_flag() {
        let global = make_global("human", 0, true, false);
        let ctx = OutputContext::from_global(&global);
        assert!(ctx.quiet);
    }

    #[test]
    fn from_global_verbose_count() {
        let global = make_global("human", 3, false, false);
        let ctx = OutputContext::from_global(&global);
        assert_eq!(ctx.verbose, 3);
    }

    #[test]
    fn show_progress_false_when_quiet() {
        let ctx = OutputContext {
            mode: OutputMode::Human,
            verbose: 0,
            quiet: true,
            use_color: true,
        };
        assert!(!ctx.show_progress());
    }

    #[test]
    fn show_progress_false_when_json() {
        let ctx = OutputContext {
            mode: OutputMode::Json,
            verbose: 0,
            quiet: false,
            use_color: true,
        };
        assert!(!ctx.show_progress());
    }
}
