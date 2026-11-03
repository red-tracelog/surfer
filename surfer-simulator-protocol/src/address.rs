use std::net::Ipv6Addr;

use crate::DEFAULT_SIMULATOR_PORT;

/// Expand a port, host, or host:port into a TCP address shared by Surfer and its plugins.
/// `localhost` lets the resolver select IPv4 or IPv6 loopback addresses.
/// IPv6 addresses with an explicit port must use brackets.
pub fn simulator_address(address: &str) -> String {
    let address = address.trim();
    if address.is_empty() {
        return format!("localhost:{DEFAULT_SIMULATOR_PORT}");
    }
    if let Ok(port) = address.parse::<u16>() {
        return format!("localhost:{port}");
    }
    if address.parse::<Ipv6Addr>().is_ok() {
        return format!("[{address}]:{DEFAULT_SIMULATOR_PORT}");
    }
    if address.ends_with(']') || !address.contains(':') {
        return format!("{address}:{DEFAULT_SIMULATOR_PORT}");
    }
    address.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ports_hosts_and_ipv6_use_the_same_defaults() {
        for (input, expected) in [
            ("", "localhost:6967"),
            (" 1234 ", "localhost:1234"),
            ("localhost", "localhost:6967"),
            ("simulator.example", "simulator.example:6967"),
            ("simulator.example:1234", "simulator.example:1234"),
            ("127.0.0.1", "127.0.0.1:6967"),
            ("127.0.0.1:1234", "127.0.0.1:1234"),
            ("::1", "[::1]:6967"),
            ("[::1]", "[::1]:6967"),
            ("[::1]:1234", "[::1]:1234"),
            ("2001:db8::1234", "[2001:db8::1234]:6967"),
            ("[2001:db8::1234]:5678", "[2001:db8::1234]:5678"),
        ] {
            assert_eq!(simulator_address(input), expected, "input: {input}");
            assert_eq!(simulator_address(expected), expected, "already expanded");
        }
    }
}
