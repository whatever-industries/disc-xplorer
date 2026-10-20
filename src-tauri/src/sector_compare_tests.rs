use super::*;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("dx-sector-compare-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn image(&self, name: &str, stride: u64, sectors: u64, prefix: u64) -> FlatInfo {
        let path = self.0.join(name);
        fs::write(&path, vec![0x33; (prefix + stride * sectors) as usize]).unwrap();
        FlatInfo { path, sector_size: stride, data_offset: prefix, total_sectors: sectors }
    }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }

#[test]
fn unequal_lengths_are_differences_in_both_scan_directions() {
    let dir = Fixture::new();
    for stride in [2048, 2352, 2448] {
        let a = dir.image("a.bin", stride, 503, 19);
        let b = dir.image("b.bin", stride, 501, 37);
        assert_eq!(find_diff_flat(&a, &b, 0, 502, true).unwrap(), Some(501));
        assert_eq!(find_diff_flat(&a, &b, 0, 502, false).unwrap(), Some(502));
        assert_eq!(find_diff_flat(&a, &b, 0, 500, false).unwrap(), None);
        assert_eq!(find_diff_flat(&a, &b, 502, 502, true).unwrap(), Some(502));
        // A real difference in the last partial chunk precedes the missing tail.
        let mut f = fs::OpenOptions::new().write(true).open(&b.path).unwrap();
        f.seek(SeekFrom::Start(b.data_offset + 499 * stride)).unwrap();
        f.write_all(&[0x99]).unwrap();
        assert_eq!(find_diff_flat(&a, &b, 0, 502, true).unwrap(), Some(499));
        assert_eq!(find_diff_flat(&a, &b, 0, 500, false).unwrap(), Some(499));
    }
}

#[test]
fn scan_errors_are_not_reported_as_identical() {
    let dir = Fixture::new();
    let a = dir.image("a.iso", 2048, 4, 0);
    let b = dir.image("b.iso", 2048, 4, 0);
    // Simulate truncation after metadata was obtained; this remains an I/O error.
    fs::OpenOptions::new().write(true).open(&b.path).unwrap().set_len(2048).unwrap();
    assert!(find_diff_flat(&a, &b, 0, 3, true).is_err());
    assert!(find_diff_sector(a.path.to_string_lossy().into_owned(), dir.0.join("missing.iso").to_string_lossy().into_owned(), 0, true, false).is_err());
}

#[test]
#[ignore = "requires DX_IMG pointing to the supplied CloneCD descriptor"]
fn clonecd_shorter_than_comparison_image() {
    let input = std::env::var("DX_IMG").unwrap();
    let b = flat_info(&input).unwrap();
    let first = read_sector_impl(&input, 0).unwrap();
    let last = read_sector_impl(&input, b.total_sectors - 1).unwrap();
    assert_eq!(first.sector_size, 2352);
    assert_eq!(b.total_sectors, 71187);
    assert_eq!(last.bytes.len(), 2352);
    let dir = Fixture::new();
    let longer = dir.0.join("longer.bin");
    fs::copy(&b.path, &longer).unwrap();
    fs::OpenOptions::new().append(true).open(&longer).unwrap().write_all(&vec![0; 2 * b.sector_size as usize]).unwrap();
    let a = FlatInfo { path: longer, sector_size: b.sector_size, data_offset: b.data_offset, total_sectors: b.total_sectors + 2 };
    assert_eq!(find_diff_flat(&a, &b, 0, a.total_sectors - 1, true).unwrap(), Some(b.total_sectors));
    assert_eq!(find_diff_flat(&a, &b, 0, a.total_sectors - 1, false).unwrap(), Some(a.total_sectors - 1));
}
