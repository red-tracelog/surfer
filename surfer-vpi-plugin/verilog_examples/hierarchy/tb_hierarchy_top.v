`timescale 1ns/1ps

module tb_hierarchy_top;
  reg enable;
  reg [7:0] stimulus;

  wire [7:0] leaf_value;
  wire done;

  tb_hierarchy_mid u_mid (
    .enable(enable),
    .stimulus(stimulus),
    .leaf_value(leaf_value),
    .done(done)
  );

  initial begin
    enable = 1'b0;
    stimulus = 8'h00;

    #5;
    enable = 1'b1;
    stimulus = 8'h11;

    #5;
    stimulus = 8'h23;

    #5;
    stimulus = 8'h34;

    #5;
    enable = 1'b0;

    #10;
    $finish;
  end
endmodule
