//! Runtime arguments: the values given to `veryl test` with `--runtime-arg`,
//! read from anywhere in a test, as UVM's command line processor.
//!
//! The host layer takes a copy of them when the component is created; they
//! do not change during the run, so they are read without a context:
//!
//! ```ignore
//! let rounds: u32 = runtime_arg_value("ROUNDS")?.unwrap_or(2);
//! ```
//!
//! The runtime reads its own under the `VERALD_` prefix (see `testbench::start`).

pub use veryl_component::{ArgError, ArgValue, RuntimeArg};

use crate::TestError;
use crate::host;

/// The prefix of the runtime arguments the runtime itself reads.
pub(crate) const PREFIX: &str = "VERALD_";

/// The runtime argument naming the test to start, over the name given to
/// `start`.
pub(crate) const TESTNAME: &str = "VERALD_TESTNAME";

/// The value of the runtime argument `name` that wins (the last one given),
/// or `None` if it was not given.
pub fn runtime_arg(name: &str) -> Option<ArgValue> {
    host::runtime_arg(name)
}

/// Whether the runtime argument `name` was given, as a flag or a value.
pub fn runtime_arg_exists(name: &str) -> bool {
    host::runtime_arg(name).is_some()
}

/// The value of the runtime argument `name` read as `T` (an integer, a
/// `String`, or a type of the testbench's own), or `None` if it was not
/// given. An error if it was given as a flag, or its value cannot be read
/// as `T`.
pub fn runtime_arg_value<T>(name: &str) -> Result<Option<T>, TestError>
where
    T: for<'a> TryFrom<&'a ArgValue, Error = ArgError>,
{
    Ok(host::runtime_arg_value(name)?)
}

/// Every runtime argument as given, in order, overridden ones included.
pub fn runtime_args() -> Vec<RuntimeArg> {
    host::runtime_args()
}

pub(crate) fn error(name: &str, error: &dyn std::fmt::Display) -> TestError {
    TestError::new(format!("runtime argument `{name}`: {error}"))
}

/// Prints the arguments as given on the command line, for the summary.
pub fn format(args: &[RuntimeArg]) -> String {
    args.iter()
        .map(|arg| match &arg.value {
            ArgValue::Flag => arg.name.clone(),
            ArgValue::Text(value) => format!("{}:{value}", arg.name),
        })
        .collect::<Vec<_>>()
        .join(" ")
}
