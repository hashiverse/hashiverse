//! # Shared environment bundle for all protocol participants
//!
//! One struct — [`RuntimeServices`] — wraps every environment-dependent trait object
//! that a client or server needs to participate in the protocol: the clock
//! ([`TimeProvider`]), the network ([`TransportFactory`]), and the PoW engine
//! ([`PowGenerator`]).
//!
//! Every layer of the crate takes its dependencies through a shared
//! `Arc<RuntimeServices>` instead of reaching for concrete implementations directly.
//! This is the seam that makes the in-memory integration tests work: swap the
//! `TransportFactory` for [`MemTransportFactory`], the `TimeProvider` for a virtual
//! clock, the `PowGenerator` for [`SingleThreadedPowGenerator`], and an entire
//! hashiverse network can run deterministically inside a single test binary.

use crate::tools::pow_generator::pow_generator::PowGenerator;
use crate::tools::pow_generator::single_threaded_pow_generator::SingleThreadedPowGenerator;
use crate::tools::time_provider::time_provider::{RealTimeProvider, TimeProvider};
use crate::transport::transport::TransportFactory;
use std::sync::Arc;
use crate::transport::mem_transport::MemTransportFactory;

/// The bundle of environment-dependent services that both clients and servers need to
/// participate in the protocol.
///
/// Every layer of the crate — transport, protocol, client, server — takes its dependencies
/// through a shared `RuntimeServices` rather than reaching out to concrete clock /
/// network / PoW implementations directly. This is the seam that makes the in-memory
/// integration tests possible: swap [`TransportFactory`] for an in-memory one, [`TimeProvider`]
/// for a virtual clock, and [`PowGenerator`] for a single-threaded stub, and an entire
/// hashiverse network can run deterministically inside a single test binary.
///
/// In production, [`crate::client::hashiverse_client::HashiverseClient`] and the server
/// binary each construct a `RuntimeServices` with the real implementations of these traits
/// and pass the same `Arc<RuntimeServices>` down through every constructor that needs it.
#[derive(Clone)]
pub struct RuntimeServices {
    pub time_provider: Arc<dyn TimeProvider>,
    pub transport_factory: Arc<dyn TransportFactory>,
    pub pow_generator: Arc<dyn PowGenerator>,
}

impl RuntimeServices {
    pub fn default_for_testing() -> Arc<Self> {
        Arc::new(RuntimeServices {
            time_provider: Arc::new(RealTimeProvider),
            transport_factory: MemTransportFactory::default(),
            pow_generator: Arc::new(SingleThreadedPowGenerator::new()),
        })
    }
}
