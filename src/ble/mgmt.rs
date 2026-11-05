//! BlueZ has no D-Bus API for Load Connection Parameters. This MGMT command sets the LE
//! connection interval that the kernel asks a device for.

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

const fn units_of_1250_us(ms: u16) -> u16 {
    ms * 4 / 5
}

const KERNEL_DEFAULT_MIN_INTERVAL: u16 = units_of_1250_us(30);
const KERNEL_DEFAULT_MAX_INTERVAL: u16 = units_of_1250_us(50);
const KERNEL_DEFAULT_LATENCY: u16 = 0;
const KERNEL_DEFAULT_SUPERVISION_TIMEOUT_10_MS: u16 = 420 / 10;

#[repr(C)]
struct SockaddrHci {
    family: libc::sa_family_t,
    dev: u16,
    channel: u16,
}

/// The bind needs no capability. But the kernel accepts commands only on a socket that a
/// process with CAP_NET_ADMIN bound.
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

const LINUX_CAPABILITY_VERSION_3: u32 = 0x2008_0522;

#[repr(C)]
struct CapHeader {
    version: u32,
    pid: i32,
}

#[repr(C)]
struct CapData {
    effective: u32,
    permitted: u32,
    inheritable: u32,
}

pub(crate) fn drop_capabilities() -> io::Result<()> {
    let header = CapHeader { version: LINUX_CAPABILITY_VERSION_3, pid: 0 };
    let none = || CapData { effective: 0, permitted: 0, inheritable: 0 };
    let data = [none(), none()];
    if unsafe { libc::syscall(libc::SYS_capset, &raw const header, data.as_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(crate) enum Error {
    PermissionDenied,
    Other(String),
}

pub(crate) struct Mgmt {
    fd: AsyncFd<OwnedFd>,
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

    async fn reply(&self) -> io::Result<u8> {
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

fn load_connection_parameters(index: u16, address: Address, address_type: u8) -> Vec<u8> {
    let mut entry: Vec<u8> = address.0.iter().rev().copied().collect();
    entry.push(address_type);
    let parameters = [
        KERNEL_DEFAULT_MIN_INTERVAL,
        KERNEL_DEFAULT_MAX_INTERVAL,
        KERNEL_DEFAULT_LATENCY,
        KERNEL_DEFAULT_SUPERVISION_TIMEOUT_10_MS,
    ];
    entry.extend(parameters.iter().flat_map(|parameter| parameter.to_le_bytes()));
    let entry_count: u16 = 1;
    let parameter_length = (size_of::<u16>() + entry.len()) as u16;
    let header = [LOAD_CONN_PARAM, index, parameter_length, entry_count];
    header.iter().flat_map(|field| field.to_le_bytes()).chain(entry).collect()
}

fn reply_status(event: &[u8]) -> Option<u8> {
    let &[code_low, code_high, _index_low, _index_high, _length_low, _length_high, opcode_low, opcode_high, status, ..] =
        event
    else {
        return None;
    };
    let code = u16::from_le_bytes([code_low, code_high]);
    let opcode = u16::from_le_bytes([opcode_low, opcode_high]);
    let is_reply = code == COMMAND_COMPLETE || code == COMMAND_STATUS;
    (is_reply && opcode == LOAD_CONN_PARAM).then_some(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_connection_parameters_is_the_kernel_layout() {
        let kickr = Address([0xe5, 0x4f, 0x50, 0x22, 0x6b, 0xe3]);
        let command = load_connection_parameters(0, kickr, AddressType::LeRandom as u8);
        let opcode = [0x35, 0x00];
        let hci0 = [0x00, 0x00];
        let parameter_length_17 = [0x11, 0x00];
        let one_entry = [0x01, 0x00];
        let address_least_significant_byte_first = [0xe3, 0x6b, 0x22, 0x50, 0x4f, 0xe5];
        let le_random = [0x02];
        let min_interval_30_ms = [0x18, 0x00];
        let max_interval_50_ms = [0x28, 0x00];
        let no_latency = [0x00, 0x00];
        let supervision_timeout_420_ms = [0x2a, 0x00];
        let expected = [
            &opcode[..],
            &hci0,
            &parameter_length_17,
            &one_entry,
            &address_least_significant_byte_first,
            &le_random,
            &min_interval_30_ms,
            &max_interval_50_ms,
            &no_latency,
            &supervision_timeout_420_ms,
        ]
        .concat();
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
