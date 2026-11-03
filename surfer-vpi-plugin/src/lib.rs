#![deny(unused_crate_dependencies)]

mod vpi_plugin;

vpi::startup_routines! {
    vpi_plugin::vpi_plugin_startup,
}
