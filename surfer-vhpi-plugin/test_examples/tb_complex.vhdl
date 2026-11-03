library ieee;
use ieee.std_logic_1164.all;

entity tb_complex is
end entity;

architecture sim of tb_complex is
  -- Define custom types
  type int_2d_t is array (natural range <>, natural range <>) of integer;
  type slv_1d_t is array (natural range <>) of std_logic_vector(7 downto 0);
  type slv_2d_t is array (natural range <>, natural range <>) of std_logic_vector(3 downto 0);

  -- Array-of-array chains: each level wraps the previous one.
  -- slv8: base element is std_logic_vector(7 downto 0)
  type slv8_row_t  is array (natural range <>) of std_logic_vector(7 downto 0);
  type slv8_2d_t   is array (natural range <>) of slv8_row_t(0 to 1);

  -- slv4: base element is std_logic_vector(3 downto 0), built up to 4D
  type slv4_row_t  is array (natural range <>) of std_logic_vector(3 downto 0);
  type slv4_2d_t   is array (natural range <>) of slv4_row_t(0 to 1);
  type slv4_3d_t   is array (natural range <>) of slv4_2d_t(0 to 1);
  type slv4_4d_t   is array (natural range <>) of slv4_3d_t(0 to 1);

  -- int: base element is integer, built up to 4D
  type int_row_t   is array (natural range <>) of integer;
  type int_2d_arr_t is array (natural range <>) of int_row_t(0 to 1);
  type int_3d_t    is array (natural range <>) of int_2d_arr_t(0 to 1);
  type int_4d_t    is array (natural range <>) of int_3d_t(0 to 1);

  type data_record_t is record
    value : integer;
    status : std_logic;
    data : std_logic_vector(7 downto 0);
  end record;

  type record_array_t is array (natural range <>) of data_record_t;

  -- Signal declarations
  signal v_int_2d : int_2d_t(4 to 5, 4 downto 2) := ((0, 0, 0), (0, 0, 0));
  signal v_slv_1d : slv_1d_t(0 to 2) := ((others => '0'), (others => '0'), (others => '0'));
  signal v_slv_2d : slv_2d_t(0 to 1, 0 to 1) := (("0000", "0000"), ("0000", "0000"));
  signal v_record : data_record_t := (value => 0, status => '0', data => x"00");
  signal v_record_arr : record_array_t(1 downto 0) := ((0, '0', x"00"), (0, '0', x"00"));

  -- Array-of-array signals
  -- 2D: [2] x slv8_row_t([0..1])  =>  [2][2] x slv8
  signal v_slv8_2d : slv8_2d_t(0 to 1) :=
    ((x"00", x"00"), (x"00", x"00"));

  -- 3D: [2] x slv4_2d_t([0..1])  =>  [2][2][2] x slv4
  signal v_slv4_3d : slv4_3d_t(0 to 1) :=
    (((x"0", x"0"), (x"0", x"0")),
     ((x"0", x"0"), (x"0", x"0")));

  -- 4D: [2] x slv4_3d_t([0..1])  =>  [2][2][2][2] x slv4
  signal v_slv4_4d : slv4_4d_t(0 to 1) :=
    ((((x"0", x"0"), (x"0", x"0")),
      ((x"0", x"0"), (x"0", x"0"))),
     (((x"0", x"0"), (x"0", x"0")),
      ((x"0", x"0"), (x"0", x"0"))));

  -- 3D: [2] x int_2d_arr_t([0..1])  =>  [2][2][2] x integer
  signal v_int_3d : int_3d_t(0 to 1) :=
    (((0, 0), (0, 0)), ((0, 0), (0, 0)));

  -- 4D: [2] x int_3d_t([0..1])  =>  [2][2][2][2] x integer
  signal v_int_4d : int_4d_t(0 to 1) :=
    ((((0, 0), (0, 0)), ((0, 0), (0, 0))),
     (((0, 0), (0, 0)), ((0, 0), (0, 0))));

begin
  stim : process
  begin
    -- Change 1
    v_int_2d <= ((1, 2, 3), (4, 5, 6));
    v_slv_1d <= (x"AA", x"BB", x"CC");
    v_slv_2d <= (("1010", "0101"), ("1100", "0011"));
    v_record <= (value => 42, status => '1', data => x"A5");
    v_record_arr <= ((42, '1', x"A5"), (10, '0', x"5A"));
    v_slv8_2d <= ((x"AB", x"CD"), (x"EF", x"01"));
    v_slv4_3d <= (((x"1", x"2"), (x"3", x"4")),
                  ((x"5", x"6"), (x"7", x"8")));
    v_slv4_4d <= ((((x"1", x"2"), (x"3", x"4")),
                   ((x"5", x"6"), (x"7", x"8"))),
                  (((x"9", x"A"), (x"B", x"C")),
                   ((x"D", x"E"), (x"F", x"0"))));
    v_int_3d <= (((1, 2), (3, 4)), ((5, 6), (7, 8)));
    v_int_4d <= ((((1, 2), (3, 4)), ((5, 6), (7, 8))),
                 (((9, 10), (11, 12)), ((13, 14), (15, 16))));
    wait for 10 ns;

    -- Change 2
    v_int_2d <= ((10, 20, 30), (40, 50, 60));
    v_slv_1d <= (x"11", x"22", x"33");
    v_slv_2d <= (("1111", "0000"), ("1001", "0110"));
    v_record <= (value => 100, status => '0', data => x"F0");
    v_record_arr <= ((100, '0', x"F0"), (50, '1', x"0F"));
    v_slv8_2d <= ((x"12", x"34"), (x"56", x"78"));
    v_slv4_3d <= (((x"A", x"B"), (x"C", x"D")),
                  ((x"E", x"F"), (x"0", x"1")));
    v_slv4_4d <= ((((x"F", x"E"), (x"D", x"C")),
                   ((x"B", x"A"), (x"9", x"8"))),
                  (((x"7", x"6"), (x"5", x"4")),
                   ((x"3", x"2"), (x"1", x"0"))));
    v_int_3d <= (((-1, -2), (-3, -4)), ((-5, -6), (-7, -8)));
    v_int_4d <= ((((-1, -2), (-3, -4)), ((-5, -6), (-7, -8))),
                 (((-9, -10), (-11, -12)), ((-13, -14), (-15, -16))));
    wait for 10 ns;

    -- Change 3
    v_int_2d <= ((-1, -2, -3), (-4, -5, -6));
    v_slv_1d <= (x"FF", x"00", x"55");
    v_slv_2d <= (("0011", "1100"), ("0101", "1010"));
    v_record <= (value => -1, status => '1', data => x"99");
    v_record_arr <= ((-1, '1', x"99"), (127, '0', x"66"));
    v_slv8_2d <= ((x"FF", x"00"), (x"55", x"AA"));
    v_slv4_3d <= (((x"F", x"0"), (x"5", x"A")),
                  ((x"3", x"C"), (x"6", x"9")));
    v_slv4_4d <= ((((x"0", x"F"), (x"5", x"A")),
                   ((x"3", x"C"), (x"6", x"9"))),
                  (((x"1", x"E"), (x"4", x"B")),
                   ((x"2", x"D"), (x"7", x"8"))));
    v_int_3d <= (((100, 200), (300, 400)), ((500, 600), (700, 800)));
    v_int_4d <= ((((1000, 2000), (3000, 4000)), ((5000, 6000), (7000, 8000))),
                 (((9000, 10000), (11000, 12000)), ((13000, 14000), (15000, 16000))));
    wait for 10 ns;

    wait;
  end process;
end architecture;
