`timescale 1ns/1ps

module tb_arrays;
    reg clk;
    reg rst_n;

    // Classic Verilog memory array.
    reg [7:0] mem_u8 [0:1];

    // Unpacked arrays of scalar/integer-like types.
    integer int_arr [0:1];

    // SystemVerilog packed arrays.
    reg [1:0][1:0] packed_2d;
    reg signed [1:0][1:0] packed_signed_2d;

    // Unpacked arrays with packed elements.
    reg [7:0] unpacked_1d [0:1];
    reg [3:0] unpacked_2d [0:1][0:1];

    // Mixed packed/unpacked multidimensional arrays.
    reg [1:0][1:0] mixed_2d [0:1];
    reg [1:0][1:0][1:0] mixed_3d [0:1][0:1];

    integer i;
    integer j;

    initial begin
        clk = 1'b0;
        forever #5 clk = ~clk;
    end

    initial begin
        rst_n = 1'b0;

        for (i = 0; i < 2; i = i + 1) begin
            mem_u8[i] = i[7:0];
        end

        for (i = 0; i < 2; i = i + 1) begin
            int_arr[i] = i * 10;
        end

        packed_2d = 4'b0110;
        packed_signed_2d = 4'sh7;

        for (i = 0; i < 2; i = i + 1) begin
            unpacked_1d[i] = 8'hA0 + i[7:0];
        end

        for (i = 0; i < 2; i = i + 1) begin
            for (j = 0; j < 2; j = j + 1) begin
                unpacked_2d[i][j] = (i + j) & 4'hF;
            end
        end

        mixed_2d[0] = {2'b01, 2'b10};
        mixed_2d[1] = {2'b11, 2'b00};

        for (i = 0; i < 2; i = i + 1) begin
            for (j = 0; j < 2; j = j + 1) begin
                mixed_3d[i][j] = 8'h80 + {6'b0, i[0], j[0]};
            end
        end

        #20 rst_n = 1'b1;

        // Keep callback traffic low for stable CI runtime while still creating changes.
        @(posedge clk);
        mem_u8[1] <= 8'h5A;
        int_arr[1] <= 123;
        packed_2d <= packed_2d ^ 4'b0011;
        packed_signed_2d <= packed_signed_2d + 4'sh1;
        unpacked_1d[1] <= 8'hC3;
        unpacked_2d[1][1] <= 4'hA;
        mixed_2d[0] <= {2'b10, 2'b01};
        mixed_3d[1][1] <= 8'h5A;

        #20;
        $finish;
    end
endmodule
