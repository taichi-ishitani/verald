# Runs the testbench's tests one after another on Veryl's simulator, each
# named to the one test module by a runtime argument.
#
#   make                  every test (`make -k` goes on past a failure)
#   make ReverseTest      one test
#   make SEED=5           with a seed
#   make ARGS="--runtime-arg=ROUNDS:4 --runtime-arg=VERALD_LOG_LEVEL:debug"
#                         with more runtime arguments

TESTS := BasicTest ReverseTest RandomTest DirectTest
VERYL ?= veryl +local

.PHONY: all clean $(TESTS)

all: $(TESTS)

$(TESTS):
	$(VERYL) test $(if $(SEED),--seed $(SEED)) --runtime-arg=VERALD_TESTNAME:$@ $(ARGS)

clean:
	$(VERYL) clean
