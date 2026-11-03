#!/usr/bin/env sh

# Require an argument
if [ -z "$1" ]; then
  echo "Usage: $0 path/to/testbench.vhdl"
  exit 1
fi

FILE_PATH="$1"

# Extract filename without directory and extension
TB_NAME=$(basename "$FILE_PATH" .vhdl)

cargo build -p surfer-vhpi-plugin || exit 1

nvc -a "$FILE_PATH" || exit 1
nvc -e "$TB_NAME" || exit 1
nvc -r "$TB_NAME" --load=../target/debug/libsurfer_vhpi_plugin.dylib
