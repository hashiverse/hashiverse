//! # Post-bundle fetching, caching, healing, and submission
//!
//! Two parallel stacks — one for posts themselves and one for their feedback —
//! each with a trait, a production "live" implementation, and a test stub.
//! Background reconciliation heals divergent copies across peers, and the
//! submission helpers orchestrate the two-phase claim+commit for posts and the
//! single-phase submit for feedback.

pub mod post_bundle_manager;
pub mod post_bundle_feedback_manager;
pub mod live_post_bundle_manager;
pub mod live_post_bundle_feedback_manager;
pub mod stub_post_bundle_manager;
pub mod stub_post_bundle_feedback_manager;
pub mod post_bundle_healing;
pub mod post_bundle_feedback_healing;

pub mod posting;



