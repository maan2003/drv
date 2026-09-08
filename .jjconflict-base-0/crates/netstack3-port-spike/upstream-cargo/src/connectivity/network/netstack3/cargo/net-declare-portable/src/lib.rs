//! FIDL-free facade for the `net_types` literal macros used by Netstack3 core.
pub use net_declare_macros::{
    net_addr_subnet, net_addr_subnet_v4, net_addr_subnet_v6, net_ip, net_ip_v4, net_ip_v6,
    net_mac, net_prefix_length_v4, net_prefix_length_v6, net_subnet_v4, net_subnet_v6,
};
pub mod net {
    pub use super::{
        net_ip as ip, net_ip_v4 as ip_v4, net_ip_v6 as ip_v6, net_mac as mac,
        net_prefix_length_v4 as prefix_length_v4, net_prefix_length_v6 as prefix_length_v6,
        net_subnet_v4 as subnet_v4, net_subnet_v6 as subnet_v6,
    };
}
