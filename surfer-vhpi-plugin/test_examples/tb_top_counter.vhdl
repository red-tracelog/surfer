library ieee;
use ieee.std_logic_1164.all;

entity tb_top_counter is
end entity tb_top_counter;

architecture sim of tb_top_counter is
    constant word_len   : positive := 8;
    constant clk_period : time := 10 ns;

    signal clk   : std_logic := '0';
    signal rst_n : std_logic := '0';
    signal en    : std_logic := '0';
    signal count : std_logic_vector(word_len - 1 downto 0);
begin
    dut : entity work.top_counter
        generic map (
            WORD_LEN => word_len
        )
        port map (
            clk   => clk,
            rst_n => rst_n,
            en    => en,
            count => count
        );

    clk_proc : process
    begin
        while now < 250 ns loop
            clk <= '0';
            wait for clk_period / 2;
            clk <= '1';
            wait for clk_period / 2;
        end loop;
        wait;
    end process;

    stim_proc : process
    begin
        rst_n <= '0';
        en <= '0';
        wait for 25 ns;

        rst_n <= '1';
        en <= '1';
        wait for 60 ns;

        en <= '0';
        wait for 20 ns;

        en <= '1';
        wait for 40 ns;

        assert false report "tb_top_counter finished" severity note;
        wait;
    end process;
end architecture sim;
