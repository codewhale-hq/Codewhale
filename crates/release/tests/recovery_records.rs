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

#[test]
fn command_restore_preserves_ownership_and_undo() {
    use codewhale_release::installs::{RECEIPT_COMMANDS, RECEIPT_FILE, sha256_hex};
    for (prior_receipt, same_bytes) in [(false, false), (true, false), (false, true)] {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path();
        let old = b"previous command";
        let new = if same_bytes {
            old.as_slice()
        } else {
            b"new command"
        };
        let receipt = dir.join(RECEIPT_FILE);
        let claims = |bytes: &[u8]| {
            RECEIPT_COMMANDS
                .iter()
                .map(|name| format!("{}  {name}\n", sha256_hex(bytes)))
                .collect::<String>()
        };
        let mut batch = RecoveryBatch::open(dir).unwrap();
        let original = batch.stamp.clone();
        if prior_receipt {
            fs::write(&receipt, claims(old)).unwrap();
            batch.move_in(&receipt).unwrap();
        }
        for name in RECEIPT_COMMANDS {
            let command = dir.join(name);
            fs::write(&command, old).unwrap();
            batch.move_in(&command).unwrap();
            fs::write(&command, new).unwrap();
        }
        batch.finish();
        fs::write(&receipt, claims(new)).unwrap();
        let unrelated = dir.join("unrelated");
        fs::write(&unrelated, b"keep me").unwrap();
        let edited = dir.join(RECEIPT_COMMANDS[1]);
        fs::write(&edited, b"my replacement").unwrap();
        assert!(restore(dir, Some(&original)).is_err());
        assert_eq!(fs::read(&edited).unwrap(), b"my replacement");
        assert_eq!(fs::read_to_string(&receipt).unwrap(), claims(new));
        assert_eq!(recovery_folders(dir).len(), 1);
        fs::write(edited, new).unwrap();
        fs::write(&receipt, b"personal note").unwrap();
        assert!(restore(dir, Some(&original)).is_err());
        assert_eq!(fs::read(&receipt).unwrap(), b"personal note");
        fs::write(&receipt, claims(new)).unwrap();
        let backup = recovery_folders(dir)[0]
            .records
            .iter()
            .find(|entry| entry.original == dir.join(RECEIPT_COMMANDS[0]))
            .unwrap()
            .stored
            .clone();
        fs::write(&backup, b"changed backup").unwrap();
        if !same_bytes || prior_receipt {
            assert!(restore(dir, Some(&original)).is_err());
            assert_eq!(fs::read_to_string(&receipt).unwrap(), claims(new));
            assert_eq!(recovery_folders(dir).len(), 1);
        }
        fs::write(backup, old).unwrap();
        let (_, report) = restore(dir, Some(&original)).unwrap();
        assert!(report.skipped.is_empty());
        for name in RECEIPT_COMMANDS {
            assert_eq!(fs::read(dir.join(name)).unwrap(), old);
        }
        assert_eq!(receipt.exists(), prior_receipt || same_bytes);
        if receipt.exists() {
            assert_eq!(fs::read_to_string(&receipt).unwrap(), claims(old));
        }
        let undo = report.displaced.unwrap();
        let folders = recovery_folders(dir);
        let saved = folders.iter().find(|f| f.stamp == undo).unwrap();
        assert_eq!(
            saved.records.iter().any(|r| r.original == receipt),
            !same_bytes
        );
        let (_, undone) = restore(dir, Some(&undo)).unwrap();
        assert!(undone.skipped.is_empty());
        for name in RECEIPT_COMMANDS {
            assert_eq!(fs::read(dir.join(name)).unwrap(), new);
        }
        assert_eq!(fs::read_to_string(&receipt).unwrap(), claims(new));
        assert_eq!(fs::read(unrelated).unwrap(), b"keep me");
    }
}
