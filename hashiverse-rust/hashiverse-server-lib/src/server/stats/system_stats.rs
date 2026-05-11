use sysinfo::{Disks, System};

/// Build the `system` subtree: process- and host-level metrics that don't
/// belong to any single subsystem. Numbers only; never asserts a specific
/// value, since these vary per host.
///
/// Disk fields sum across all disks the OS exposes. Load average is reported
/// as three floats; on platforms that don't compute load averages (notably
/// Windows) the fields are present but zero.
pub fn system_stats_subtree() -> serde_json::Value {
    let mut system = System::new();
    system.refresh_memory();
    let disks = Disks::new_with_refreshed_list();
    let load = System::load_average();

    let memory_total_bytes = system.total_memory();
    let memory_free_bytes = system.available_memory();

    let mut disk_total_bytes: u64 = 0;
    let mut disk_free_bytes: u64 = 0;
    for disk in disks.list() {
        disk_total_bytes = disk_total_bytes.saturating_add(disk.total_space());
        disk_free_bytes = disk_free_bytes.saturating_add(disk.available_space());
    }

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
