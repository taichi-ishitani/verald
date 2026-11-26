// The DPI functions of Verald's DPI host (`crates/tb/dpi`), as a
// wrapper calls them (all return at once), and tasks built on them.

// The widest `signal_map` entry a vector carries; define it to change it.
`ifndef VERALD_DPI_MAX_WIDTH
    `define VERALD_DPI_MAX_WIDTH 1024
`endif

package verald_dpi_pkg;
    // The unit of the delays here: `verald_wait_end`'s interval is in ns.
    timeunit 1ns;
    timeprecision 1ps;

    // The widest `signal_map` entry a vector carries (`VERALD_DPI_MAX_WIDTH`);
    // narrower ones are zero-extended. Given to `verald_init`.
    localparam int VERALD_MAX_W = `VERALD_DPI_MAX_WIDTH;

    // Loads the table, creates the testbench and calls `on_init`; 0 if done.
    // `label` names the instance in messages (`[tb] cycle 71: ..`), as Veryl's
    // simulator names a component's; `vector_width` is `VERALD_MAX_W`.
    import "DPI-C" function int verald_init(
        input string           label,
        input string           table_path,
        input longint unsigned seed,
        input int              vector_width
    );

    // Bind the names the testbench looks up to the table's keys; 0 if done.
    import "DPI-C" function int verald_set_clock(input string name, input int key);
    import "DPI-C" function int verald_set_reset(input string name, input int key);
    import "DPI-C" function int verald_set_modport(input string name, input int key);
    import "DPI-C" function int verald_set_input(input string name, input int key);
    import "DPI-C" function int verald_set_output(input string name, input int key);

    // The framework's options and the test's settings; 0 if done.
    import "DPI-C" function int verald_option(input string key, input longint value);
    import "DPI-C" function int verald_set(input string key, input longint value);

    // Starts the test; 0 if it started.
    import "DPI-C" function int verald_start(input string test, input longint unsigned now);

    // A `signal_map` entry's inputs, before the hook: in the table's order,
    // the first most significant.
    import "DPI-C" function void verald_sample(input int key, input bit [VERALD_MAX_W-1:0] inputs);

    // The hooks of a clock, and of its reset.
    import "DPI-C" function void verald_on_clock(input int clock, input longint unsigned now);
    import "DPI-C" function void verald_on_reset(input int clock, input longint unsigned now);

    // A `signal_map` entry's outputs after the hook, packed as its inputs;
    // 1 if any was written, when the wrapper updates them all.
    import "DPI-C" function bit verald_drive(
        input  int                 key,
        output bit [VERALD_MAX_W-1:0] outputs
    );

    // 0 running, 1 passed, 2 failed.
    import "DPI-C" function int verald_status();

    // From `final`: reports a finish the test did not ask for.
    import "DPI-C" function void verald_on_finish(input longint unsigned now);

    // Waits until the test ends, checking its status every `interval` (in
    // ns, this package's unit), and reports the result: `$fatal` when it
    // failed. The caller ends the simulation after it (`$finish`).
    task automatic verald_wait_end(input time interval);
        int status;
        status = 0;
        while (status == 0) begin
            #(interval);
            status = verald_status();
        end
        if (status == 2) $fatal(1, "verald: test failed");
        $display("verald: test passed");
    endtask
endpackage
