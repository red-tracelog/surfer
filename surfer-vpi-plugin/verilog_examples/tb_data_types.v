`timescale 1ns/1ps

module tb_data_types;
  // Net types
  wire w1;
  tri tri0_sig;
  wand wand_sig;

  // Variable types available in Verilog
  reg r1;
  reg logic1;
  reg [7:0] logic_vec;
  reg [3:0] bit_vec;

  integer i_var;
  time time_var;
  real real_var;
  realtime shortreal_var;

  // Arrays / memories
  reg [7:0] mem [0:7];
  reg [7:0] unpacked_arr [0:5];

  // Simple continuous assignments to exercise nets
  assign w1 = logic1;
  assign tri0_sig = logic1 ? 1'bz : 1'b0;
  assign wand_sig = logic1 & r1;

  integer idx;

  initial begin
    // Initialize values
    r1 = 1'b0;
    logic1 = 1'b0;
    logic_vec = 8'h00;
    bit_vec = 4'h0;

    i_var = 0;
    time_var = 0;
    real_var = 0.0;
    shortreal_var = 0.0;

    for (idx = 0; idx < 8; idx = idx + 1) begin
      mem[idx] = idx;
    end
    for (idx = 0; idx < 6; idx = idx + 1) begin
      unpacked_arr[idx] = 8'hA0 + idx;
    end

    // Drive dynamic changes over time to create rich wave activity
    #5;
    logic1 = 1'b1;
    r1 = 1'b1;
    logic_vec = 8'hA5;
    bit_vec = 4'b1010;
    i_var = -12;
    time_var = $time;
    real_var = 3.1415926535;
    shortreal_var = 1.5e-2;
    mem[0] = 8'hDE;
    mem[1] = 8'hAD;

    #5;
    logic_vec = 8'hXX;
    logic1 = 1'bx;
    r1 = 1'bz;
    mem[3] = 8'hFF;
    mem[4] = 8'h00;
    real_var = -2.75;
    shortreal_var = 0.333;

    #5;
    logic1 = 1'b0;
    r1 = 1'b1;
    logic_vec = 8'h3C;
    bit_vec = bit_vec + 1;
    i_var = i_var + 99;
    time_var = $time;
    mem[2] = mem[2] + 1;
    unpacked_arr[1] = unpacked_arr[1] ^ 8'h0F;

    #10;
    $finish;
  end
endmodule
