use sysinfo::{Disks, LoadAvg, System};

/// Build the `system` subtree: process- and host-level metrics that don't
/// belong to any single subsystem. Numbers only; never asserts a specific
/// value, since these vary per host.
///
/// Disk fields sum across all disks the OS exposes. Load average is reported
/// as three floats; on platforms that don't compute load averages (notably
/// Windows, where sysinfo returns `Err(Unsupported)`) the fields are present but zero.
/// Likewise, any metric sysinfo fails to read is reported as zero rather than
/// failing the whole stats response.
pub fn system_stats_subtree() -> serde_json::Value {
    let (memory_total_bytes, memory_free_bytes) = match System::new() {
        Ok(mut system) => {
            system.refresh_memory();
            (system.total_memory(), system.available_memory())
        }
        Err(_) => (0, 0),
    };

    let mut disk_total_bytes: u64 = 0;
    let mut disk_free_bytes: u64 = 0;
    if let Ok(disks) = Disks::new_with_refreshed_list() {
        for disk in &disks {
            disk_total_bytes = disk_total_bytes.saturating_add(disk.total_space());
            disk_free_bytes = disk_free_bytes.saturating_add(disk.available_space());
        }
    }

    let load = System::load_average().unwrap_or(LoadAvg { one: 0.0, five: 0.0, fifteen: 0.0 });

    serde_json::json!({
        "memory_total_bytes": memory_total_bytes,
        "memory_free_bytes": memory_free_bytes,
        "disk_total_bytes": disk_total_bytes,
        "disk_free_bytes": disk_free_bytes,
        "load_1m": load.one,
        "load_5m": load.five,
        "load_15m": load.fifteen,
    })
}

#[cfg(test)]
mod tests {
    use super::system_stats_subtree;

    #[test]
    fn system_stats_subtree_has_all_numeric_fields() {
        let system_stats = system_stats_subtree();
        for field in ["memory_total_bytes", "memory_free_bytes", "disk_total_bytes", "disk_free_bytes", "load_1m", "load_5m", "load_15m"] {
            assert!(system_stats[field].is_number(), "field {} missing or not numeric: {}", field, system_stats);
        }
        // Totals may legitimately be 0 (the documented fallback when sysinfo can't read a metric, e.g. containers
        // without listed disks), so only check invariants that hold for both real values and the 0/0 fallback.
        assert!(system_stats["memory_free_bytes"].as_u64().unwrap() <= system_stats["memory_total_bytes"].as_u64().unwrap(), "free memory exceeds total: {}", system_stats);
        assert!(system_stats["disk_free_bytes"].as_u64().unwrap() <= system_stats["disk_total_bytes"].as_u64().unwrap(), "free disk exceeds total: {}", system_stats);
    }
}
