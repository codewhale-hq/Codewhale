use std::fs;

use codewhale_release::installs::{RECOVERY_MANIFEST, RecoveryBatch, recovery_folders, restore};

#[test]
fn recovery_size_counts_nested_folders() {
    let home = tempfile::tempdir().unwrap();
    let leftover = home.path().join(".codewhale-update-old");
    fs::create_dir_all(leftover.join("a/b")).unwrap();
    fs::write(leftover.join("top"), [0u8; 10]).unwrap();
    fs::write(leftover.join("a/mid"), [0u8; 20]).unwrap();
    fs::write(leftover.join("a/b/deep"), [0u8; 30]).unwrap();

    let mut batch = RecoveryBatch::open(home.path()).unwrap();
    let moved = batch.move_in(&leftover).unwrap();
    batch.finish();

    assert_eq!(moved.bytes, 60);
    let folders = recovery_folders(home.path());
    assert_eq!(folders[0].bytes(), 60);
}

#[test]
fn restore_refuses_an_unhashed_file_whose_size_changed() {
    let home = tempfile::tempdir().unwrap();
    let original = home.path().join("codewhale.bak");
    fs::write(&original, b"12345").unwrap();

    let mut batch = RecoveryBatch::open(home.path()).unwrap();
    let moved = batch.move_in(&original).unwrap();
    let dir = batch.dir.clone();
    batch.finish();

    let manifest = dir.join(RECOVERY_MANIFEST);
    let line = format!("{}\t{}\t-\t5\n", original.display(), moved.to.display());
    fs::write(&manifest, line).unwrap();
    fs::write(&moved.to, b"123456789").unwrap();

    let (_, report) = restore(home.path(), None).unwrap();
    assert!(report.restored.is_empty());
    assert_eq!(report.skipped.len(), 1);
    assert!(!original.exists());
    assert!(moved.to.exists());
}

#[test]
fn restore_refuses_hashed_record_swapped_for_symlink_or_directory() {
    let home = tempfile::tempdir().unwrap();
    let dir_target = home.path().join("outside");
    fs::create_dir(&dir_target).unwrap();

    for swap_dir in [false, true] {
        let original = home
            .path()
            .join(if swap_dir { "bin_dir" } else { "bin_link" });
        fs::write(&original, b"binary-bytes").unwrap();

        let mut batch = RecoveryBatch::open(home.path()).unwrap();
        let moved = batch.move_in(&original).unwrap();
        batch.finish();

        fs::remove_file(&moved.to).unwrap();
        if swap_dir {
            fs::create_dir(&moved.to).unwrap();
        } else {
            #[cfg(unix)]
            std::os::unix::fs::symlink(&dir_target, &moved.to).unwrap();
            #[cfg(windows)]
            fs::create_dir(&moved.to).unwrap();
        }

        let (_, report) = restore(home.path(), None).unwrap();
        assert!(report.restored.is_empty());
        assert_eq!(report.skipped.len(), 1);
        assert!(!original.exists());
        assert!(fs::symlink_metadata(&moved.to).is_ok());

        let _ = fs::remove_dir_all(home.path().join("recovery"));
    }
}
