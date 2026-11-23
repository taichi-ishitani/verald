use std::fmt;

/// An error that fails the test.
///
/// Returned by a task (a fatal error: the run phase ends at once), by
/// building a component, and by a sequence; `?` carries it across all of
/// them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestError {
    message: String,
}

impl TestError {
    pub fn new(message: impl Into<String>) -> Self {
        TestError {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for TestError {}

impl From<String> for TestError {
    fn from(message: String) -> Self {
        TestError::new(message)
    }
}

impl From<&str> for TestError {
    fn from(message: &str) -> Self {
        TestError::new(message)
    }
}

/// A binding failure or an error of the host layer.
impl From<crate::host_layer::HostError> for TestError {
    fn from(error: crate::host_layer::HostError) -> Self {
        TestError::new(error.message())
    }
}

impl From<veryl_component::Error> for TestError {
    fn from(error: veryl_component::Error) -> Self {
        TestError::new(format!("{error:#}"))
    }
}

/// A failed randomization, so that `?` carries it in a task or a sequence.
impl From<veryl_tbf_random::RandError> for TestError {
    fn from(error: veryl_tbf_random::RandError) -> Self {
        TestError::new(error.to_string())
    }
}
