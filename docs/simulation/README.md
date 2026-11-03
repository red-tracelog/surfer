# Simulation

It is possible to connect to simulators and get the waveform information directly from the simulator, without the need to generate a waveform file. There is also some basic support to compile code and start the simulator from within Surfer. It may be worth noting that one can start a simulator at a different computer, setup an SSH-tunnel between the computers and connect with Surfer. Also, note that it is possible to start the simulation on WSL and connect with the Windows version of Surfer without any additional steps.

There are simulator plugins relying on either VPI or VHPI, where the VPI plugin is typically used for Verilog simulators and the VHPI plugin for VHDL simulators. However, different simulators have different well adapted support for VPI/VHPI, so it is not always the case. For simulators with built-in support, you do not need to worry about this. However, the built-in support is typically most suitable for small designs (as in number of files involved), so for larger designs it may be more feasible to start the simulator with a plugin directly and then connect to the plugin. The plugins are included in the downloadable zip-files on the Surfer webpage (build artifacts from GitLab) or can be built locally.

These features are experimental and we expect that they will improve over time. There is an [issue](https://gitlab.com/surfer-project/surfer/-/work_items/616) that can be used to point out what is not working. Or open a separate issue.

For the VS Code extension it is possible to start the simulator with the correct plugin, see below, and then connect to the simulator.

Note that this is not available in the browser version as the the browser plugin is not allowed to connect to something outside of the browser. If there are usecases where the simulator is also run inside the browser, it should be possible to enable this. In that case, open an issue.

## VPI

VPI (Verilog Procedural Interface) is a standardized C-language application programming interface defined by the IEEE 1800 standard that allows external software to interact with a Verilog/SystemVerilog simulation at runtime.

There is a plugin, `surfer_vpi_plugin`, that can be started with a simulator that supports VPI. This has been tested and shown to work (to some extent) with [Icarus Verilog](https://steveicarus.github.io/iverilog/) and [Xezim](https://github.com/aionhw/xezim).

If required, build the plugin:

```bash
cargo build --release --package surfer-vpi-plugin
```

Copy the file from `./target/release`, where the filename depends on the platform:

* Linux: `libsurfer_vpi_plugin.so`
* Macos: `libsurfer_vpi_plugin.dylib`
* Windows: `surfer_vpi_plugin.dll`

For Icarus Verilog, you may want to change the extension to `.vpi`, especially on Windows.

### Icarus Verilog

If Surfer can find Icarus Verilog on your computer, there will be an Icarus Verilog submenu under Simulation.

#### Start Icarus Verilog from within Surfer

You can select Simulation -> Icarus Verilog -> Compile... to compile a number of files into a single vvp. This vvp is called `surfer_default.vvp`.

If you have compiled your files into vvp-files in the current directory or used the Compile... menu option, you can select Simulation -> Icarus Verilog -> Run simulation... Now there will be a possibility to select a design to simulate.

#### Start Icarus Verilog separately and connect

Compile the sources as usual:

```bash
iverilog -o design.vvp file1.v file2.v ...
```

When starting the simulation, supply the plugin with the `-m` switch:

```bash
vvp -m./surfer_vpi_plugin.vpi -i design.vvp
```

You should now see something like (the `-i` flag is to drop the buffering of the output):

```text
No valid port startup argument found, using default 6967
Waiting for Surfer to connect...
```

Now, you can select Simulation -> Connect to running simulator...

See [Plugin arguments](#plugin-arguments) for the available startup arguments.

### Xezim

It is possible to start Xezim directly with the plugin as:

```bash
xezim file1.sv file2.sv ... --vpi-lib ./surfer_vpi_plugin.vpi
```

See [Plugin arguments](#plugin-arguments) for the available VPI startup arguments.

There are some known limitations in the current version of Xezim so not all values are captured.

### GHDL - VPI

[GHDL](https://ghdl.github.io/ghdl/) supports VPI. However, it is currently not possible to dynamically look up the required symbols from the plugin on Windows (and most likely macos). On Linux, it is possible to compile the plugin under the consideration that `vpi_get_cb_info` currently is not supported by GHDL:

```bash
cargo build --release --package surfer-vpi-plugin --no-default-features --features=ghdl
```

This will use a slightly different version of the code when removing the callbacks that doesn't rely on `vpi_get_cb_info`.

To run it, use:

```bash
ghdl -r my_entity --vpi=./surfer_vpi_plugin.vpi
```

Although it is possible to get the hierarchy, obtaining the simulated values does not currently fully work in the most recent version of GHDL.

### Verilator

[Verilator](https://verilator.org) still must be tested for compatibility. It seems like the simulation always starts running, so it is not possible to control it from the plugin.

## VHPI

VHPI (VHDL Procedural Interface) is the analogous to VPI standardized C-language API defined by IEEE 1076 for VHDL simulations. In general, the VHPI support is better on the Surfer side, providing more type information etc. In fact, in some cases the VHPI plugin provides more information that the waveform files. However, fewer simulators support VHPI.

There is a plugin, `surfer_vhpi_plugin`, that can be started with a simulator that supports VHPI. This has been tested and shown to work (to some extent) with NVC. It is included in the downloadable zip-files or can be built locally.

If required, build the plugin:

``` bash
cargo build --release --package surfer-vhpi-plugin
```

Copy the file from `./target/release`, where the filename depends on the platform:

* Linux: `libsurfer_vhpi_plugin.so`
* Macos: `libsurfer_vhpi_plugin.dylib`
* Windows: `surfer_vhpi_plugin.dll`

### NVC

If Surfer can find NVC on your computer, there will be an [NVC](https://www.nickg.me.uk/nvc/) submenu under Simulation.

#### Start NVC from within Surfer

You can select Simulation -> NVC -> Compile... to compile an arbitrary number of files. These files will be compiled into a `work` directory.

All compiled files in a `work` directory in the current directory will be listed when selecting Simulation -> NVC -> Run simulation... After selecting the correct top-level file, including architecture, it will automatically be elaborated if needed and then the simulation will start.

#### Start NVC separately and connect

This is the preferred method when running large designs, possibly over multiple libraries etc.

Analyze and elaborate as usual, for example:

```bash
nvc -a file1.vhdl file2.vhdl ...
nvc -e top_level
```

Now, when running the simulation provide the VHPI plugin:

```bash
nvc -r top_level --load surfer_vhpi_plugin.dll
```

Which will print something like:

```text
** Note: loading plugin libsurfer_vhpi_plugin.so
** Note: (init): No valid port startup argument found, using default 6967
** Note: (init): Waiting for Surfer to connect...
```

Now, you can select Simulation -> Connect to running simulator...

See [Plugin arguments](#plugin-arguments) for the available VHPI startup arguments.

### GHDL - VHPI

[GHDL](https://ghdl.github.io/ghdl/) also has some basic VHPI support. However, it is not enough for the VHPI plugin. Currently, one get (with VHPI traces on):

```text
loading VHPI library './target/release/libsurfer_vhpi_plugin.so'
vhpi_register_cb ({reason=1034}, 1) return 0000000000000000 [not implemented]
vhpi_check_error (00007FFFD38248F0) return 1
vhpi_register_cb ({reason=1035}, 1) return 0000000000000000 [not implemented]
vhpi_check_error (00007FFFD38248F0) return 1
VHPI module loaded and vhpi_startup_routines functions called!
```

## Plugin arguments

The VPI and VHPI plugins accept the following startup arguments:

* `+address=TARGET` sets the listening address. A port number uses `localhost`; a host without a port uses `6967`. The default is `localhost:6967`.
* `+port=TARGET` is an alias for `+address=TARGET`.
* `+debug` enables detailed diagnostic messages, including hierarchy scanning, signal type detection, array expansion, connection setup, and protocol messages.
* `+no-info` suppresses the startup message containing the command to connect Surfer to the simulator.

Protocol messages are automatically split into bounded chunks. Large hierarchies and timesteps do not require buffer-size configuration. See the [simulator protocol documentation](../../surfer-simulator-protocol/README.md) for the wire format and shared Rust plugin helpers.

Use one address argument. If multiple `+address` or `+port` arguments are supplied, the last one wins. IPv6 hosts can be bare (`::1`) or bracketed (`[::1]`); an explicit IPv6 port requires brackets (`[::1]:7777`). `localhost` resolves to IPv4 or IPv6 loopback addresses.

Surfer accepts the same targets in the connection dialog, `simulation_connect TARGET`, and `surfer sim --address TARGET` (`--port` is an alias). For example, `surfer sim --address 7777` connects to `localhost:7777`, and `surfer sim --address simulator.example` connects to `simulator.example:6967`.

For example, to listen on port `7777` when simulating with Icarus Verilog:

```bash
vvp -m./surfer_vpi_plugin.vpi -i design.vvp +port=7777
```

To listen on a specific interface and port, use:

```bash
vvp -m./surfer_vpi_plugin.vpi -i design.vvp +address=192.168.1.20:7777
```

To enable debug messages while running with Icarus Verilog:

```bash
vvp -m./surfer_vpi_plugin.vpi -i design.vvp +debug
```

To suppress the startup connection instructions:

```bash
vvp -m./surfer_vpi_plugin.vpi -i design.vvp +no-info
```

## Running a simulation

Once Surfer has connected to a simulator, the hierarchy is shown. Add variables as usual and then start the simulation.

Starting the simulation can be done using the toolbar, either type a time and press the "play" button or press the "fast forward" button to run the whole simulation. (Pressing "play" without a time will run until the end.)

There is also a restart button, although that currently removes the whole variable setup.

There are also commands for controlling the simulation. See [Interactive simulation](../commands/README.md#interactive-simulation).

## Other simulators

It is also possible to implement the required protocol for communicating between an arbitrary simulator and Surfer. The protocol is defined using protobuf, so it can be used for several languages. See the `surfer-simulator-protocol` crate in the repository.

## Plugin location

Surfer first checks if the environment variable `SURFER_PLUGIN_PATH` is set and if so looks there for plugins. Hence, this is the preferred way. If it is not set, Surfer will look at typical locations, before doing a more complete search.

### Development note

When running tests, the VPI plugin will be built as `surfer-vpi-plugin/verilog_examples/surfer_vpi_plugin.vpi`. There is a risk that this version will be used, even after changing the source file, so it makes sense to remove it before running the tests.
