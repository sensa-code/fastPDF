//! Minimal stderr logger for the `log` facade (spec §31).
//!
//! `FASTPDF_LOG` takes comma-separated directives: a bare level sets the
//! default, `target=level` sets a level for targets starting with `target`
//! (`FASTPDF_LOG=debug`, `FASTPDF_LOG=fastpdf_render=trace,info`). Release
//! builds default to `warn`, debug builds to `info`. No dependency beyond
//! `log` itself.

use std::io::Write;
use std::str::FromStr;
use std::time::Instant;

use log::{LevelFilter, Log, Metadata, Record};

#[derive(Debug, Clone, PartialEq)]
struct Filter {
    default: LevelFilter,
    /// (target prefix, level), longest prefix wins.
    rules: Vec<(String, LevelFilter)>,
}

impl Filter {
    fn parse(spec: Option<&str>, default: LevelFilter) -> Self {
        let mut filter = Self {
            default,
            rules: Vec::new(),
        };
        for part in spec.unwrap_or_default().split(',').map(str::trim) {
            if part.is_empty() {
                continue;
            }
            match part.split_once('=') {
                Some((target, level)) => {
                    if let Ok(level) = LevelFilter::from_str(level.trim()) {
                        // Crate names use '-', module paths '_'.
                        filter.rules.push((target.trim().replace('-', "_"), level));
                    }
                }
                None => {
                    if let Ok(level) = LevelFilter::from_str(part) {
                        filter.default = level;
                    }
                }
            }
        }
        filter
    }

    fn level_for(&self, target: &str) -> LevelFilter {
        self.rules
            .iter()
            .filter(|(prefix, _)| target.starts_with(prefix.as_str()))
            .max_by_key(|(prefix, _)| prefix.len())
            .map_or(self.default, |(_, level)| *level)
    }

    fn max_level(&self) -> LevelFilter {
        self.rules
            .iter()
            .map(|(_, level)| *level)
            .fold(self.default, Ord::max)
    }
}

struct Logger {
    start: Instant,
    filter: Filter,
}

impl Log for Logger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= self.filter.level_for(metadata.target())
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        // Never panic on a closed or missing stderr (GUI subsystem).
        let _ = writeln!(
            std::io::stderr().lock(),
            "{:>9.3} {:<5} {}: {}",
            self.start.elapsed().as_secs_f64(),
            record.level(),
            record.target(),
            record.args()
        );
    }

    fn flush(&self) {
        let _ = std::io::stderr().flush();
    }
}

/// Installs the logger. `spec` is the value of `FASTPDF_LOG`.
pub(crate) fn init(spec: Option<&str>) {
    let default = if cfg!(debug_assertions) {
        LevelFilter::Info
    } else {
        LevelFilter::Warn
    };
    let filter = Filter::parse(spec, default);
    let max = filter.max_level();
    let logger: &'static Logger = Box::leak(Box::new(Logger {
        start: Instant::now(),
        filter,
    }));
    if log::set_logger(logger).is_ok() {
        log::set_max_level(max);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_level_sets_the_default() {
        let f = Filter::parse(Some("debug"), LevelFilter::Warn);
        assert_eq!(f.level_for("gpui::window"), LevelFilter::Debug);
        assert_eq!(f.max_level(), LevelFilter::Debug);
    }

    #[test]
    fn longest_target_prefix_wins() {
        let f = Filter::parse(
            Some("fastpdf-render=trace, fastpdf=info ,warn,bogus=loud"),
            LevelFilter::Error,
        );
        assert_eq!(f.default, LevelFilter::Warn);
        assert_eq!(f.level_for("fastpdf_render::scheduler"), LevelFilter::Trace);
        assert_eq!(f.level_for("fastpdf_ui::reader"), LevelFilter::Info);
        assert_eq!(f.level_for("gpui"), LevelFilter::Warn);
        assert_eq!(f.max_level(), LevelFilter::Trace);
    }

    #[test]
    fn missing_spec_keeps_the_default() {
        let f = Filter::parse(None, LevelFilter::Warn);
        assert_eq!(f.level_for("anything"), LevelFilter::Warn);
        assert!(f.rules.is_empty());
    }
}
