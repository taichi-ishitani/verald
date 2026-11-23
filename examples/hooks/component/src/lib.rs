//! A testbench on the runtime alone, checking how Veryl's simulator calls
//! the hooks around a reset.

use verald_tb_runtime::prelude::*;
use verald_tb_runtime::host;

struct Hooks;

impl Testbench for Hooks {
    fn start(start: &mut Start) -> Result<(), TestError> {
        let clk = start.clock("clk")?;
        let rst = start.reset("rst")?;
        start.main("test", async move {
            rst.asserted().await;
            host::log(&format!("asserted at {}", host::time()));
            rst.released().await;
            let edges = clk.edges();
            host::log(&format!("released at {}, edges {edges}", host::time()));
            // No `on_clock` came in reset; the one that released it is the
            // first edge counted.
            if edges != 1 {
                return Err(format!("{edges} clock edges counted by the release, not 1").into());
            }
            clk.wait(3).await;
            host::log(&format!(
                "3 edges later at {}, edges {}",
                host::time(),
                clk.edges()
            ));
            Ok(())
        });
        Ok(())
    }
}

verald_tb_runtime::export!("verald_hooks", Hooks);
