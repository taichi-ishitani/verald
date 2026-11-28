//! The register block's assertions alone, without a testbench: the test
//! module drives the bus itself. The assertions are the bus VIP's.

verald::export!(
    "verald_regblock_assertions",
    assertions = [regblock_vip::assertions::BusAssertions]
);
