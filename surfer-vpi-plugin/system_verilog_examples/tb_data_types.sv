`timescale 1ns/1ps

module tb_data_types;
  // 4-state net and variable types
  wire w1;
  tri tri0_sig;
  wand wand_sig;

  reg r1;
  logic logic1;
  logic [7:0] logic_vec;
  bit [3:0] bit_vec;

  // Integer-like and numeric variable types
  integer i_var;
  int int_var;
  shortint shortint_var;
  longint longint_var;
  byte byte_var;
  time time_var;
  real real_var;
  shortreal shortreal_var;

  // String / enum / aggregate SV types
  string str_var;
  typedef enum logic [1:0] {
    IDLE  = 2'b00,
    RUN   = 2'b01,
    WAIT  = 2'b10,
    ERROR = 2'b11
  } state_t;
  state_t state;

  typedef struct packed {
    logic [3:0] tag;
    logic [7:0] payload;
  } packet_t;
  packet_t pkt;

  typedef union packed {
    logic [15:0] all;
    logic [15:0] bytes;
  } word_u;
  word_u wu;

  // Arrays / memories
  reg [7:0] mem [0:3];
  logic [3:0][7:0] packed_2d;
  logic [7:0] unpacked_arr [0:3];

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
    int_var = 0;
    shortint_var = 0;
    longint_var = 0;
    byte_var = 0;
    time_var = 0;
    real_var = 0.0;
    shortreal_var = 0.0;

    str_var = "boot";
    state = IDLE;
    pkt.tag = 4'h0;
    pkt.payload = 8'h00;
    wu.all = 16'h0000;
    packed_2d = '0;

    for (idx = 0; idx < 4; idx = idx + 1) begin
      mem[idx] = idx;
    end
    for (idx = 0; idx < 4; idx = idx + 1) begin
      unpacked_arr[idx] = 8'hA0 + idx;
    end

    // Drive dynamic changes over time to create rich wave activity
    #5;
    logic1 = 1'b1;
    r1 = 1'b1;
    logic_vec = 8'hA5;
    bit_vec = 4'b1010;
    i_var = -12;
    int_var = 32'h1234_ABCD;
    shortint_var = -1234;
    longint_var = 64'h0102_0304_0506_0708;
    byte_var = -8'sd7;
    time_var = $time;
    real_var = 3.1415926535;
    shortreal_var = 1.5e-2;
    str_var = "running";
    state = RUN;
    pkt.tag = 4'hA;
    pkt.payload = 8'h5C;
    wu.bytes[7:0] = 8'hDE;
    wu.bytes[15:8] = 8'hAD;
    packed_2d[0] = 8'h11;
    packed_2d[1] = 8'h22;
    packed_2d[2] = 8'h33;
    packed_2d[3] = 8'h44;
    unpacked_arr[0] = 8'hFE;
    unpacked_arr[1] = 8'hED;

    #5;
    logic_vec = 8'hXX;
    logic1 = 1'bx;
    r1 = 1'bz;
    state = WAIT;
    str_var = "x-z-phase";
    mem[3] = 8'hFF;
    mem[2] = 8'h00;
    real_var = -2.75;
    shortreal_var = 0.333;

    #5;
    logic1 = 1'b0;
    r1 = 1'b1;
    logic_vec = 8'h3C;
    bit_vec = bit_vec + 1;
    i_var = i_var + 99;
    int_var = int_var ^ 32'hFFFF_0000;
    shortint_var = shortint_var + 321;
    longint_var = longint_var - 64'd10;
    byte_var = byte_var + 8'sd9;
    time_var = $time;
    state = ERROR;
    str_var = {"t=", $sformatf("%0t", $time)};
    pkt.payload = pkt.payload + 8'h01;
    wu.all = 16'hBEEF;

    #5;
    state = IDLE;
    str_var = "done";

    #10;
    $finish;
  end
endmodule
