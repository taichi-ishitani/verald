// The adder's test on a SystemVerilog simulator: the counterpart of
// `adder_test` in `../src/adder.veryl`, with the testbench reached through
// Verald's DPI host. Written by hand; Veryl would generate it.
`timescale 1ns / 1ps

module top;
    import verald_dpi_pkg::*;

    // The table's keys (`verald_table.toml`).
    localparam int BUS   = 0;
    localparam int COUNT = 1;
    localparam int CLK   = 0;
    localparam int RST   = 0;

    logic clk   = 0;
    logic rst_n = 1;
    always #5 clk = ~clk;

    adder_adder_if bus ();
    logic [7:0] count;

    adder_adder dut (
        .i_clk  (clk    ),
        .i_rst  (rst_n  ),
        .bus    (bus.dut),
        .o_count(count  )
    );

    // The testbench's outputs, held between the edges it writes them at;
    // undriven (X) until the testbench first writes one of them, when the
    // bus's outputs are updated together.
    logic       start_q;
    logic [7:0] a_q;
    logic [7:0] b_q;
    assign bus.start = start_q;
    assign bus.a     = a_q;
    assign bus.b     = b_q;

    // At each edge: the inputs before the edge, the hook, then the outputs
    // written, at the edge (nonblocking). The reset wins over the clock, as
    // `on_reset` and `on_clock` are exclusive on Veryl's simulator.
    always @(posedge clk, negedge rst_n) begin
        bit [VERALD_MAX_W-1:0] out;
        verald_sample(BUS, {bus.done, bus.result});
        verald_sample(COUNT, count);
        if (!rst_n) verald_on_reset(CLK, $time);
        else        verald_on_clock(CLK, $time);
        if (verald_drive(BUS, out)) {start_q, a_q, b_q} <= out[16:0];
    end

    // Registration and start, then the reset (as `rst.assert(clk)`).
    initial begin
        longint unsigned seed;
        if (!$value$plusargs("verald_seed=%d", seed)) seed = 1;
        if (verald_init("tb", "verald_table.toml", seed, VERALD_MAX_W) != 0)
            $fatal(1, "verald: verald_init failed");
        if (verald_set_clock("clk", CLK) != 0) $fatal(1, "verald: set_clock failed");
        if (verald_set_reset("rst", RST) != 0) $fatal(1, "verald: set_reset failed");
        if (verald_set_modport("bus", BUS) != 0) $fatal(1, "verald: set_modport failed");
        if (verald_set_input("count", COUNT) != 0) $fatal(1, "verald: set_input failed");
        if (verald_option("timeout", 100000) != 0) $fatal(1, "verald: option failed");
        if (verald_set("count", 20) != 0) $fatal(1, "verald: set failed");
        if (verald_start("adds", $time) != 0) $fatal(1, "verald: could not start");
        #12 rst_n = 0;
        repeat (3) @(posedge clk);
        @(negedge clk) rst_n = 1;
    end

    // Waits for the test's end, checking every 1 us (100 cycles), and ends
    // the simulation.
    initial begin
        verald_wait_end(1000);
        $finish;
    end

    final verald_on_finish($time);
endmodule
