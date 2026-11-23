//! A testbench on the runtime alone, checking how Veryl's simulator calls
//! the hooks around a reset.

use veryl_tbf_runtime::prelude::*;
use veryl_tbf_runtime::host_layer;

struct Hooks;

impl Testbench for Hooks {
    fn start(start: &mut Start) -> Result<(), TestError> {
        let clk = start.clock("clk")?;
        let rst = start.reset("rst")?;
        start.main("test", async move {
            rst.asserted().await;
            host_layer::log(&format!("asserted at {}", host_layer::time()));
            rst.released().await;
            let edges = clk.edges();
            host_layer::log(&format!("released at {}, edges {edges}", host_layer::time()));
            // No `on_clock` came in reset; the one that released it is the
            // first edge counted.
            if edges != 1 {
                return Err(format!("{edges} clock edges counted by the release, not 1").into());
            }
            clk.wait(3).await;
            host_layer::log(&format!(
                "3 edges later at {}, edges {}",
                host_layer::time(),
                clk.edges()
            ));
            Ok(())
        });
        Ok(())
    }
}

veryl_tbf_runtime::export!("tbf_hooks", Hooks);
