`timescale 1ns/1ps

module tb_hierarchy_leaf (
  input enable,
  input [7:0] stimulus,
  output [7:0] value,
  output done
);
  // Arrays / memories
  reg [7:0] mem [0:7];
  reg [7:0] unpacked_arr [0:3];

  integer idx;

  initial begin
    for (idx = 0; idx < 8; idx = idx + 1) begin
      mem[idx] = 8'h10 + idx;
    end

    for (idx = 0; idx < 4; idx = idx + 1) begin
      unpacked_arr[idx] = 8'h80 + idx;
    end
  end

  assign value = enable
    ? (mem[stimulus[2:0]] ^ unpacked_arr[stimulus[1:0]])
    : mem[0];

  assign done = enable && (stimulus == 8'h34);
endmodule
