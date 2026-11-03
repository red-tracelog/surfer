`timescale 1ns/1ps

module simple_logic_dut(
    input  wire a,
    input  wire b,
    input  wire c,
    output wire y,
    output wire z
);
    assign y = (a & b) | (~c);
    assign z = (a ^ b) & c;
endmodule

module tb_simple_logic;
    reg a;
    reg b;
    reg c;
    wire y;
    wire z;

    integer i;

    simple_logic_dut dut (
        .a(a),
        .b(b),
        .c(c),
        .y(y),
        .z(z)
    );

    initial begin
        $display("time\ta b c\ty z");
        $monitor("%0t\t%b %b %b\t%b %b", $time, a, b, c, y, z);

        // Apply 30 different input stimuli.
        for (i = 0; i < 30; i = i + 1) begin
            a = i[0];
            b = i[1] ^ i[3];
            c = i[2] ^ i[4];
            #10;
        end

        #10;
        $finish;
    end
endmodule
