use std::{
    fs,
    io::{self, Read},
    os::unix::fs::OpenOptionsExt,
    path::Path,
};

pub fn load_tls(path: Option<&Path>) -> io::Result<femto_microlink::host_tls::TlsConnector> {
    use femto_microlink::host_tls::TlsConnector;
    let timeout = std::time::Duration::from_secs(60);
    let Some(path) = path else {
        return TlsConnector::mozilla(timeout);
    };
    TlsConnector::with_der_root(&load_der(path)?, timeout)
}
pub fn load_service_tls(
    path: Option<&Path>,
) -> io::Result<femto_microlink::host_tcp_tls::Connector> {
    use femto_microlink::host_tcp_tls::Connector;
    match path {
        Some(path) => Connector::with_der_root(&load_der(path)?),
        None => Connector::mozilla(),
    }
}
fn load_der(path: &Path) -> io::Result<Vec<u8>> {
    if !fs::symlink_metadata(path)?.is_file() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let mut bytes = Vec::new();
    file.take(16 * 1024 + 1).read_to_end(&mut bytes)?;
    Ok(bytes)
}
