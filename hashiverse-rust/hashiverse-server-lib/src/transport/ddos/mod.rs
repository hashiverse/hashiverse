//! # Kernel-level DDoS protection for production servers
//!
//! The production `DdosProtection` implementation that escalates bans from
//! in-RAM scoring to Linux `ipset` + `iptables`, dropping offending traffic at
//! the kernel before it ever touches userspace.

pub mod ipset_ddos;
