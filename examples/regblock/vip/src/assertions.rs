//! The bus's assertions, over the monitor modport: the VIP provides them
//! beside its interface and its agent, for the testbench's component and
//! for a component holding the assertions alone.

use verald::prelude::*;

use crate::bus_if;

/// The bus protocol and the registers' contents, checked at each edge of
/// `clk`: a request is accepted at an edge where `valid` and `ready` are
/// both high, and a read's data comes back at the next edge with `rvalid`.
///
/// The protocol is written as temporal properties; the registers' contents
/// are kept in auxiliary state, updated after them at each edge.
#[assertions]
#[derive(Default)]
pub struct BusAssertions {
    #[clock]
    clk: Clock,
    #[reset]
    rst: Reset,
    #[modport]
    bus_mon: bus_if::Monitor,
    /// A read accepted at the previous edge, and its address.
    read: Option<u8>,
    /// The registers' contents, as the writes left them.
    regs: [u16; 8],
}

#[assertions]
impl BusAssertions {
    fn accepted(&self) -> bool {
        bus_mon.valid && bus_mon.ready
    }

    /// `valid && ready && !write |=> rvalid`
    #[assert]
    fn rvalid_follows_a_read(&self) {
        if self.accepted() && !bus_mon.write {
            seq!(delay(1) -> bus_mon.rvalid);
        }
    }

    /// `rvalid |-> $past(valid && ready && !write)`
    #[assert]
    fn rvalid_only_after_a_read(&self) {
        if bus_mon.rvalid {
            past(bus_mon.valid && bus_mon.ready && !bus_mon.write);
        }
    }

    /// `valid && !ready |=> valid && $stable(write) && $stable(addr)
    /// && (!write || $stable(wdata))`: a request is held until it is
    /// accepted.
    #[assert]
    fn a_stalled_request_is_held(&self) {
        if bus_mon.valid && !bus_mon.ready {
            seq!(
                delay(1)
                    -> bus_mon.valid
                        && stable(bus_mon.write)
                        && stable(bus_mon.addr)
                        && (!bus_mon.write || stable(bus_mon.wdata))
            );
        }
    }

    #[on_clock]
    fn update(&mut self) {
        if let Some(addr) = self.read {
            let expected = self.regs[usize::from(addr)];
            check!(
                bus_mon.rdata == expected,
                "read {:#06x} from register {addr}, expected {expected:#06x}",
                bus_mon.rdata
            );
        }
        self.read = None;
        if self.accepted() {
            if bus_mon.write {
                self.regs[usize::from(bus_mon.addr)] = bus_mon.wdata;
            } else {
                self.read = Some(bus_mon.addr);
            }
        }
    }
}
