use cargo_quicksilver::config::CloneBackendKind;
use cargo_quicksilver::scratch::{probe, Sharing};

#[test]
fn probe_fails_cleanly_without_cow_and_without_allow_copy() {
    let tmp = tempfile::tempdir().unwrap();
    // /tmp on this host is ext4, which does not implement FICLONE. Even if the
    // temp dir is elsewhere without CoW, the probe must refuse a silent copy.
    let err = probe(tmp.path(), CloneBackendKind::Auto, false).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("FICLONE") || msg.contains("CoW") || msg.contains("clone backend"),
        "error should name the clone chain, got:\n{msg}"
    );
    assert!(
        msg.contains("allow-copy") || msg.contains("--allow-copy"),
        "error should mention --allow-copy, got:\n{msg}"
    );
}

#[test]
fn probe_with_allow_copy_succeeds_and_does_not_claim_shared_blocks() {
    let tmp = tempfile::tempdir().unwrap();
    let report = probe(tmp.path(), CloneBackendKind::Auto, true).unwrap();
    assert_ne!(report.sharing, Sharing::SharedBlocks);
    assert!(
        matches!(
            report.sharing,
            Sharing::ServerSideCopy | Sharing::FullCopy | Sharing::Unknown
        ),
        "allow-copy path must not pretend blocks are shared: {:?}",
        report.sharing
    );
    assert!(
        report.message.to_ascii_lowercase().contains("copy") || report.message.contains("WARNING")
    );
}

#[test]
fn successful_reflink_probe_verifies_shared_blocks_when_fs_supports_it() {
    let xfs = std::path::Path::new("/mnt/containers");
    if !xfs.is_dir() {
        eprintln!("skip: /mnt/containers not mounted");
        return;
    }
    let dir = xfs.join("quicksilver-probe-test");
    let _ = std::fs::create_dir_all(&dir);
    match probe(&dir, CloneBackendKind::Auto, false) {
        Ok(report) => {
            assert_eq!(report.backend, CloneBackendKind::Reflink);
            assert_eq!(report.sharing, Sharing::SharedBlocks);
            assert!(report.message.contains("shared blocks"));
        }
        Err(err) => {
            panic!("expected reflink CoW on /mnt/containers: {err:#}");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
