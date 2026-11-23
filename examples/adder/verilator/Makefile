# Runs the adder's test on Verilator through verald's DPI host: builds the
# testbench with the `dpi` feature, the design's SystemVerilog with Veryl,
# and the simulation with Verilator.
#
#   make              build and run
#   make run SEED=5   with a seed (`+verald_seed`)
#   make clean        remove what the build made

COMPONENT := ../component
LIB_DIR   := $(abspath $(COMPONENT)/target/debug)
LIB       := $(LIB_DIR)/libadder_tb.so
DESIGN    := ../src/adder.sv
DPI_PKG   := ../../../crates/tb/dpi/verald_dpi_pkg.sv
SIM       := obj_dir/vtop
SEED      ?= 1

.PHONY: all run lib design clean

all: run

run: $(SIM)
	./$(SIM) +verald_seed=$(SEED)

# Cargo and Veryl decide themselves whether anything changed.
lib:
	cd $(COMPONENT) && cargo build --features dpi

design:
	cd .. && veryl +local build

$(SIM): lib design $(DPI_PKG) top.sv
	verilator --binary --timing -Wno-fatal -Wno-lint -Wno-style \
	    --top-module top -o vtop \
	    $(DPI_PKG) $(DESIGN) top.sv $(LIB) \
	    -LDFLAGS "-Wl,-rpath,$(LIB_DIR)"

clean:
	rm -rf obj_dir
	rm -f ../src/*.sv ../src/*.sv.map ../adder.f
	cd $(COMPONENT) && cargo clean
