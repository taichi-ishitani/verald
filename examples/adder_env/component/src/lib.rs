//! The adder's testbench as a component hierarchy: the test builds an env,
//! the env a driver that sends additions and checks the sums; the test's
//! `run` ends the run phase once the driver is done, and `check` compares
//! the adder's own count.

use veryl_tbf::core::Kind;
use veryl_tbf::prelude::*;

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

/// The adder's bus, and its count of additions outside the bus.
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

/// Sends `count` additions once the reset is released and checks each sum;
/// signals `done` when it has sent them all.
struct Driver {
    cb: AdderCb,
    rst: Reset,
    count: u64,
    sent: u64,
    done: Event,
}

impl Component for Driver {
    type Context = u64;
    const KIND: Kind = Kind::Driver;

    fn build(scope: &Scope, count: u64) -> Result<Self, TestError> {
        Ok(Driver {
            cb: scope.clocking::<AdderCb>("clk")?,
            rst: scope.reset("rst")?,
            count,
            sent: 0,
            done: Event::new(),
        })
    }

    async fn run(&mut self, cx: &mut RunContext) -> Result<(), TestError> {
        let cb = &mut self.cb;
        cb.init();
        self.rst.asserted().await;
        self.rst.released().await;
        for i in 0..self.count {
            let (x, y) = operands(i);
            cb.bus.start = true;
            cb.bus.a = x;
            cb.bus.b = y;
            tick!(cb);
            cb.bus.start = false;
            posedge_within!(cb.bus.done, 8)?;
            let expected = u16::from(x) + u16::from(y);
            if cb.bus.result == expected {
                cx.debug(format!("{x} + {y} = {expected}"));
            } else {
                cx.error(format!(
                    "{x} + {y}: expected {expected}, got {}",
                    cb.bus.result
                ));
            }
            self.sent += 1;
        }
        tick!(cb);
        self.done.trigger();
        Ok(())
    }

    fn check(&mut self, cx: &mut CheckContext) {
        if u64::from(self.cb.count) != self.sent {
            cx.error(format!(
                "the adder counted {} additions, the driver sent {}",
                self.cb.count, self.sent
            ));
        }
    }

    fn report(&self, cx: &ReportContext) {
        cx.info(format!("{} additions sent", self.sent));
    }
}

struct Env {
    driver: Instance<Driver>,
}

impl Component for Env {
    type Context = u64;
    const KIND: Kind = Kind::Env;

    fn build(scope: &Scope, count: u64) -> Result<Self, TestError> {
        Ok(Env {
            driver: scope.create_instance::<Driver>("driver", count)?,
        })
    }
}

/// Runs until the driver is done.
struct AdderTest {
    env: Instance<Env>,
    done: Event,
}

impl Component for AdderTest {
    type Context = u64;
    const KIND: Kind = Kind::Test;

    fn build(scope: &Scope, count: u64) -> Result<Self, TestError> {
        let env = scope.create_instance::<Env>("env", count)?;
        // Taken from the child right after making it.
        let done = env.driver.done.clone();
        Ok(AdderTest { env, done })
    }

    async fn run(&mut self, cx: &mut RunContext) -> Result<(), TestError> {
        self.done.wait_on().await;
        cx.info(format!("{} is done", self.env.borrow().driver.path()));
        Ok(())
    }
}

impl Test for AdderTest {
    fn setup(cx: &mut SetupContext) -> Result<u64, TestError> {
        cx.get("count")
    }
}

register_test!(AdderTest);

veryl_tbf::export!("tbf_adder_env");
