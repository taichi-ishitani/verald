//! Random stability, as UVM's: every component and every started sequence
//! has its own seed source, derived from the test's seed and its names, and
//! entered while it runs, so that the seeds its objects take do not depend
//! on the rest of the testbench.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use veryl_tbf_random::{SeedSource, derive_seed};

thread_local! {
    static TEST_SEED: Cell<u64> = const { Cell::new(0) };
    /// How many sources each name has made, so that repeated sequences of
    /// one name differ while staying reproducible (UVM's per-string count).
    static COUNTS: RefCell<HashMap<String, u64>> = RefCell::new(HashMap::new());
}

/// Starts a test's seeding from its seed.
pub(crate) fn begin(seed: u64) {
    TEST_SEED.with(|s| s.set(seed));
    COUNTS.with(|c| c.borrow_mut().clear());
}

pub(crate) fn test_seed() -> u64 {
    TEST_SEED.with(Cell::get)
}

/// The source of a component: from its type name and its full path (UVM's
/// `uvm_create_random_seed(type_name, full_name)`).
pub(crate) fn component(type_name: &str, path: &str) -> SeedSource {
    SeedSource::new(derive_seed(test_seed(), &[type_name, path]))
}

/// The source of a sequence started on `sequencer`: from its name and the
/// sequencer's path, and the count of those started before it under them.
pub(crate) fn sequence(name: &str, sequencer: &str) -> SeedSource {
    let key = format!("{sequencer}.{name}");
    let count = COUNTS.with(|c| {
        let mut c = c.borrow_mut();
        let count = c.entry(key.clone()).or_insert(0);
        *count += 1;
        *count
    });
    SeedSource::new(derive_seed(test_seed(), &[name, &key, &count.to_string()]))
}
