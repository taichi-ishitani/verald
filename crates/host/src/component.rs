//! `component!`: a Veryl component over a list of subscribers.

/// Defines a Veryl component `$ty` over subscribers, without exporting it:
/// a struct holding one field per subscriber, each hook dispatched to them
/// in the order given, and the registration methods (`set_clock`,
/// `set_reset`, `set_modport`, `set_input`, `set_output`). The methods
/// after the struct are added to the component's: `start` among them,
/// which calls `self.__verald_on_start(test)` through
/// [`entry::start`](crate::entry::start), since only the caller knows
/// whether the component takes a test's name.
///
/// ```ignore
/// verald_host::component! {
///     pub struct Checks { assertions: MyAssertions }
///
///     fn start(&mut self, ctx: &mut ::veryl_component::SimCtx) -> ::veryl_component::Result<()> {
///         verald_host::entry::start(ctx, None, |test| self.__verald_on_start(test))
///     }
/// }
/// ```
///
/// The crate must depend on `veryl-component` directly: its derives name
/// `::veryl_component`.
#[macro_export]
macro_rules! component {
    (
        $(#[$meta:meta])*
        $vis:vis struct $ty:ident { $($field:ident : $sub:ty),+ $(,)? }
        $($methods:tt)*
    ) => {
        $(#[$meta])*
        #[derive(::veryl_component::Component)]
        #[component(kind = clocked)]
        $vis struct $ty {
            $($field: $sub,)+
        }

        impl $ty {
            /// Calls the subscribers' `on_start`, in order.
            #[doc(hidden)]
            fn __verald_on_start(
                &mut self,
                test: ::core::option::Option<&str>,
            ) -> ::core::result::Result<(), $crate::HostError> {
                $($crate::HookSubscriber::on_start(&mut self.$field, test)?;)+
                ::core::result::Result::Ok(())
            }
        }

        #[::veryl_component::component_impl]
        impl $ty {
            fn on_build(
                &mut self,
                ctx: &mut ::veryl_component::BuildCtx,
            ) -> ::veryl_component::Result<()> {
                $crate::entry::on_build(ctx, |ctx| {
                    $($crate::HookSubscriber::on_build(&mut self.$field, ctx)?;)+
                    ::core::result::Result::Ok(())
                })
            }

            fn on_init(
                &mut self,
                ctx: &mut ::veryl_component::SimCtx,
            ) -> ::veryl_component::Result<()> {
                $crate::entry::on_init(ctx, || {
                    $($crate::HookSubscriber::on_init(&mut self.$field)?;)+
                    ::core::result::Result::Ok(())
                })
            }

            fn on_clock(
                &mut self,
                ctx: &mut ::veryl_component::SimCtx,
            ) -> ::veryl_component::Result<()> {
                $crate::entry::on_clock(ctx, || {
                    $($crate::HookSubscriber::on_clock(&mut self.$field)?;)+
                    ::core::result::Result::Ok(())
                })
            }

            fn on_reset(
                &mut self,
                ctx: &mut ::veryl_component::SimCtx,
            ) -> ::veryl_component::Result<()> {
                $crate::entry::on_reset(ctx, || {
                    $($crate::HookSubscriber::on_reset(&mut self.$field)?;)+
                    ::core::result::Result::Ok(())
                })
            }

            fn on_finish(
                &mut self,
                ctx: &mut ::veryl_component::SimCtx,
            ) -> ::veryl_component::Result<()> {
                $crate::entry::on_finish(ctx, || {
                    // Every subscriber is told, so that each lets go of what
                    // it holds; the first error is returned.
                    let mut result = ::core::result::Result::Ok(());
                    $(
                        let r = $crate::HookSubscriber::on_finish(&mut self.$field);
                        if result.is_ok() {
                            result = r;
                        }
                    )+
                    result
                })
            }

            /// Registers a clock under `name`.
            fn set_clock(
                &mut self,
                _ctx: &mut ::veryl_component::SimCtx,
                name: ::std::string::String,
                clk: ::veryl_component::ClockPort,
            ) -> ::veryl_component::Result<()> {
                $crate::entry::register($crate::registrations::set_clock(name, clk))
            }

            /// Registers a reset under `name`.
            fn set_reset(
                &mut self,
                _ctx: &mut ::veryl_component::SimCtx,
                name: ::std::string::String,
                rst: ::veryl_component::ResetPort,
            ) -> ::veryl_component::Result<()> {
                $crate::entry::register($crate::registrations::set_reset(name, rst))
            }

            /// Registers a modport of an interface under `name`.
            fn set_modport(
                &mut self,
                _ctx: &mut ::veryl_component::SimCtx,
                name: ::std::string::String,
                bus: ::veryl_component::ModportRef,
            ) -> ::veryl_component::Result<()> {
                $crate::entry::register($crate::registrations::set_modport(name, bus))
            }

            /// Registers a signal read under `name`.
            fn set_input(
                &mut self,
                _ctx: &mut ::veryl_component::SimCtx,
                name: ::std::string::String,
                sig: ::veryl_component::InputPort,
            ) -> ::veryl_component::Result<()> {
                $crate::entry::register($crate::registrations::set_input(name, sig))
            }

            /// Registers a signal driven under `name`.
            fn set_output(
                &mut self,
                _ctx: &mut ::veryl_component::SimCtx,
                name: ::std::string::String,
                sig: ::veryl_component::OutputPort,
            ) -> ::veryl_component::Result<()> {
                $crate::entry::register($crate::registrations::set_output(name, sig))
            }

            $($methods)*
        }
    };
}
