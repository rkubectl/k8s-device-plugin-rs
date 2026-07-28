//! Dynamic Resource Allocation (DRA) driver runtime.
//!
//! Phase 1 scope: a pluginwatcher-based `Registration` server, the
//! `DRAPlugin` gRPC service (`NodePrepareResources`/
//! `NodeUnprepareResources`), a `ResourceClaim` resolver, and a
//! single-slice `ResourceSlice` publisher — wired together by a
//! `DraPlugin::run` lifecycle harness. See `docs/dra-design.md` for the
//! full design and phasing.

pub use claim::ClaimResolver;
pub use registration::DraRegistrationServer;
pub use resourceslice::ResourceSlicePublisher;
pub use service::DraPluginService;

mod claim;
mod registration;
mod resourceslice;
mod service;
