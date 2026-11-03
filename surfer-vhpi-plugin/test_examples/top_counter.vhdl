library ieee;
use ieee.std_logic_1164.all;

entity top_counter is
    generic (
        WORD_LEN : positive := 4
    );
    port (
        clk   : in  std_logic;
        rst_n : in  std_logic;
        en    : in  std_logic;
        count : out std_logic_vector(WORD_LEN - 1 downto 0)
    );
end entity top_counter;

architecture rtl of top_counter is
    component counter_component is
        generic (
            WORD_LEN : positive := 4
        );
        port (
            clk   : in  std_logic;
            rst_n : in  std_logic;
            en    : in  std_logic;
            q     : out std_logic_vector(WORD_LEN - 1 downto 0)
        );
    end component;
begin
    u_counter : counter_component
        generic map (
            WORD_LEN => WORD_LEN
        )
        port map (
            clk   => clk,
            rst_n => rst_n,
            en    => en,
            q     => count
        );
end architecture rtl;
