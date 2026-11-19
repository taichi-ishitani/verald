//! Messages, error counts and the test's result.
//!
//! Every message is counted by its severity whatever is printed; what is
//! printed is a threshold (`tb.option("log_level", "info")`), as the `log`
//! crate's `LevelFilter`. Errors decide the result; reaching the error limit
//! (`tb.option("max_errors", n)`) ends the run phase with a fatal error.

use std::cell::RefCell;
use std::fmt;

use veryl_component::Value;
use veryl_tbf_runtime::{TestError, host, sim};

/// A message's severity, most severe first. A fatal error is not one: it is
/// an `Err` returned from `run`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Severity {
    Error,
    Warning,
    Info,
    Debug,
    Trace,
}

impl Severity {
    const ALL: [Severity; 5] = [
        Severity::Error,
        Severity::Warning,
        Severity::Info,
        Severity::Debug,
        Severity::Trace,
    ];

    fn name(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
            Severity::Debug => "debug",
            Severity::Trace => "trace",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Severity::Error => "ERROR",
            Severity::Warning => "WARNING",
            Severity::Info => "INFO",
            Severity::Debug => "DEBUG",
            Severity::Trace => "TRACE",
        }
    }
}

struct Report {
    log_level: Severity,
    max_errors: Option<u64>,
    counts: [u64; 5],
    first_error: Option<String>,
    limit_reached: bool,
}

impl Report {
    fn new() -> Self {
        Report {
            log_level: Severity::Info,
            max_errors: None,
            counts: [0; 5],
            first_error: None,
            limit_reached: false,
        }
    }
}

thread_local! {
    static REPORT: RefCell<Report> = RefCell::new(Report::new());
}

/// A fresh report for a new simulation.
pub(crate) fn init() {
    REPORT.with(|r| *r.borrow_mut() = Report::new());
}

/// Takes the report's options; `Ok(false)` for a key it does not know.
pub(crate) fn option(key: &str, value: &Value) -> Result<bool, TestError> {
    match key {
        "log_level" => {
            let name = value
                .as_str()
                .map_err(|_| TestError::new("`log_level` takes a string"))?;
            let level = Severity::ALL
                .into_iter()
                .find(|s| s.name() == name)
                .ok_or_else(|| {
                    TestError::new(format!(
                        "`log_level` is one of error, warning, info, debug, trace, not `{name}`"
                    ))
                })?;
            REPORT.with(|r| r.borrow_mut().log_level = level);
            Ok(true)
        }
        "max_errors" => {
            let n = value
                .as_u64()
                .map_err(|e| TestError::new(format!("`max_errors`: {e}")))?;
            REPORT.with(|r| r.borrow_mut().max_errors = (n > 0).then_some(n));
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// The options the report takes, for the runtime's unknown-option error.
pub(crate) const OPTIONS: &[&str] = &[
    "`log_level` (string: error, warning, info, debug or trace)",
    "`max_errors` (u64: the errors that end the test; 0 for no limit)",
];

/// Counts a message, prints it if its severity is within the threshold, and
/// ends the run phase if it is the error that reaches the limit.
pub(crate) fn message(severity: Severity, path: &str, text: fmt::Arguments<'_>) {
    let (print, limit) = REPORT.with(|r| {
        let mut r = r.borrow_mut();
        r.counts[severity as usize] += 1;
        let mut limit = None;
        if severity == Severity::Error {
            if r.first_error.is_none() {
                r.first_error = Some(format!("{path}: {text}"));
            }
            let errors = r.counts[Severity::Error as usize];
            if !r.limit_reached && r.max_errors.is_some_and(|max| errors >= max) {
                r.limit_reached = true;
                limit = Some(errors);
            }
        }
        (severity <= r.log_level, limit)
    });
    if print {
        sim::log(&format!(
            "{} @ {} {path}: {text}",
            severity.label(),
            sim::time()
        ));
    }
    if let Some(errors) = limit {
        host::fatal(
            path,
            TestError::new(format!(
                "the error limit is reached ({errors} errors); the last: {text}"
            )),
        );
    }
}

/// Logs the report summary, as UVM's: the runtime arguments given, the
/// counts, and a fatal error if one ended the run.
pub(crate) fn summary(test: &str, seed: u64, fatal: Option<(&str, &TestError)>) {
    let counts = REPORT.with(|r| r.borrow().counts);
    let counted: Vec<String> = Severity::ALL
        .into_iter()
        .map(|s| format!("{} {}", counts[s as usize], s.name()))
        .collect();
    sim::log(&format!(
        "--- report summary: test {test}, seed {seed}, ended at {} ---",
        sim::time()
    ));
    let runtime_args = veryl_tbf_runtime::runtime_args();
    if !runtime_args.is_empty() {
        sim::log(&format!(
            "runtime arguments: {}",
            veryl_tbf_runtime::args::format(&runtime_args)
        ));
    }
    sim::log(&counted.join(", "));
    match fatal {
        Some(("", error)) => sim::log(&format!("fatal: {error}")),
        Some((owner, error)) => sim::log(&format!("fatal: {owner}: {error}")),
        None => {}
    }
}

/// The test's result once it finished: an error if any was reported.
pub(crate) fn result() -> Result<(), TestError> {
    REPORT.with(|r| {
        let r = r.borrow();
        let errors = r.counts[Severity::Error as usize];
        if errors == 0 {
            Ok(())
        } else {
            Err(TestError::new(format!(
                "{errors} error{}; the first: {}",
                if errors == 1 { "" } else { "s" },
                r.first_error.as_deref().unwrap_or("")
            )))
        }
    })
}
