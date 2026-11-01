use super::*;
use std::fs;

#[test]
fn exclusive_publication_preserves_even_an_empty_destination() {
    let temp = tempfile::tempdir().unwrap();
    let stage = temp.path().join("stage");
    let destination = temp.path().join("destination");
    fs::create_dir(&stage).unwrap();
    fs::write(stage.join("complete"), b"complete").unwrap();
    fs::create_dir(&destination).unwrap();
    assert_eq!(
        publish_snapshot(&stage, &destination).unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(fs::read_dir(destination).unwrap().count(), 0);
    assert!(stage.join("complete").is_file());
}
