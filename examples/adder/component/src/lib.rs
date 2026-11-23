//! A testbench on the runtime alone: drives additions through the adder's
//! bus with a clocking block and checks the sums.

use veryl_tbf_runtime::prelude::*;
use veryl_tbf_runtime::host_layer;

/// The `tb` modport of `adder_if`, as values.
#[modport]
struct AdderBus {
    #[output(init = false)]
    start: bool,
    #[output]
    a: u8,
    #[output]
    b: u8,
    #[input]
    done: bool,
    #[input]
    result: u16,
}

/// The testbench's view of the adder, synchronized with `clk`: its bus,
/// and the count of additions outside the bus.
#[clocking]
struct AdderCb {
    #[modport]
    bus: AdderBus,
    #[input]
    count: u8,
}

/// Operand pairs, spread over the 8-bit range without a random generator.
fn operands(i: u64) -> (u8, u8) {
    let x = (i * 37 + 11) % 256;
    let y = (i * 91 + 200) % 256;
    (x as u8, y as u8)
}

struct Adder;

impl Testbench for Adder {
    fn start(start: &mut Start) -> Result<(), TestError> {
        if start.test_name() != "adds" {
            return Err(format!("no test `{}`", start.test_name()).into());
        }
        let count = start
            .setting("count")
            .ok_or("setting `count` is missing")?
            .as_u64()
            .map_err(|e| TestError::new(e.to_string()))?;
        let rst = start.reset("rst")?;
        let mut cb = start.clocking::<AdderCb>("clk")?;
        start.main("test", async move {
            // Wait out the reset, with the bus idle meanwhile.
            cb.init();
            rst.asserted().await;
            rst.released().await;
            for i in 0..count {
                let (x, y) = operands(i);
                cb.bus.start = true;
                cb.bus.a = x;
                cb.bus.b = y;
                tick!(cb);
                cb.bus.start = false;
                posedge_within!(cb.bus.done, 8)?;
                let expected = u16::from(x) + u16::from(y);
                if cb.bus.result != expected {
                    return Err(
                        format!("{x} + {y}: expected {expected}, got {}", cb.bus.result).into(),
                    );
                }
            }
            tick!(cb);
            if u64::from(cb.count) != count {
                return Err(
                    format!("the adder counted {} additions, not {count}", cb.count).into(),
                );
            }
            host_layer::log(&format!("{count} additions checked"));
            Ok(())
        });
        Ok(())
    }
}

#[cfg(not(feature = "dpi"))]
veryl_tbf_runtime::export!("tbf_adder", Adder);

// The same testbench, for a SystemVerilog simulator (see `verilator/`).
#[cfg(feature = "dpi")]
mod dpi {
    veryl_tbf_runtime::component!(pub Tb, super::Adder);
    veryl_tbf_dpi::export!(Tb);
}
