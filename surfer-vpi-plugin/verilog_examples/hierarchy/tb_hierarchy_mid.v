module tb_hierarchy_mid (
  input enable,
  input [7:0] stimulus,
  output [7:0] leaf_value,
  output done
);
  wire [7:0] stage_value;

  tb_hierarchy_leaf u_leaf (
    .enable(enable),
    .stimulus(stimulus),
    .value(stage_value),
    .done(done)
  );

  assign leaf_value = stage_value;
endmodule
