//! # Time-ordered walks over the bucket hierarchy
//!
//! A single-source timeline walks one feed (a user's posts, a hashtag, mentions,
//! replies) backwards through time; a multiple-source timeline merges several of
//! those into one chronologically sorted stream. A pure recursive visitor powers
//! the walk, and a short-lived scratch pad stashes just-authored local posts so
//! they appear in the user's own timeline before network propagation completes.

pub mod single_timeline;
pub mod multiple_timeline;
pub mod recent_posts_pen;

pub mod recursive_bucket_visitor;

