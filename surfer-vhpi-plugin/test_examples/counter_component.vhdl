library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;

entity counter_component is
    generic (
        WORD_LEN : positive := 4
    );
    port (
        clk   : in  std_logic;
        rst_n : in  std_logic;
        en    : in  std_logic;
        q     : out std_logic_vector(WORD_LEN - 1 downto 0)
    );
end entity counter_component;

architecture rtl of counter_component is
    signal count_reg : unsigned(WORD_LEN - 1 downto 0) := (others => '0');
begin
    process (clk)
    begin
        if rising_edge(clk) then
            if rst_n = '0' then
                count_reg <= (others => '0');
            elsif en = '1' then
                count_reg <= count_reg + 1;
            end if;
        end if;
    end process;

    q <= std_logic_vector(count_reg);
end architecture rtl;
