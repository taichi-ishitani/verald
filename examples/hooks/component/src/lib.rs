//! A testbench on the runtime alone, checking how Veryl's simulator calls
//! the hooks around a reset.

use veryl_tbf_runtime::prelude::*;
use veryl_tbf_runtime::sim;

struct Hooks;

impl Testbench for Hooks {
    fn start(start: &mut Start) -> Result<(), TestError> {
        let clk = start.clock("clk")?;
        let rst = start.reset("rst")?;
        start.main("test", async move {
            rst.asserted().await;
            sim::log(&format!("asserted at {}", sim::time()));
            rst.released().await;
            let edges = clk.edges();
            sim::log(&format!("released at {}, edges {edges}", sim::time()));
            // No `on_clock` came in reset; the one that released it is the
            // first edge counted.
            if edges != 1 {
                return Err(format!("{edges} clock edges counted by the release, not 1").into());
            }
            clk.wait(3).await;
            sim::log(&format!(
                "3 edges later at {}, edges {}",
                sim::time(),
                clk.edges()
            ));
            Ok(())
        });
        Ok(())
    }
}

veryl_tbf_runtime::export!("tbf_hooks", Hooks);
