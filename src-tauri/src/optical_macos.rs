//! Parse diskutil's drive inventory without assuming an optical disc has no partitions.

fn field<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    text.lines().find_map(|line| {
        let (key, value) = line.trim().split_once(':')?;
        (key == name).then_some(value.trim())
    })
}

fn meaningful(value: &str) -> bool {
    !value.is_empty() && !value.starts_with("Not applicable") && value != "(null)"
}

pub fn candidate_nodes(list: &str) -> Vec<String> {
    list.lines()
        .filter(|line| {
            line.starts_with("/dev/disk")
                && !line.contains("(disk image)")
                && !line.contains("(synthesized)")
        })
        .filter_map(|line| line.split_whitespace().next())
        .map(|node| node.trim_start_matches("/dev/").to_string())
        .collect()
}

pub fn optical_name(info: &str) -> Option<String> {
    // Confirm hardware type, not the partition table or filesystem on the disc.
    field(info, "Optical Drive Type").filter(|s| meaningful(s))?;
    field(info, "Device / Media Name")
        .filter(|s| meaningful(s))
        .map(str::to_string)
}

fn whole_node(value: &str) -> Option<String> {
    let node = value.trim().trim_start_matches("/dev/");
    let digits = node
        .strip_prefix("disk")
        .or_else(|| node.strip_prefix("rdisk"))?;
    (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
        .then(|| format!("disk{digits}"))
}

pub fn resolve_nodes(
    profiler: &serde_json::Value,
    scanned: &[(String, String)],
) -> Vec<(String, String)> {
    let mut nodes = Vec::new();
    if let Some(drives) = profiler
        .get("SPDiscBurningDataType")
        .and_then(|v| v.as_array())
    {
        for drive in drives {
            let node = [
                "spdisc_burner-devicenode",
                "spdisc_burning_device",
                "bsd_name",
            ]
            .iter()
            .find_map(|key| whole_node(drive.get(key)?.as_str()?));
            if let Some(node) = node {
                let name = drive
                    .get("_name")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or("Optical Drive")
                    .to_string();
                if !nodes.iter().any(|(_, n)| n == &node) {
                    nodes.push((name, node));
                }
            }
        }
    }
    // Identify devices by BSD node, never by model: two attached drives may
    // have the same model, and profiler may omit either their node or the drive.
    for (name, node) in scanned {
        if !nodes.iter().any(|(_, n)| n == node) {
            nodes.push((name.clone(), node.clone()));
        }
    }
    nodes
}

pub fn disc_status(info: &str) -> (bool, Option<String>, Option<String>) {
    let volume = field(info, "Volume Name")
        .filter(|s| meaningful(s))
        .map(str::to_string);
    let mount = field(info, "Mount Point")
        .filter(|s| meaningful(s))
        .map(str::to_string);
    let media = field(info, "Optical Media Type")
        .is_some_and(|s| meaningful(s) && !matches!(s, "No Media" | "None"));
    (media || volume.is_some(), volume, mount)
}

pub fn partition_nodes(list: &str, whole: &str) -> Vec<String> {
    let whole = whole.trim_start_matches("/dev/");
    list.lines()
        .filter_map(|line| {
            let node = line.split_whitespace().last()?;
            let suffix = node.strip_prefix(whole)?.strip_prefix('s')?;
            // Nested Macintosh CD partitions can be named disk14s1s2.
            suffix
                .split('s')
                .all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
                .then(|| node.to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partitioned_cds_are_candidates_but_regular_disks_are_not_optical() {
        let list = "/dev/disk0 (internal, physical):\n 0: GUID_partition_scheme disk0\n\
            /dev/disk3 (synthesized):\n 0: APFS Container Scheme disk3\n\
            /dev/disk4 (disk image):\n 0: GUID_partition_scheme disk4\n\
            /dev/disk14 (external, physical):\n 0: CD_partition_scheme disk14\n\
             1: Apple_partition_scheme disk14s1\n 2: Apple_HFS Test Disc disk14s1s2";
        assert_eq!(candidate_nodes(list), ["disk0", "disk14"]);
        assert_eq!(
            optical_name("Device / Media Name: SSD\nContent (IOContent): GUID_partition_scheme"),
            None
        );
        assert_eq!(
            optical_name(
                "Device / Media Name: USB Optical Drive\nOptical Drive Type: CD-ROM, DVD-ROM"
            ),
            Some("USB Optical Drive".into())
        );
    }

    #[test]
    fn media_presence_is_independent_of_a_whole_disk_volume_name() {
        assert_eq!(disc_status("Volume Name: Not applicable (no file system)\nMount Point: Not applicable\nOptical Media Type: CD-ROM"), (true, None, None));
        assert_eq!(
            disc_status("Volume Name: Test Disc\nMount Point: /Volumes/Test Disc"),
            (
                true,
                Some("Test Disc".into()),
                Some("/Volumes/Test Disc".into())
            )
        );
        assert_eq!(
            disc_status("Volume Name: (null)\nOptical Media Type: No Media"),
            (false, None, None)
        );
        assert_eq!(
            disc_status("Optical Media Type: DVD-ROM"),
            (true, None, None)
        );
    }

    #[test]
    fn nested_partition_lookup_stays_within_the_selected_disc() {
        assert_eq!(partition_nodes("0: CD_partition_scheme disk14\n1: Apple_partition_scheme disk14s1\n2: Apple_partition_map disk14s1s1\n3: Apple_HFS Test Disc disk14s1s2\n0: disk140s1\n0: disk14something", "/dev/disk14"), ["disk14s1", "disk14s1s1", "disk14s1s2"]);
    }

    #[test]
    fn missing_identifiers_and_duplicate_models_do_not_hide_attached_drives() {
        let scanned = vec![
            ("Same model".into(), "disk14".into()),
            ("Same model".into(), "disk15".into()),
        ];
        for node in [
            serde_json::Value::Null,
            serde_json::json!(""),
            serde_json::json!("/dev/disk14s1"),
        ] {
            let profiler = serde_json::json!({"SPDiscBurningDataType": [{"_name": "Same model", "bsd_name": node}]});
            assert_eq!(resolve_nodes(&profiler, &scanned), scanned);
        }
        assert_eq!(resolve_nodes(&serde_json::Value::Null, &scanned), scanned);
        let profiler = serde_json::json!({"SPDiscBurningDataType": [
            {"_name":"Same model", "bsd_name":"/dev/rdisk14"},
            {"_name":"Same model", "bsd_name":"disk14"}
        ]});
        assert_eq!(resolve_nodes(&profiler, &scanned), scanned);
        assert!(resolve_nodes(
            &serde_json::json!({"SPDiscBurningDataType": [{"bsd_name":"/dev/disk0; command"}]}),
            &[]
        )
        .is_empty());
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn attached_optical_drive_inventory() {
        let node = std::env::var("DX_DRIVE").expect("set DX_DRIVE to the attached BSD disk name");
        let volume = std::env::var("DX_VOLUME").expect("set DX_VOLUME to the mounted disc label");
        let drives = crate::list_optical_drives().unwrap();
        let drive = drives
            .iter()
            .find(|d| d.raw_device_path == node)
            .expect("attached drive missing");
        assert!(drive.has_disc);
        assert_eq!(drive.volume_name.as_deref(), Some(volume.as_str()));
        assert_eq!(
            drive.mount_point.as_deref(),
            Some(format!("/Volumes/{volume}").as_str())
        );
        println!(
            "Detected {} on {}: {}",
            drive.name, drive.raw_device_path, drive.device_path
        );
    }
}
