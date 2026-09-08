#![forbid(unsafe_code)]

use ath11k_qmi::{Incoming, MessageId, QmiError, RawIndication, Request, Response, TransactionId, Transport};
use qrtr_socket::{is_disconnect_error, QrtrAddr, QrtrSocket};
use std::collections::VecDeque;
use std::io;
use std::time::{Duration, Instant};

const QMI_SERVICE_ID_WLFW: u32 = 0x45;
const QRTR_PORT_CTRL: u32 = 0xffff_fffe;
const QRTR_TYPE_NEW_SERVER: u32 = 4;
const QRTR_TYPE_DEL_SERVER: u32 = 5;
const QMI_HEADER_LEN: usize = 7;
const QMI_REQUEST: u8 = 0;
const QMI_RESPONSE: u8 = 2;
const QMI_INDICATION: u8 = 4;
const MAX_PACKET_LEN: usize = QMI_HEADER_LEN + ath11k_qmi::wire::RESPONSE_MAX_LEN;
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(10);

trait Datagram {
    fn lookup(&self, service: u32, packed_instance: u32, timeout: Duration) -> io::Result<QrtrAddr>;
    fn send_to(&self, bytes: &[u8], address: QrtrAddr) -> io::Result<usize>;
    fn recv_from(&self, bytes: &mut [u8], timeout: Duration) -> io::Result<(usize, QrtrAddr)>;
}

impl Datagram for QrtrSocket {
    fn lookup(&self, service: u32, packed_instance: u32, timeout: Duration) -> io::Result<QrtrAddr> {
        QrtrSocket::lookup(self, service, packed_instance, timeout)
    }
    fn send_to(&self, bytes: &[u8], address: QrtrAddr) -> io::Result<usize> {
        QrtrSocket::send_to(self, bytes, address)
    }
    fn recv_from(&self, bytes: &mut [u8], timeout: Duration) -> io::Result<(usize, QrtrAddr)> {
        QrtrSocket::recv_from(self, bytes, timeout)
    }
}

/// Linux AF_QIPCRTR adapter for the platform-free ath11k QMI protocol crate.
pub struct QrtrTransport<S = QrtrSocket> {
    socket: S,
    epoch: Instant,
    service: Option<QrtrAddr>,
    lookup: Option<(u32, u32)>,
    next_transaction: u16,
    pending: VecDeque<Incoming>,
}

impl QrtrTransport<QrtrSocket> {
    pub fn open() -> io::Result<Self> {
        Ok(Self::with_socket(QrtrSocket::open()?))
    }
}

impl<S> QrtrTransport<S> {
    fn with_socket(socket: S) -> Self {
        Self {
            socket,
            epoch: Instant::now(),
            service: None,
            lookup: None,
            next_transaction: 0,
            pending: VecDeque::new(),
        }
    }
}

impl<S: Datagram> Transport for QrtrTransport<S> {
    fn start_service(&mut self, version: u32, instance: u32) -> Result<(), QmiError> {
        let packed_instance = version | (instance << 8);
        let address = self.socket.lookup(QMI_SERVICE_ID_WLFW, packed_instance, LOOKUP_TIMEOUT)
            .map_err(map_io_error)?;
        self.service = Some(address);
        self.lookup = Some((QMI_SERVICE_ID_WLFW, packed_instance));
        self.pending.push_back(Incoming::ServerArrived);
        Ok(())
    }

    fn stop_service(&mut self) {
        self.service = None;
        self.lookup = None;
        self.pending.clear();
    }

    fn send(&mut self, request: Request) -> Result<TransactionId, QmiError> {
        let address = self.service.ok_or(QmiError::Disconnected)?;
        self.next_transaction = self.next_transaction.wrapping_add(1);
        if self.next_transaction == 0 { self.next_transaction = 1; }
        let transaction = TransactionId::new(self.next_transaction);
        let packet = encode_request(&request, transaction)?;
        let written = self.socket.send_to(&packet, address).map_err(map_io_error)?;
        if written != packet.len() { return Err(QmiError::Transport); }
        Ok(transaction)
    }

    fn now_ns(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }

    fn receive(&mut self, timeout_ns: u64) -> Result<Incoming, QmiError> {
        if let Some(incoming) = self.pending.pop_front() { return Ok(incoming); }
        let mut packet = [0u8; MAX_PACKET_LEN];
        loop {
            let (length, source) = self.socket.recv_from(&mut packet, Duration::from_nanos(timeout_ns))
                .map_err(map_io_error)?;
            if source.port == QRTR_PORT_CTRL {
                if let Some(event) = self.handle_control(&packet[..length])? { return Ok(event); }
                continue;
            }
            if Some(source) != self.service { continue; }
            return decode_qmi_packet(&packet[..length]);
        }
    }
}

impl<S> QrtrTransport<S> {
    fn handle_control(&mut self, bytes: &[u8]) -> Result<Option<Incoming>, QmiError> {
        let fields = control_fields(bytes)?;
        match fields[0] {
            QRTR_TYPE_DEL_SERVER if self.service == Some(QrtrAddr { node: fields[3], port: fields[4] }) => {
                self.service = None;
                Ok(Some(Incoming::ServerExited))
            }
            QRTR_TYPE_NEW_SERVER if self.lookup == Some((fields[1], fields[2])) && fields[3] != 0 && fields[4] != 0 => {
                self.service = Some(QrtrAddr { node: fields[3], port: fields[4] });
                Ok(Some(Incoming::ServerArrived))
            }
            _ => Ok(None),
        }
    }
}

fn encode_request(request: &Request, transaction: TransactionId) -> Result<Vec<u8>, QmiError> {
    let length = u16::try_from(request.bytes().len()).map_err(|_| QmiError::MessageTooLong)?;
    let mut packet = Vec::with_capacity(QMI_HEADER_LEN + request.bytes().len());
    packet.push(QMI_REQUEST);
    packet.extend_from_slice(&transaction.value().to_le_bytes());
    packet.extend_from_slice(&(request.message_id() as u16).to_le_bytes());
    packet.extend_from_slice(&length.to_le_bytes());
    packet.extend_from_slice(request.bytes());
    Ok(packet)
}

fn decode_qmi_packet(packet: &[u8]) -> Result<Incoming, QmiError> {
    if packet.len() < QMI_HEADER_LEN { return Err(QmiError::Malformed); }
    let kind = packet[0];
    let transaction = TransactionId::new(u16::from_le_bytes([packet[1], packet[2]]));
    let message_id = MessageId::from_u16(u16::from_le_bytes([packet[3], packet[4]]))?;
    let length = u16::from_le_bytes([packet[5], packet[6]]) as usize;
    if packet.len() != QMI_HEADER_LEN + length { return Err(QmiError::Malformed); }
    let body = packet[QMI_HEADER_LEN..].to_vec();
    match kind {
        QMI_RESPONSE => Ok(Incoming::Response(Response::checked(transaction, message_id, body)?)),
        QMI_INDICATION => Ok(Incoming::Indication(RawIndication::checked(message_id, body)?)),
        _ => Err(QmiError::Malformed),
    }
}

fn control_fields(bytes: &[u8]) -> Result<[u32; 5], QmiError> {
    if bytes.len() < 20 { return Err(QmiError::Malformed); }
    let mut fields = [0; 5];
    for (field, chunk) in fields.iter_mut().zip(bytes[..20].chunks_exact(4)) {
        *field = u32::from_le_bytes(chunk.try_into().map_err(|_| QmiError::Malformed)?);
    }
    Ok(fields)
}

fn map_io_error(error: io::Error) -> QmiError {
    if is_disconnect_error(&error) { QmiError::Disconnected }
    else if matches!(error.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock) { QmiError::Timeout }
    else { QmiError::Transport }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ath11k_qmi::wire::{CapabilityRequest, HostCapabilityRequest};
    use std::cell::RefCell;

    struct FakeSocket {
        sent: RefCell<Vec<(Vec<u8>, QrtrAddr)>>,
        received: RefCell<VecDeque<(Vec<u8>, QrtrAddr)>>,
        server: QrtrAddr,
    }
    impl Datagram for FakeSocket {
        fn lookup(&self, service: u32, packed: u32, _: Duration) -> io::Result<QrtrAddr> {
            assert_eq!((service, packed), (0x45, 0x301)); Ok(self.server)
        }
        fn send_to(&self, bytes: &[u8], address: QrtrAddr) -> io::Result<usize> {
            self.sent.borrow_mut().push((bytes.to_vec(), address)); Ok(bytes.len())
        }
        fn recv_from(&self, bytes: &mut [u8], _: Duration) -> io::Result<(usize, QrtrAddr)> {
            let (packet, address) = self.received.borrow_mut().pop_front()
                .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "fixture exhausted"))?;
            bytes[..packet.len()].copy_from_slice(&packet); Ok((packet.len(), address))
        }
    }

    fn response(transaction: u16, id: MessageId, body: &[u8]) -> Vec<u8> {
        let mut packet=vec![QMI_RESPONSE]; packet.extend_from_slice(&transaction.to_le_bytes());
        packet.extend_from_slice(&(id as u16).to_le_bytes()); packet.extend_from_slice(&(body.len() as u16).to_le_bytes()); packet.extend_from_slice(body); packet
    }

    #[test]
    fn qmi_header_is_linux_layout() {
        let request=CapabilityRequest.encode().unwrap();
        assert_eq!(encode_request(&request,TransactionId::new(0x1234)).unwrap(), [0,0x34,0x12,0x24,0,0,0]);
    }

    #[test]
    fn malformed_and_truncated_packets_fail() {
        for length in 0..QMI_HEADER_LEN { assert_eq!(decode_qmi_packet(&[0;QMI_HEADER_LEN][..length]),Err(QmiError::Malformed)); }
        assert_eq!(decode_qmi_packet(&[QMI_RESPONSE,1,0,0x24,0,1,0]),Err(QmiError::Malformed));
    }

    #[test]
    fn transport_correlates_envelope_and_service() {
        let server=QrtrAddr{node:7,port:9};
        let socket=FakeSocket{sent:RefCell::new(Vec::new()),received:RefCell::new(VecDeque::from(vec![(response(1,MessageId::HostCapability,&[2,4,0,0,0,0,0]),server)])),server};
        let mut transport=QrtrTransport::with_socket(socket);
        transport.start_service(1,3).unwrap(); assert_eq!(transport.receive(1).unwrap(),Incoming::ServerArrived);
        let transaction=transport.send(HostCapabilityRequest::default().encode().unwrap()).unwrap();
        assert_eq!(transaction,TransactionId::new(1));
        let Incoming::Response(response)=transport.receive(1).unwrap() else {panic!("response")};
        assert_eq!((response.transaction_id(),response.message_id()),(transaction,MessageId::HostCapability));
    }

    #[test]
    fn control_delete_emits_server_exit() {
        let server=QrtrAddr{node:7,port:9}; let control=QrtrAddr{node:1,port:QRTR_PORT_CTRL};
        let mut packet=Vec::new(); for value in [QRTR_TYPE_DEL_SERVER,0x45,0x301,7,9]{packet.extend_from_slice(&value.to_le_bytes());}
        let socket=FakeSocket{sent:RefCell::new(Vec::new()),received:RefCell::new(VecDeque::from(vec![(packet,control)])),server};
        let mut transport=QrtrTransport::with_socket(socket); transport.start_service(1,3).unwrap(); let _=transport.receive(1);
        assert_eq!(transport.receive(1).unwrap(),Incoming::ServerExited);
    }
}
