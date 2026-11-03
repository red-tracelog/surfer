#![deny(unused_crate_dependencies)]

use vhpi::startup_routines;

mod vhpi_plugin;

startup_routines! {
    vhpi_plugin::vhpi_plugin_startup,
}
