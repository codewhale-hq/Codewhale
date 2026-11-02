//! Real Windows kernel/host receipts. These are not source-only substitutes.
use super::*;
use crate::extension_host::supervisor::{HostSandbox, MemoryEnforcement};
use crate::extension_host::test_support::{FixturePlugins, host_tool};
use crate::extension_host::{ExtensionHostManager, ExtensionHostOptions, HostStatus};
use crate::tools::spec::ToolContext;
use codewhale_config::extension_host::ExtensionHostRuntime;
use codewhale_plugins::activation::TestPolicyGuard;
use serde_json::json;

const ACL_LOCK_FIXTURE_DIR: &str = "CODEWHALE_WINDOWS_ACL_LOCK_FIXTURE_DIR";

#[test]
fn windows_acl_edit_guard_fixture() {
    let Some(root) = std::env::var_os(ACL_LOCK_FIXTURE_DIR).map(PathBuf::from) else {
        return;
    };
    fs::write(root.join("ready"), b"ready").unwrap();
    let _guard = AclEditGuard::acquire().unwrap();
    fs::write(root.join("acquired"), b"acquired").unwrap();
}

#[test]
fn windows_acl_edit_guard_serializes_processes() {
    // Retain the owned child even during an assertion panic. This fixture
    // starts no descendants; cleanup never targets any other process.
    struct FixtureChild(std::process::Child);
    impl Drop for FixtureChild {
        fn drop(&mut self) {
            if !matches!(self.0.try_wait(), Ok(Some(_))) {
                let _ = self.0.kill();
            }
            let _ = self.0.wait();
        }
    }

    let root = tempfile::tempdir().unwrap();
    let ready = root.path().join("ready");
    let acquired = root.path().join("acquired");
    let guard = AclEditGuard::acquire().unwrap();
    let mut child = FixtureChild(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "extension_host::windows::tests::windows_acl_edit_guard_fixture",
                "--nocapture",
                "--test-threads=1",
            ])
            // The unique marker directory is supplied only to this child.
            // No process-global environment mutation or env lock is needed.
            .env(ACL_LOCK_FIXTURE_DIR, root.path())
            .spawn()
            .unwrap(),
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !ready.exists() {
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "ACL fixture exited before its ready marker; exact fixture must run"
        );
        assert!(
            std::time::Instant::now() < deadline,
            "ACL fixture was not ready"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let held_until = std::time::Instant::now() + Duration::from_millis(200);
    while std::time::Instant::now() < held_until {
        assert!(
            !acquired.exists(),
            "child acquired the parent's held ACL lock"
        );
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "ACL fixture exited while the parent still held the lock"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!acquired.exists(), "child acquired before parent release");
    drop(guard);

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert!(status.success(), "ACL fixture failed: {status}");
            assert!(
                acquired.exists(),
                "fixture exited without acquiring the lock"
            );
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "ACL fixture did not acquire and exit after parent release"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn runtime(choice: ExtensionHostRuntime, override_path: Option<&Path>) -> Option<HostRuntime> {
    let resolution = codewhale_dependencies::resolve_extension_host_runtime(
        choice,
        (choice == ExtensionHostRuntime::Node)
            .then_some(override_path)
            .flatten(),
        (choice == ExtensionHostRuntime::Bun)
            .then_some(override_path)
            .flatten(),
        crate::extension_host::bundle_sha256(),
    );
    match resolution.selected {
        Some(runtime) => Some(runtime),
        None if std::env::var_os("CODEWHALE_EXT_HOST_TESTS").is_some()
            || std::env::var_os("CODEWHALE_EXT_HOST_BUN_TESTS").is_some() =>
        {
            panic!("required Windows runtime: {}", resolution.failure())
        }
        None => {
            eprintln!(
                "Windows runtime receipt unavailable: {}",
                resolution.failure()
            );
            None
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
async fn owner_boundary(runtime: HostRuntime) {
    let _policy = TestPolicyGuard::extension_host(true);
    let fixture = FixturePlugins::new(&["secret-probe"]).await;
    let choice = match runtime.kind {
        codewhale_dependencies::HostRuntimeKind::Node => ExtensionHostRuntime::Node,
        codewhale_dependencies::HostRuntimeKind::Bun => ExtensionHostRuntime::Bun,
    };
    let registry = fixture.registry();
    let plugin = registry.get("secret-probe").unwrap();
    let source = plugin.base_path.join("index.mjs");
    let staged = plugin.staged_root.as_ref().unwrap().join("index.mjs");
    assert_ne!(source, staged);
    let manager = Arc::new(ExtensionHostManager::new(ExtensionHostOptions {
        runtime: choice,
        node_override: (choice == ExtensionHostRuntime::Node).then_some(runtime.path.clone()),
        bun_override: (choice == ExtensionHostRuntime::Bun).then_some(runtime.path),
        root: Some(fixture.root.clone()),
        ..Default::default()
    }));
    let engine = manager.attach(registry);
    engine.sync().await.unwrap();
    let HostStatus::Ready {
        sandbox, memory, ..
    } = manager.status()
    else {
        panic!(
            "Windows Native failed: {:?}; {:?}",
            manager.status(),
            manager.diagnostics()
        );
    };
    assert_eq!(sandbox, HostSandbox::Wrapped("windows-lpac".into()));
    assert_eq!(memory, MemoryEnforcement::JobObject);
    let context = ToolContext::new(fixture.workspace()).with_plugin_registry(engine.plugin_view());
    let read = host_tool(&engine, fixture.workspace(), "probe_read");
    let write = host_tool(&engine, fixture.workspace(), "probe_write");
    let staged_read = read
        .execute(json!({"path":staged}), &context)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&staged_read.content).unwrap()["ok"],
        true,
        "reviewed runtime root must be readable by the real Native module"
    );
    for path in [
        source,
        fixture.workspace().join("ungranted.txt"),
        fixture
            .root
            .join("extension-host/builtin-data/private.json"),
    ] {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        if !path.exists() {
            std::fs::write(&path, b"synthetic-private-control").unwrap();
        }
        let result = read.execute(json!({"path":path}), &context).await.unwrap();
        let result: serde_json::Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(
            result["ok"], false,
            "mutable/ungranted roots must remain unreadable"
        );
        assert!(
            matches!(result["code"].as_str(), Some("EACCES" | "EPERM")),
            "must be an access denial, not a missing file"
        );
    }
    let data = fixture.root.join("extension-host/data/positive-write.txt");
    let result = write.execute(json!({"path":data}), &context).await.unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&result.content).unwrap()["ok"],
        true
    );
    let denied = staged.parent().unwrap().join("forbidden-write.txt");
    let result = write
        .execute(json!({"path":denied}), &context)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&result.content).unwrap()["ok"],
        false
    );
    assert!(!denied.exists(), "readonly snapshot was modified");
    engine.set_plugins(fixture.disable("secret-probe"));
    engine.sync().await.unwrap();
    assert!(
        read.execute(json!({"path":staged}), &context)
            .await
            .is_err(),
        "revoked caller cannot use an earlier handle"
    );
    manager.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn windows_native_lpac_node_owner_boundary() {
    if let Some(runtime) = runtime(ExtensionHostRuntime::Node, None) {
        owner_boundary(runtime).await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn windows_native_lpac_bun_owner_boundary() {
    if let Some(runtime) = runtime(ExtensionHostRuntime::Bun, None) {
        owner_boundary(runtime).await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn windows_native_lpac_compiled_owner_boundary() {
    let Some(binary) = std::env::var_os("CODEWHALE_COMPILED_HOST_TEST_BINARY") else {
        assert!(
            std::env::var_os("CODEWHALE_EXT_HOST_TESTS").is_none(),
            "required Windows compiled-host receipt is missing"
        );
        eprintln!(
            "compiled Windows receipt unavailable: compile the canonical bundle and name its image"
        );
        return;
    };
    let runtime = runtime(ExtensionHostRuntime::Bun, Some(Path::new(&binary))).unwrap();
    assert!(
        runtime.compiled,
        "test must use the exact canonical compiled-host filename/identity"
    );
    owner_boundary(runtime).await;
}

#[test]
fn windows_native_profiles_are_distinct_and_acl_grant_refuses_junctions() {
    let first = Profile::create().unwrap();
    let second = Profile::create().unwrap();
    assert_ne!(first.name, second.name);
    assert_eq!(
        unsafe { EqualSid(first.sid, second.sid) },
        0,
        "retired profile ACLs cannot be inherited by a new host"
    );
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let link = root.path().join("junction");
    let outside_file = outside.path().join("ungranted.txt");
    std::fs::write(&outside_file, b"outside control").unwrap();
    // Junction creation needs no symlink privilege; every failure is a red
    // Windows receipt, never a silent environment skip.
    let status = std::process::Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(&link)
        .arg(outside.path())
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "cannot create the actual junction control"
    );
    assert!(WindowsDirectory::open(&link).is_err());
    let sandbox = NativeSandbox {
        profile: Arc::new(first),
        _assets: Arc::new(tempfile::tempdir().unwrap()),
        program: PathBuf::new(),
        data: root.path().to_path_buf(),
    };
    let result = sandbox.admit_root(root.path());
    // Remove only the exact junction we created before tempfile cleanup.
    std::fs::remove_dir(&link).unwrap();
    assert!(
        result.is_err(),
        "grant traversal must refuse reparse points"
    );
    assert_no_profile_grant(outside.path(), sandbox.profile.sid);
    assert_no_profile_grant(&outside_file, sandbox.profile.sid);
}

#[test]
fn windows_sandbox_args_use_granted_bun_config_without_changing_other_values() {
    let runtime_dir = Path::new("runtime copy 鲸鱼");
    let args = vec![
        "--no-addons".into(),
        "--config=NUL".into(),
        "--input-type=module".into(),
        "-e".into(),
        "console.log('--config=NUL')".into(),
        String::new(),
    ];
    let mapped = sandbox_args(&args, runtime_dir);
    let mut expected = args.clone();
    expected[1] = format!("--config={}", runtime_dir.join(EMPTY_BUN_CONFIG).display());
    assert_eq!(mapped, expected);
    assert_eq!(sandbox_args(&mapped, runtime_dir), mapped);
}

#[test]
fn windows_sandbox_args_preserve_node_and_compiled_arguments() {
    for args in [
        vec!["--no-addons".into(), "--input-type=module".into()],
        vec!["--tier=plugin".into(), "--windows-sandbox-probe".into()],
        vec!["--config=NUL.toml".into(), "--config=custom.toml".into()],
        Vec::new(),
    ] {
        assert_eq!(sandbox_args(&args, Path::new("runtime copy")), args);
    }
}

#[test]
fn windows_argv_and_environment_keep_exact_values_and_reject_nul() {
    let args = vec![
        String::new(),
        "C:\\folder with space\\".into(),
        "a\"b".into(),
        "鲸鱼".into(),
    ];
    let line = command_line(OsStr::new("C:\\runtime folder\\node.exe"), &args).unwrap();
    assert_eq!(
        String::from_utf16(&line[..line.len() - 1]).unwrap(),
        "\"C:\\runtime folder\\node.exe\" \"\" \"C:\\folder with space\\\\\" \"a\\\"b\" \"鲸鱼\""
    );
    assert!(command_line(OsStr::new("node.exe"), &["x\0y".into()]).is_err());
    assert!(environment_block(&[("A".into(), "value\0tail".into())]).is_err());
    assert!(environment_block(&[("A=B".into(), "value".into())]).is_err());
    let env = environment_block(&[
        ("Z".into(), "last".into()),
        ("a".into(), "first=literal".into()),
    ])
    .unwrap();
    assert_eq!(
        String::from_utf16(&env).unwrap(),
        "a=first=literal\0Z=last\0\0"
    );
}

// Check the actual external ACL, not only the resolver's return value. The
// current user still owns these controls; our AppContainer SID must never gain
// an allow ACE through SetSecurityInfo's descendant propagation.
fn assert_no_profile_grant(path: &Path, profile: PSID) {
    assert_eq!(
        acl_snapshot(path, None),
        acl_snapshot(path, Some(profile)),
        "outside control {} received the sandbox SID",
        path.display()
    );
}
fn acl_snapshot(path: &Path, exclude: Option<PSID>) -> Vec<Vec<u8>> {
    acl_snapshot_view(path, exclude, false).1
}
fn acl_snapshot_view(path: &Path, exclude: Option<PSID>, raw: bool) -> (u16, Vec<Vec<u8>>) {
    use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT};
    use windows_sys::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACE_HEADER, GetAce, GetSecurityDescriptorControl,
    };
    use windows_sys::Win32::System::SystemServices::ACCESS_ALLOWED_ACE_TYPE;
    let directory;
    let file;
    let opened = if path.is_dir() {
        directory = WindowsDirectory::open(path).unwrap();
        directory.acl_handle().unwrap()
    } else {
        file = fs::OpenOptions::new()
            .access_mode(READ_CONTROL | FILE_READ_ATTRIBUTES)
            .share_mode(1)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .unwrap();
        &file
    };
    let raw_descriptor = raw.then(|| RawDacl::read(opened).unwrap());
    let mut _descriptor = None;
    let mut control = 0_u16;
    let mut dacl = null_mut();
    if let Some(raw) = &raw_descriptor {
        dacl = raw.acl();
        control = raw.control;
    } else {
        let mut descriptor = null_mut();
        let error = unsafe {
            GetSecurityInfo(
                opened.as_raw_handle(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                &mut dacl,
                null_mut(),
                &mut descriptor,
            )
        };
        assert_eq!(error, 0, "cannot inspect control ACL");
        _descriptor = Some(LocalAllocation(descriptor));
        let mut revision = 0;
        assert_ne!(
            unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) },
            0
        );
    }
    let dacl = std::ptr::NonNull::new(dacl).expect("control must have an actual DACL");
    let mut result = Vec::new();
    for index in 0..unsafe { dacl.as_ref().AceCount as u32 } {
        let mut ace = null_mut();
        assert_ne!(unsafe { GetAce(dacl.as_ptr(), index, &mut ace) }, 0);
        let ace = std::ptr::NonNull::new(ace).expect("GetAce must return an actual ACE");
        let header = unsafe { ace.cast::<ACE_HEADER>().as_ref() };
        assert!(header.AceSize as usize >= std::mem::size_of::<ACE_HEADER>());
        if header.AceType as u32 == ACCESS_ALLOWED_ACE_TYPE {
            assert!(header.AceSize as usize >= std::mem::size_of::<ACCESS_ALLOWED_ACE>());
            let sid = unsafe {
                std::ptr::addr_of_mut!((*ace.cast::<ACCESS_ALLOWED_ACE>().as_ptr()).SidStart)
            };
            if exclude.is_some_and(|profile| unsafe { EqualSid(profile, sid.cast()) } != 0) {
                continue;
            }
        }
        result.push(
            unsafe {
                std::slice::from_raw_parts(ace.cast::<u8>().as_ptr(), header.AceSize as usize)
            }
            .to_vec(),
        );
    }
    (control, result)
}

#[test]
fn windows_raw_dacl_rejects_truncated_unrestricted_and_malformed_descriptors() {
    use windows_sys::Win32::Security::{
        ACL_REVISION, InitializeAcl, SE_DACL_PRESENT, SE_SELF_RELATIVE,
        SECURITY_DESCRIPTOR_RELATIVE,
    };
    fn fixture() -> Vec<u32> {
        let length = size_of::<SECURITY_DESCRIPTOR_RELATIVE>() + size_of::<ACL>();
        let mut storage = vec![0_u32; length.div_ceil(size_of::<u32>())];
        let descriptor = storage.as_mut_ptr().cast::<SECURITY_DESCRIPTOR_RELATIVE>();
        // SAFETY: the aligned allocation contains both complete structures.
        unsafe {
            (*descriptor).Revision = 1;
            (*descriptor).Control = SE_SELF_RELATIVE | SE_DACL_PRESENT;
            (*descriptor).Dacl = size_of::<SECURITY_DESCRIPTOR_RELATIVE>() as u32;
            let acl = storage
                .as_mut_ptr()
                .cast::<u8>()
                .add(size_of::<SECURITY_DESCRIPTOR_RELATIVE>())
                .cast::<ACL>();
            assert_ne!(InitializeAcl(acl, size_of::<ACL>() as u32, ACL_REVISION), 0);
        }
        storage
    }
    let valid = fixture();
    let length = valid.len() * size_of::<u32>();
    let descriptor = RawDacl::from_storage(valid.clone(), length).unwrap();
    assert_eq!(descriptor.control, SE_SELF_RELATIVE | SE_DACL_PRESENT);
    assert_eq!(
        unsafe { (*descriptor.acl()).AceCount },
        0,
        "an empty restrictive ACL is valid"
    );
    assert!(RawDacl::from_storage(Vec::new(), 0).is_err());
    assert!(RawDacl::from_storage(valid.clone(), length + 1).is_err());
    assert!(
        RawDacl::from_storage(valid.clone(), size_of::<SECURITY_DESCRIPTOR_RELATIVE>() - 1)
            .is_err()
    );
    for (control, offset) in [
        (
            SE_DACL_PRESENT,
            size_of::<SECURITY_DESCRIPTOR_RELATIVE>() as u32,
        ),
        (SE_SELF_RELATIVE, 0),
        (SE_SELF_RELATIVE | SE_DACL_PRESENT, 0),
        (SE_SELF_RELATIVE | SE_DACL_PRESENT, 4),
        (SE_SELF_RELATIVE | SE_DACL_PRESENT, 21),
        (SE_SELF_RELATIVE | SE_DACL_PRESENT, length as u32),
    ] {
        let mut storage = valid.clone();
        let header = storage.as_mut_ptr().cast::<SECURITY_DESCRIPTOR_RELATIVE>();
        unsafe {
            (*header).Control = control;
            (*header).Dacl = offset;
        }
        assert!(
            RawDacl::from_storage(storage, length).is_err(),
            "control={control:#x}, offset={offset}"
        );
    }
    for (size, count) in [(0, 0), (u16::MAX, 0), (size_of::<ACL>() as u16, 1)] {
        let mut storage = valid.clone();
        let acl = unsafe {
            storage
                .as_mut_ptr()
                .cast::<u8>()
                .add(size_of::<SECURITY_DESCRIPTOR_RELATIVE>())
                .cast::<ACL>()
        };
        unsafe {
            (*acl).AclSize = size;
            (*acl).AceCount = count;
        }
        assert!(
            RawDacl::from_storage(storage, length).is_err(),
            "size={size}, count={count}"
        );
    }
    // A complete allow ACE with S-1-5-32 provides the positive control. A
    // bounded ACL alone must not authorize EqualSid to read past that ACE.
    let mut allowed = fixture();
    let entry_offset = length;
    allowed.resize(allowed.len() + 5, 0);
    let allowed_length = allowed.len() * size_of::<u32>();
    unsafe {
        let bytes =
            std::slice::from_raw_parts_mut(allowed.as_mut_ptr().cast::<u8>(), allowed_length);
        bytes[size_of::<SECURITY_DESCRIPTOR_RELATIVE>() + 2..][..2]
            .copy_from_slice(&((size_of::<ACL>() + 20) as u16).to_le_bytes());
        bytes[size_of::<SECURITY_DESCRIPTOR_RELATIVE>() + 4] = 1; // AceCount
        bytes[entry_offset + 2..][..2].copy_from_slice(&20_u16.to_le_bytes());
        bytes[entry_offset + 4] = 1; // Mask
        bytes[entry_offset + 8..][..12].copy_from_slice(&[1, 1, 0, 0, 0, 0, 0, 5, 32, 0, 0, 0]);
    }
    assert!(RawDacl::from_storage(allowed.clone(), allowed_length).is_ok());
    for (offset, value) in [(2, 12), (8, 2), (9, 2)] {
        let mut malformed = allowed.clone();
        unsafe {
            *malformed
                .as_mut_ptr()
                .cast::<u8>()
                .add(entry_offset + offset) = value;
        }
        assert!(
            RawDacl::from_storage(malformed, allowed_length).is_err(),
            "malformed allow ACE at byte {offset}"
        );
    }
}

#[test]
fn windows_profile_retirement_removes_only_its_grants_and_inherited_data_on_restarts() {
    use windows_sys::Win32::Security::GetLengthSid;
    let data = tempfile::tempdir().unwrap();
    let original_file = data.path().join("existing-state.json");
    fs::write(&original_file, b"retained user state").unwrap();
    let original_root_acl = acl_snapshot(data.path(), None);
    let original_file_acl = acl_snapshot(&original_file, None);
    let original_raw_root_acl = acl_snapshot_view(data.path(), None, true);
    let original_raw_file_acl = acl_snapshot_view(&original_file, None, true);
    let make = || NativeSandbox {
        profile: Arc::new(Profile::create().unwrap()),
        _assets: Arc::new(tempfile::tempdir().unwrap()),
        program: PathBuf::new(),
        data: data.path().to_path_buf(),
    };
    // A concurrent/new profile's exact grants must survive old-profile cleanup.
    let other = make();
    other.grant_tree(data.path(), true).unwrap();
    // Check stored ACE bytes and all control bits before taking a later
    // baseline. GetSecurityInfo can temporarily normalize the child's view
    // while its parent carries a grant; no flag masking is allowed here.
    assert_eq!(
        acl_snapshot_view(data.path(), Some(other.profile.sid), true),
        original_raw_root_acl,
        "first profile grant changed pre-existing directory ACEs"
    );
    assert_eq!(
        acl_snapshot_view(&original_file, Some(other.profile.sid), true),
        original_raw_file_acl,
        "first profile grant changed pre-existing file ACEs"
    );
    for turn in 0..8 {
        let current = make();
        current.grant_tree(data.path(), true).unwrap();
        let root_without_current = acl_snapshot(data.path(), Some(current.profile.sid));
        let file_without_current = acl_snapshot(&original_file, Some(current.profile.sid));
        // Creation after admission exercises inherited grants, not only the
        // immutable list of files that happened to exist during prepare.
        let nested = data.path().join(format!("new-state-{turn}"));
        fs::create_dir(&nested).unwrap();
        let created = nested.join("state.json");
        fs::write(&created, b"new persisted state").unwrap();
        let length = unsafe { GetLengthSid(current.profile.sid) } as usize;
        let mut sid_copy = vec![0_usize; length.div_ceil(std::mem::size_of::<usize>())];
        unsafe {
            std::ptr::copy_nonoverlapping(
                current.profile.sid as *const u8,
                sid_copy.as_mut_ptr().cast::<u8>(),
                length,
            );
        }
        let retired_sid = sid_copy.as_mut_ptr().cast();
        assert_ne!(
            acl_snapshot(&created, None),
            acl_snapshot(&created, Some(retired_sid)),
            "control must really inherit the current profile grant"
        );
        // A new/live owner may still have a data writer open. Exact ACL
        // retirement must not fail only because legitimate writes continue.
        let writer = fs::OpenOptions::new()
            .write(true)
            .open(&original_file)
            .unwrap();
        drop(current); // no Tokio context here: exact retirement is synchronous
        drop(writer);
        assert_eq!(acl_snapshot(data.path(), None), root_without_current);
        assert_eq!(acl_snapshot(&original_file, None), file_without_current);
        assert_no_profile_grant(&nested, retired_sid);
        assert_no_profile_grant(&created, retired_sid);
        assert_eq!(fs::read(&created).unwrap(), b"new persisted state");
        assert_eq!(fs::read(&original_file).unwrap(), b"retained user state");
    }
    drop(other);
    assert_eq!(
        acl_snapshot(data.path(), None),
        original_root_acl,
        "final profile retirement changed pre-existing directory ACEs"
    );
    assert_eq!(
        acl_snapshot(&original_file, None),
        original_file_acl,
        "final profile retirement changed pre-existing file ACEs"
    );
}

#[test]
fn windows_bundle_directory_listing_never_grants_children_and_retires_exactly() {
    let root = tempfile::tempdir().unwrap();
    let sibling = root.path().join("builtin-private.json");
    fs::write(&sibling, b"retained private data").unwrap();
    let before = acl_snapshot_view(root.path(), None, true);
    let sibling_before = acl_snapshot_view(&sibling, None, true);
    let sandbox = NativeSandbox {
        profile: Arc::new(Profile::create().unwrap()),
        _assets: Arc::new(tempfile::tempdir().unwrap()),
        program: PathBuf::new(),
        data: root.path().to_path_buf(),
    };
    sandbox.grant_directory(root.path()).unwrap();
    assert_ne!(acl_snapshot_view(root.path(), None, true), before);
    assert_eq!(
        acl_snapshot_view(root.path(), Some(sandbox.profile.sid), true),
        before,
        "directory listing must preserve every pre-existing ACE and control bit"
    );
    assert_no_profile_grant(&sibling, sandbox.profile.sid);
    let created = root.path().join("created-after-grant.json");
    fs::write(&created, b"private after admission").unwrap();
    assert_no_profile_grant(&created, sandbox.profile.sid);
    drop(sandbox);
    assert_eq!(acl_snapshot_view(root.path(), None, true), before);
    assert_eq!(acl_snapshot_view(&sibling, None, true), sibling_before);
    assert_eq!(fs::read(&created).unwrap(), b"private after admission");
}

#[test]
fn windows_profile_directory_budget_and_recorded_identity_refuse_before_overwrite() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("first"), b"one").unwrap();
    fs::write(root.path().join("second"), b"two").unwrap();
    // Budget includes pending paths, so even a flat directory fails before
    // growing the allocation past the bound. Admission and retirement share
    // this exact production helper.
    let mut pending = vec![PathBuf::from("already-pending"); MAX_GRANT_ENTRIES - 3];
    assert!(enqueue_children(root.path(), &mut pending, 2).is_err());
    assert_eq!(2 + pending.len(), MAX_GRANT_ENTRIES);
    let profile = Profile::create().unwrap();
    let path = root.path().join("first");
    let original = File::open(&path).unwrap();
    profile.remember(&path, &original, GrantKind::File).unwrap();
    let identity = profile.grants.lock().unwrap().get(&path).unwrap().identity;
    drop(original);
    fs::rename(&path, root.path().join("moved-original")).unwrap();
    fs::write(&path, b"different object").unwrap();
    let replacement = File::open(&path).unwrap();
    assert!(
        profile
            .remember(&path, &replacement, GrantKind::File)
            .is_err()
    );
    assert_eq!(
        profile.grants.lock().unwrap().get(&path).unwrap().identity,
        identity
    );
    // No ACL was granted in this accounting-only check. Do not schedule an
    // irrelevant retirement against the deliberately replaced fixture path.
    profile.grants.lock().unwrap().clear();
}
