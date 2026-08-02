//! Bounded, versioned Ethernet attachment for the native daemon.

use netstack3_port_spike::{EthernetFrame, MAX_FRAME_LEN};
use std::{
    io::{self, ErrorKind},
    os::{
        fd::{AsRawFd as _, OwnedFd},
        unix::net::UnixDatagram,
    },
};

const MAGIC: &[u8; 4] = b"NS3E";
const VERSION: u8 = 1;
const HEADER_LEN: usize = 8;
const ATTACH: u8 = 1;
const LINK_STATE: u8 = 2;
const RECEIVE_FRAME: u8 = 3;
const TRANSMIT_FRAME: u8 = 4;
const ACK: u8 = 5;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EthernetAttachment {
    pub mac: [u8; 6],
    pub mtu: u16,
}

#[derive(Debug, Eq, PartialEq)]
pub enum EthernetInput {
    Frame(EthernetFrame),
    LinkDown,
}

pub struct SeqpacketEthernet {
    socket: UnixDatagram,
}

impl SeqpacketEthernet {
    pub fn from_owned_fd(fd: OwnedFd) -> io::Result<Self> {
        let mut socket_type = 0;
        let mut length = std::mem::size_of_val(&socket_type) as libc::socklen_t;
        // SAFETY: the output points to an initialized integer of the supplied
        // length and `fd` stays owned for the duration of the call.
        if unsafe {
            libc::getsockopt(
                fd.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_TYPE,
                (&mut socket_type as *mut i32).cast(),
                &mut length,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        if socket_type != libc::SOCK_SEQPACKET {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                "Ethernet fd is not SOCK_SEQPACKET",
            ));
        }
        Ok(Self {
            socket: UnixDatagram::from(fd),
        })
    }

    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            socket: self.socket.try_clone()?,
        })
    }

    /// Completes versioned attach and link-up before the provider device opens.
    pub fn attach(&self) -> io::Result<EthernetAttachment> {
        let packet = self.receive_packet()?;
        if packet.opcode != ATTACH || packet.payload.len() != 8 {
            return Err(protocol_error("expected attach"));
        }
        let attachment = EthernetAttachment {
            mac: packet.payload[..6].try_into().unwrap(),
            mtu: u16::from_le_bytes(packet.payload[6..8].try_into().unwrap()),
        };
        if attachment.mac == [0; 6]
            || attachment.mac[0] & 1 != 0
            || !(1280..=1500).contains(&attachment.mtu)
        {
            return Err(protocol_error("invalid attach parameters"));
        }
        self.send_packet(ACK, &[ATTACH])?;

        let packet = self.receive_packet()?;
        if packet.opcode != LINK_STATE || packet.payload != [1] {
            return Err(protocol_error("expected link up"));
        }
        self.send_packet(ACK, &[LINK_STATE])?;
        Ok(attachment)
    }

    pub fn receive(&self) -> io::Result<EthernetInput> {
        let packet = self.receive_packet()?;
        match packet.opcode {
            RECEIVE_FRAME => EthernetFrame::copy_from_slice(&packet.payload)
                .map(EthernetInput::Frame)
                .map_err(|error| protocol_error(&format!("invalid Ethernet frame: {error:?}"))),
            LINK_STATE if packet.payload == [0] => Ok(EthernetInput::LinkDown),
            _ => Err(protocol_error("unexpected Ethernet message")),
        }
    }

    pub fn transmit(&self, frame: &EthernetFrame) -> io::Result<()> {
        self.send_packet(TRANSMIT_FRAME, frame.as_bytes())
    }

    fn receive_packet(&self) -> io::Result<Packet> {
        let mut bytes = [0; HEADER_LEN + MAX_FRAME_LEN + 1];
        let length = loop {
            match self.socket.recv(&mut bytes) {
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                result => break result?,
            }
        };
        if length == 0 {
            return Err(io::Error::new(
                ErrorKind::BrokenPipe,
                "Ethernet peer closed",
            ));
        }
        decode(&bytes[..length])
    }

    fn send_packet(&self, opcode: u8, payload: &[u8]) -> io::Result<()> {
        let packet = encode(opcode, payload)?;
        let sent = loop {
            match self.socket.send(&packet) {
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                result => break result?,
            }
        };
        if sent != packet.len() {
            return Err(io::Error::new(
                ErrorKind::WriteZero,
                "partial seqpacket write",
            ));
        }
        Ok(())
    }
}

struct Packet {
    opcode: u8,
    payload: Vec<u8>,
}

fn encode(opcode: u8, payload: &[u8]) -> io::Result<Vec<u8>> {
    let length = u16::try_from(payload.len()).map_err(|_| protocol_error("oversize payload"))?;
    let mut packet = Vec::with_capacity(HEADER_LEN + payload.len());
    packet.extend(MAGIC);
    packet.push(VERSION);
    packet.push(opcode);
    packet.extend(length.to_le_bytes());
    packet.extend(payload);
    Ok(packet)
}

fn decode(bytes: &[u8]) -> io::Result<Packet> {
    if bytes.len() < HEADER_LEN
        || &bytes[..4] != MAGIC
        || bytes[4] != VERSION
        || bytes.len() != HEADER_LEN + usize::from(u16::from_le_bytes([bytes[6], bytes[7]]))
    {
        return Err(protocol_error("invalid Ethernet packet"));
    }
    Ok(Packet {
        opcode: bytes[5],
        payload: bytes[HEADER_LEN..].to_vec(),
    })
}

fn protocol_error(message: &str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::FromRawFd as _;

    fn pair() -> (SeqpacketEthernet, UnixDatagram) {
        let mut fds = [-1; 2];
        // SAFETY: `fds` has room for both returned descriptors. Each descriptor
        // is transferred exactly once into an owning type below.
        assert_eq!(
            unsafe {
                libc::socketpair(
                    libc::AF_UNIX,
                    libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
                    0,
                    fds.as_mut_ptr(),
                )
            },
            0
        );
        // SAFETY: socketpair returned two fresh, owned descriptors.
        let adapter =
            SeqpacketEthernet::from_owned_fd(unsafe { OwnedFd::from_raw_fd(fds[0]) }).unwrap();
        // SAFETY: ownership of the second fresh descriptor moves here.
        let peer = unsafe { UnixDatagram::from_raw_fd(fds[1]) };
        (adapter, peer)
    }

    fn send(peer: &UnixDatagram, opcode: u8, payload: &[u8]) {
        let packet = encode(opcode, payload).unwrap();
        assert_eq!(peer.send(&packet).unwrap(), packet.len());
    }

    #[test]
    fn attach_frames_link_down_and_peer_loss_are_explicit() {
        let (adapter, peer) = pair();
        let mut attach = [0; 8];
        attach[..6].copy_from_slice(&[2, 0, 0, 0, 0, 1]);
        attach[6..].copy_from_slice(&1500u16.to_le_bytes());
        send(&peer, ATTACH, &attach);
        send(&peer, LINK_STATE, &[1]);
        assert_eq!(
            adapter.attach().unwrap(),
            EthernetAttachment {
                mac: [2, 0, 0, 0, 0, 1],
                mtu: 1500,
            }
        );
        let mut ack = [0; 32];
        assert!(peer.recv(&mut ack).unwrap() > HEADER_LEN);
        assert!(peer.recv(&mut ack).unwrap() > HEADER_LEN);

        let frame = vec![0x5a; 14];
        send(&peer, RECEIVE_FRAME, &frame);
        assert_eq!(
            adapter.receive().unwrap(),
            EthernetInput::Frame(EthernetFrame::try_from(frame).unwrap())
        );
        send(&peer, LINK_STATE, &[0]);
        assert_eq!(adapter.receive().unwrap(), EthernetInput::LinkDown);
        drop(peer);
        assert_eq!(adapter.receive().unwrap_err().kind(), ErrorKind::BrokenPipe);
    }

    #[test]
    fn malformed_or_oversize_frames_are_rejected() {
        let (adapter, peer) = pair();
        send(&peer, RECEIVE_FRAME, &[0; 13]);
        assert_eq!(
            adapter.receive().unwrap_err().kind(),
            ErrorKind::InvalidData
        );

        let packet = encode(RECEIVE_FRAME, &[0; MAX_FRAME_LEN + 1]).unwrap();
        peer.send(&packet).unwrap();
        assert_eq!(
            adapter.receive().unwrap_err().kind(),
            ErrorKind::InvalidData
        );
    }
}
