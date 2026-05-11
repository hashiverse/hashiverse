use crate::environment::environment::Environment;

/// Build the `environment` subtree: counts and sizes of persisted state.
///
/// Counts come from the underlying store (cheap — fjall keyspace length, no
/// scan). `post_bundle_total_bytes` comes from the running counter the quota
/// system already maintains, so this is also O(1).
pub fn environment_stats_subtree(environment: &Environment) -> serde_json::Value {
    let post_bundle_count = environment.post_bundle_count().unwrap_or(0);
    let post_bundle_feedback_count = environment.post_bundle_feedback_count().unwrap_or(0);
    let post_bundle_total_bytes = environment.post_bundle_total_bytes();

    serde_json::json!({
        "post_bundle_count": post_bundle_count,
        "post_bundle_feedback_count": post_bundle_feedback_count,
        "post_bundle_total_bytes": post_bundle_total_bytes,
    })
}
