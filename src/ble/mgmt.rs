//! The Bluetooth management socket of the kernel (MGMT), for one command: Load Connection
//! Parameters, which sets the LE connection interval the kernel asks a device for. BlueZ
//! has no D-Bus API for it. The kernel accepts the command only on a socket that a process
//! with CAP_NET_ADMIN bound.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::time::Duration;

use bluer::{Address, AddressType};
use tokio::io::unix::AsyncFd;

const BTPROTO_HCI: libc::c_int = 1;
const HCI_DEV_NONE: u16 = 0xffff;
const HCI_CHANNEL_CONTROL: u16 = 3;
const LOAD_CONN_PARAM: u16 = 0x0035;
const COMMAND_COMPLETE: u16 = 0x0001;
const COMMAND_STATUS: u16 = 0x0002;
const PERMISSION_DENIED: u8 = 0x14;
const REPLY_TIMEOUT: Duration = Duration::from_secs(1);

/// The kernel defaults: 30 ms to 50 ms (units of 1.25 ms), no latency, 420 ms (units of 10 ms).
const MIN_INTERVAL: u16 = 24;
const MAX_INTERVAL: u16 = 40;
const LATENCY: u16 = 0;
const SUPERVISION_TIMEOUT: u16 = 42;

#[repr(C)]
struct SockaddrHci {
    family: libc::sa_family_t,
    dev: u16,
    channel: u16,
}

/// Opens and binds the control channel. The bind needs no capability, but only a bind with
/// CAP_NET_ADMIN gives a socket the kernel accepts commands from.
pub(crate) fn open() -> io::Result<OwnedFd> {
    let flags = libc::SOCK_RAW | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK;
    let fd = unsafe { libc::socket(libc::AF_BLUETOOTH, flags, BTPROTO_HCI) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let family = libc::AF_BLUETOOTH as libc::sa_family_t;
    let address = SockaddrHci { family, dev: HCI_DEV_NONE, channel: HCI_CHANNEL_CONTROL };
    let length = size_of::<SockaddrHci>() as libc::socklen_t;
    if unsafe { libc::bind(fd.as_raw_fd(), (&raw const address).cast(), length) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(fd)
}

pub(crate) enum Error {
    PermissionDenied,
    Other(String),
}

pub(crate) struct Mgmt {
    fd: AsyncFd<OwnedFd>,
    /// The controller index of the adapter, from its name "hciN".
    index: u16,
}

impl Mgmt {
    pub(crate) fn new(fd: OwnedFd, adapter: &str) -> Result<Mgmt, String> {
        let index = adapter.strip_prefix("hci").and_then(|n| n.parse().ok()).ok_or(format!("adapter {adapter}"))?;
        // Sound: the OwnedFd stays open and owned by the AsyncFd.
        let fd = unsafe { AsyncFd::register(fd) }.map_err(|e| e.to_string())?;
        Ok(Mgmt { fd, index })
    }

    /// Asks the kernel to use the fast interval for its next connection to `address`.
    pub(crate) async fn load_connection_parameters(
        &self, address: Address, address_type: AddressType,
    ) -> Result<(), Error> {
        let command = load_connection_parameters(self.index, address, address_type as u8);
        // The kernel answers a refused command with a Command Status and a write result of 0.
        if unsafe { libc::write(self.fd.as_raw_fd(), command.as_ptr().cast(), command.len()) } < 0 {
            return Err(Error::Other(io::Error::last_os_error().to_string()));
        }
        match tokio::time::timeout(REPLY_TIMEOUT, self.reply()).await {
            Ok(Ok(0)) => Ok(()),
            Ok(Ok(PERMISSION_DENIED)) => Err(Error::PermissionDenied),
            Ok(Ok(status)) => Err(Error::Other(format!("status 0x{status:02x}"))),
            Ok(Err(e)) => Err(Error::Other(e.to_string())),
            Err(_) => Err(Error::Other("no reply".into())),
        }
    }

    /// The status of the next Command Complete or Command Status for our command. Every
    /// MGMT event comes in on this socket; the others are skipped.
    async fn reply(&self) -> io::Result<u8> {
        // A longer event is cut off; the status is in the first 9 bytes.
        let mut event = [0u8; 64];
        loop {
            let mut guard = self.fd.readable().await?;
            let read = guard.try_io(|fd| {
                let n = unsafe { libc::read(fd.as_raw_fd(), event.as_mut_ptr().cast(), event.len()) };
                if n < 0 { Err(io::Error::last_os_error()) } else { Ok(n as usize) }
            });
            let Ok(read) = read else { continue };
            if let Some(status) = reply_status(&event[..read?]) {
                return Ok(status);
            }
        }
    }
}

/// Load Connection Parameters for one device, as the kernel reads it: a header of opcode,
/// controller index and parameter length, then the count and the entry, all little endian.
fn load_connection_parameters(index: u16, address: Address, address_type: u8) -> [u8; 23] {
    let mut command = [0u8; 23];
    let fields = [LOAD_CONN_PARAM, index, 17, 1];
    for (i, field) in fields.iter().enumerate() {
        command[2 * i..2 * i + 2].copy_from_slice(&field.to_le_bytes());
    }
    let mut bdaddr = address.0;
    bdaddr.reverse();
    command[8..14].copy_from_slice(&bdaddr);
    command[14] = address_type;
    let parameters = [MIN_INTERVAL, MAX_INTERVAL, LATENCY, SUPERVISION_TIMEOUT];
    for (i, parameter) in parameters.iter().enumerate() {
        command[15 + 2 * i..17 + 2 * i].copy_from_slice(&parameter.to_le_bytes());
    }
    command
}

/// The status byte of a Command Complete or Command Status event for Load Connection
/// Parameters; `None` for any other event.
fn reply_status(event: &[u8]) -> Option<u8> {
    let code = u16::from_le_bytes([*event.first()?, *event.get(1)?]);
    if code != COMMAND_COMPLETE && code != COMMAND_STATUS {
        return None;
    }
    let opcode = u16::from_le_bytes([*event.get(6)?, *event.get(7)?]);
    if opcode != LOAD_CONN_PARAM {
        return None;
    }
    event.get(8).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_connection_parameters_is_the_kernel_layout() {
        let kickr = Address([0xe5, 0x4f, 0x50, 0x22, 0x6b, 0xe3]);
        let command = load_connection_parameters(0, kickr, AddressType::LeRandom as u8);
        let expected = [
            0x35, 0x00, // Load Connection Parameters
            0x00, 0x00, // hci0
            0x11, 0x00, // 17 parameter bytes
            0x01, 0x00, // one entry
            0xe3, 0x6b, 0x22, 0x50, 0x4f, 0xe5, // the address, least significant byte first
            0x02, // LE random
            0x18, 0x00, // min 24 (30 ms)
            0x28, 0x00, // max 40 (50 ms)
            0x00, 0x00, // latency
            0x2a, 0x00, // timeout 42 (420 ms)
        ];
        assert_eq!(command, expected);
    }

    #[test]
    fn reply_status_reads_only_replies_to_our_command() {
        let complete = [0x01, 0x00, 0x00, 0x00, 0x03, 0x00, 0x35, 0x00, 0x14];
        assert_eq!(reply_status(&complete), Some(0x14));
        let status = [0x02, 0x00, 0x00, 0x00, 0x03, 0x00, 0x35, 0x00, 0x00];
        assert_eq!(reply_status(&status), Some(0x00));
        let other_command = [0x01, 0x00, 0x00, 0x00, 0x03, 0x00, 0x01, 0x00, 0x00];
        assert_eq!(reply_status(&other_command), None);
        let device_found = [0x12, 0x00, 0x00, 0x00, 0x0f, 0x00, 0x35, 0x00, 0x00];
        assert_eq!(reply_status(&device_found), None);
        assert_eq!(reply_status(&complete[..8]), None);
    }
}
