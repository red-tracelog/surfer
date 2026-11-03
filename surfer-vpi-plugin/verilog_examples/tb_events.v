`timescale 1ns/1ps

module tb_events;
  // Event objects
  event ev_start;
  event ev_data;
  event ev_error;
  event ev_done;

  // Variables to accompany event traffic
  reg clk;
  reg ready;
  reg [7:0] data;
  reg [3:0] status;

  integer start_count;
  integer data_count;
  integer error_count;
  integer done_count;
  integer last_event_id;

  time last_event_time;
  real accumulated_value;

  // Free-running clock for additional wave activity
  initial begin
    clk = 1'b0;
    forever #2 clk = ~clk;
  end

  // Main stimulus thread: trigger a number of events while mutating variables.
  initial begin
    ready = 1'b0;
    data = 8'h00;
    status = 4'h0;

    start_count = 0;
    data_count = 0;
    error_count = 0;
    done_count = 0;
    last_event_id = -1;
    last_event_time = 0;
    accumulated_value = 0.0;

    #1;
    -> ev_start;

    repeat (6) begin
      #5;
      ready = ~ready;
      data = data + 8'h13;
      status = status + 1'b1;
      -> ev_data;

      if (data[2] && !ready) begin
        #1;
        status = 4'hE;
        -> ev_error;
      end
    end

    #8;
    status = 4'hF;
    -> ev_done;

    #6;
    $finish;
  end

  // Event handlers update counters and metadata variables.
  always @(ev_start) begin
    start_count = start_count + 1;
    last_event_id = 1;
    last_event_time = $time;
    accumulated_value = accumulated_value + 1.25;
  end

  always @(ev_data) begin
    data_count = data_count + 1;
    last_event_id = 2;
    last_event_time = $time;
    accumulated_value = accumulated_value + data_count;
  end

  always @(ev_error) begin
    error_count = error_count + 1;
    last_event_id = 3;
    last_event_time = $time;
    accumulated_value = accumulated_value - 0.5;
  end

  always @(ev_done) begin
    done_count = done_count + 1;
    last_event_id = 4;
    last_event_time = $time;
    accumulated_value = accumulated_value + 10.0;
  end
endmodule
