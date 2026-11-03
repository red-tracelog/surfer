# NVC Plugin

This is basically just the dumper example for rust-vhpi.

## How to run

```bash
cargo build
nvc -a test_examples/tb_simple.vhdl
nvc -e tb_simple
nvc -r tb_simple --load=../target/debug/libvhpi_plugin.dylib # .dylib on mac, .so on linux, .dll on windows
```
