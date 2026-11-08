//! Local connection failures exercise journal reuse; no coordinator is contacted.
use femto_microlink::{protocol::identity::DeviceIdentity, store::HostStore};
use std::{net::TcpListener, process::Command};

#[test]
fn failed_rotation_connections_keep_the_same_durable_candidate() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("identity");
    let store = HostStore::open(&directory).unwrap();
    let old = store.identity().unwrap().node_key().public();
    drop(store);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
            let stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && std::time::Instant::now() < deadline =>
                    {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    Err(error) => panic!("local test connection failed: {error}"),
                }
            };
            drop(stream);
        }
    });
    let control_key = DeviceIdentity::generate()
        .unwrap()
        .machine_key()
        .public()
        .to_string();
    let mut candidate = None;
    for _ in 0..2 {
        let result = Command::new(env!("CARGO_BIN_EXE_femto"))
            .arg("tailnet-connect")
            .arg(&directory)
            .args([
                "--host",
                "control.example",
                "--endpoint",
                &address.to_string(),
                "--control-key",
                &control_key,
                "--rotate-node-key",
                "--register-only",
            ])
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(result.stdout.is_empty());
        let mut store = HostStore::open(&directory).unwrap();
        assert!(store.identity().is_err());
        let pending = store
            .begin_node_key_rotation("control.example", address.port())
            .unwrap();
        assert_eq!(pending.old_node_key(), old);
        let current = pending.candidate().node_key().public();
        if let Some(expected) = candidate {
            assert_eq!(current, expected);
        }
        candidate = Some(current);
    }
    server.join().unwrap();
}
