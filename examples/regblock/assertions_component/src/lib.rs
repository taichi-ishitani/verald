//! The register block's assertions alone, without a testbench: the test
//! module drives the bus itself.

verald::export!(
    "verald_regblock_assertions",
    assertions = [regblock_assertions::BusAssertions]
);
