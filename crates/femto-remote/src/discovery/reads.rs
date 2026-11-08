use super::Connection;

pub const MAX_MAP_BYTES: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadKind {
    ReportMap,
    ReportReference,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadRequest {
    pub connection: Connection,
    pub handle: u16,
    pub kind: ReadKind,
}

pub struct ReadResponse {
    pub request: ReadRequest,
    bytes: [u8; MAX_MAP_BYTES],
    len: usize,
}

impl ReadResponse {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

#[derive(Default)]
pub struct ReadSlot {
    pending: Option<ReadRequest>,
    response: Option<ReadResponse>,
    error: Option<&'static str>,
}

impl ReadSlot {
    pub fn error(&self) -> Option<&'static str> {
        self.error
    }
    pub fn is_idle(&self) -> bool {
        self.pending.is_none() && self.error.is_none()
    }

    pub fn begin(&mut self, request: ReadRequest) -> Result<(), &'static str> {
        if !self.is_idle() || request.handle == 0 {
            return Err("GATT read busy, failed or invalid");
        }
        self.pending = Some(request);
        Ok(())
    }

    fn fail(&mut self, error: &'static str) {
        self.error.get_or_insert(error);
        self.pending = None;
        self.response = None;
    }

    pub fn cancel(&mut self) {
        self.fail("GATT read cancelled");
    }

    /// Copies one SDK-assembled value. A different connection cannot supply it.
    pub fn complete(&mut self, request: ReadRequest, success: bool, bytes: &[u8]) {
        let Some(pending) = self.pending else {
            return;
        };
        if request.connection != pending.connection {
            return;
        }
        if request != pending || self.response.is_some() {
            self.fail("unexpected or duplicate GATT read completion");
            return;
        }
        let valid_length = match request.kind {
            ReadKind::ReportMap => (1..=MAX_MAP_BYTES).contains(&bytes.len()),
            ReadKind::ReportReference => bytes.len() == 2,
        };
        if !success || !valid_length {
            self.fail("GATT read status or length");
            return;
        }
        let mut response = ReadResponse {
            request,
            bytes: [0; MAX_MAP_BYTES],
            len: bytes.len(),
        };
        response.bytes[..bytes.len()].copy_from_slice(bytes);
        self.response = Some(response);
    }

    pub fn take(&mut self) -> Result<Option<ReadResponse>, &'static str> {
        if let Some(error) = self.error {
            return Err(error);
        }
        let response = self.response.take();
        if response.is_some() {
            self.pending = None;
        }
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const REQUEST: ReadRequest = ReadRequest {
        connection: Connection {
            interface: 1,
            id: 2,
        },
        handle: 10,
        kind: ReadKind::ReportMap,
    };

    #[test]
    fn copies_value_and_requires_consumption_before_another_read() {
        let mut slot = ReadSlot::default();
        slot.begin(REQUEST).unwrap();
        assert!(slot.take().unwrap().is_none());
        let mut bytes = [5; MAX_MAP_BYTES];
        slot.complete(REQUEST, true, &bytes);
        bytes.fill(9);
        assert!(slot.begin(REQUEST).is_err());
        assert_eq!(slot.take().unwrap().unwrap().bytes(), &[5; MAX_MAP_BYTES]);
        assert!(slot.is_idle());
        slot.begin(ReadRequest {
            kind: ReadKind::ReportReference,
            ..REQUEST
        })
        .unwrap();
    }

    #[test]
    fn rejects_wrong_handles_duplicates_lengths_and_failed_completions() {
        for (request, success, bytes) in [
            (
                ReadRequest {
                    handle: 11,
                    ..REQUEST
                },
                true,
                vec![1],
            ),
            (
                ReadRequest {
                    kind: ReadKind::ReportReference,
                    ..REQUEST
                },
                true,
                vec![1, 1],
            ),
            (REQUEST, false, vec![1]),
            (REQUEST, true, vec![]),
            (REQUEST, true, vec![0; MAX_MAP_BYTES + 1]),
        ] {
            let mut slot = ReadSlot::default();
            slot.begin(REQUEST).unwrap();
            slot.complete(request, success, &bytes);
            assert!(slot.take().is_err());
            assert!(slot.begin(REQUEST).is_err());
        }
        let mut slot = ReadSlot::default();
        slot.begin(REQUEST).unwrap();
        slot.complete(REQUEST, true, &[1]);
        slot.complete(REQUEST, true, &[2]);
        assert!(slot.take().is_err());
    }

    #[test]
    fn references_require_two_bytes_and_stale_connections_cannot_complete() {
        let request = ReadRequest {
            kind: ReadKind::ReportReference,
            ..REQUEST
        };
        for bytes in [vec![], vec![1], vec![1, 1, 1]] {
            let mut slot = ReadSlot::default();
            slot.begin(request).unwrap();
            slot.complete(request, true, &bytes);
            assert!(slot.take().is_err());
        }
        let mut slot = ReadSlot::default();
        slot.begin(request).unwrap();
        slot.complete(
            ReadRequest {
                connection: Connection {
                    id: 3,
                    ..request.connection
                },
                ..request
            },
            true,
            &[9, 9],
        );
        assert!(slot.take().unwrap().is_none());
        slot.complete(request, true, &[1, 1]);
        slot.cancel();
        assert!(slot.take().is_err());
    }
}
