//! Wire format for `mullvad/win-split-tunnel` (GPL-3.0).
//!
//! Nimbus does not write its own kernel driver: Microsoft only loads drivers it
//! has signed, and that needs an EV certificate. Mullvad's driver is already
//! signed and is GPL-3.0, so Nimbus drives it instead. This module is the
//! protocol half — pure encoding and decoding, no Windows APIs — so it can be
//! tested on any platform.
//!
//! Everything here mirrors the headers at
//! `mullvad/win-split-tunnel:src/defs/*.h`. The layout is x64: `HANDLE` and
//! `SIZE_T` are 8 bytes, `USHORT` is 2, `WCHAR` is 2, `BOOLEAN` is 1.
//!
//! # Contract worth knowing
//!
//! The driver implements **exclude** mode: listed images are pushed *out* of the
//! tunnel. That is the opposite of Nimbus's Linux feature, so the UI must present
//! it as "exclude these apps from the VPN", not "VPN only for these apps".
//!
//! It also needs a user-mode agent (this crate) to tell it which images to
//! exclude and which addresses belong to the tunnel and to the physical
//! adapter, and to keep that information current. Eddie stays in its default
//! `in` mode: everything is tunneled, and the listed apps are pulled out.

/// Symbolic link the driver creates, opened from user mode.
pub const DEVICE_PATH: &str = r"\\.\MULLVADSPLITTUNNEL";

const DEVICE_TYPE: u32 = 0x8000;
const FILE_ANY_ACCESS: u32 = 0;
const METHOD_BUFFERED: u32 = 0;
const METHOD_NEITHER: u32 = 3;

const fn ctl_code(function: u32, method: u32) -> u32 {
    (DEVICE_TYPE << 16) | (FILE_ANY_ACCESS << 14) | (function << 2) | method
}

pub const IOCTL_ST_INITIALIZE: u32 = ctl_code(1, METHOD_BUFFERED);
pub const IOCTL_ST_DEQUEUE_EVENT: u32 = ctl_code(2, METHOD_BUFFERED);
pub const IOCTL_ST_REGISTER_PROCESSES: u32 = ctl_code(3, METHOD_BUFFERED);
pub const IOCTL_ST_REGISTER_IP_ADDRESSES: u32 = ctl_code(4, METHOD_BUFFERED);
pub const IOCTL_ST_GET_IP_ADDRESSES: u32 = ctl_code(5, METHOD_BUFFERED);
pub const IOCTL_ST_SET_CONFIGURATION: u32 = ctl_code(6, METHOD_BUFFERED);
pub const IOCTL_ST_GET_CONFIGURATION: u32 = ctl_code(7, METHOD_BUFFERED);
pub const IOCTL_ST_CLEAR_CONFIGURATION: u32 = ctl_code(8, METHOD_NEITHER);
pub const IOCTL_ST_GET_STATE: u32 = ctl_code(9, METHOD_BUFFERED);
pub const IOCTL_ST_QUERY_PROCESS: u32 = ctl_code(10, METHOD_BUFFERED);
pub const IOCTL_ST_RESET: u32 = ctl_code(11, METHOD_NEITHER);

/// Driver lifecycle, from `defs/state.h`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverState {
    None,
    Started,
    Initialized,
    Ready,
    Engaged,
    Zombie,
    Unknown(u32),
}

impl DriverState {
    pub fn from_raw(value: u32) -> Self {
        match value {
            0 => Self::None,
            1 => Self::Started,
            2 => Self::Initialized,
            3 => Self::Ready,
            4 => Self::Engaged,
            5 => Self::Zombie,
            other => Self::Unknown(other),
        }
    }
}

/// `ST_IP_ADDRESSES`: the addresses the driver needs to tell the two paths
/// apart. `None` means "this family is unavailable on that side".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IpAddresses {
    pub tunnel_v4: Option<[u8; 4]>,
    pub internet_v4: Option<[u8; 4]>,
    pub tunnel_v6: Option<[u8; 16]>,
    pub internet_v6: Option<[u8; 16]>,
}

impl IpAddresses {
    /// 4 + 4 + 16 + 16 bytes, in the order the driver expects.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(40);
        out.extend_from_slice(&self.tunnel_v4.unwrap_or([0; 4]));
        out.extend_from_slice(&self.internet_v4.unwrap_or([0; 4]));
        out.extend_from_slice(&self.tunnel_v6.unwrap_or([0; 16]));
        out.extend_from_slice(&self.internet_v6.unwrap_or([0; 16]));
        out
    }
}

/// `ST_SUBLAYER_GUIDS`: two GUIDs the driver registers its WFP filters under,
/// supplied by the agent so it can find and undo its own filters.
#[derive(Debug, Clone, Copy)]
pub struct SublayerGuids {
    pub baseline: [u8; 16],
    pub dns: [u8; 16],
}

impl SublayerGuids {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(32);
        out.extend_from_slice(&self.baseline);
        out.extend_from_slice(&self.dns);
        out
    }
}

/// Sizes of the variable-length structures. They are spelled out rather than
/// derived with `repr(C)` so the wire format is explicit and testable.
const CONFIG_HEADER_LEN: usize = 16;
const CONFIG_ENTRY_LEN: usize = 16;
const PROCESS_HEADER_LEN: usize = 16;
const PROCESS_ENTRY_LEN: usize = 32;
const EVENT_HEADER_LEN: usize = 16;

/// Build an `ST_CONFIGURATION_HEADER` block listing image device paths to
/// exclude, e.g. `\Device\HarddiskVolume3\Program Files\Helium\helium.exe`.
///
/// Offsets in the entries are relative to the string blob that follows the
/// entries, which is how the header describes it.
pub fn configuration(image_paths: &[String]) -> Vec<u8> {
    let mut strings: Vec<u8> = Vec::new();
    let mut entries: Vec<(u64, u16)> = Vec::with_capacity(image_paths.len());

    for path in image_paths {
        let utf16: Vec<u16> = path.encode_utf16().collect();
        let bytes = utf16.len() * 2;
        let len = u16::try_from(bytes).unwrap_or(u16::MAX);
        entries.push((strings.len() as u64, len));
        for unit in utf16 {
            strings.extend_from_slice(&unit.to_le_bytes());
        }
    }

    let total = CONFIG_HEADER_LEN + CONFIG_ENTRY_LEN * entries.len() + strings.len();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&(entries.len() as u64).to_le_bytes());
    out.extend_from_slice(&(total as u64).to_le_bytes());
    for (offset, len) in entries {
        out.extend_from_slice(&offset.to_le_bytes());
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&[0u8; 6]); // padding to the 8-byte alignment
    }
    out.extend_from_slice(&strings);
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessEntry {
    pub pid: u64,
    pub parent_pid: u64,
    /// Image device path, as Windows reports it.
    pub image: String,
}

/// Build an `ST_PROCESS_DISCOVERY_HEADER` block: the full process tree at the
/// moment the agent starts, so the driver can classify existing processes.
pub fn processes(existing: &[ProcessEntry]) -> Vec<u8> {
    let mut strings: Vec<u8> = Vec::new();
    let mut entries: Vec<(u64, u64, u64, u16)> = Vec::with_capacity(existing.len());

    for process in existing {
        let utf16: Vec<u16> = process.image.encode_utf16().collect();
        let bytes = utf16.len() * 2;
        entries.push((
            process.pid,
            process.parent_pid,
            strings.len() as u64,
            u16::try_from(bytes).unwrap_or(u16::MAX),
        ));
        for unit in utf16 {
            strings.extend_from_slice(&unit.to_le_bytes());
        }
    }

    let total = PROCESS_HEADER_LEN + PROCESS_ENTRY_LEN * entries.len() + strings.len();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&(entries.len() as u64).to_le_bytes());
    out.extend_from_slice(&(total as u64).to_le_bytes());
    for (pid, parent, offset, len) in entries {
        out.extend_from_slice(&pid.to_le_bytes());
        out.extend_from_slice(&parent.to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&[0u8; 6]);
    }
    out.extend_from_slice(&strings);
    out
}

/// Something the driver reported through `IOCTL_ST_DEQUEUE_EVENT`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    StartSplitting {
        pid: u64,
        reason: u32,
        image: String,
    },
    StopSplitting {
        pid: u64,
        reason: u32,
        image: String,
    },
    ErrorStartSplitting {
        pid: u64,
        image: String,
    },
    ErrorStopSplitting {
        pid: u64,
        image: String,
    },
    ErrorMessage {
        status: u32,
        message: String,
    },
    Unknown(u32),
}

const EVENT_START_SPLITTING: u32 = 0;
const EVENT_STOP_SPLITTING: u32 = 1;
const EVENT_ERROR_FLAG: u32 = 0x8000_0000;
const EVENT_ERROR_START: u32 = EVENT_ERROR_FLAG;
const EVENT_ERROR_STOP: u32 = EVENT_ERROR_FLAG + 1;
const EVENT_ERROR_MESSAGE: u32 = EVENT_ERROR_FLAG + 2;

/// Decode an `ST_EVENT_HEADER` and its payload. Malformed buffers yield `None`
/// instead of panicking: this parses data produced by a kernel driver, and a
/// wrong length must not take the agent down.
pub fn parse_event(buffer: &[u8]) -> Option<Event> {
    if buffer.len() < EVENT_HEADER_LEN {
        return None;
    }
    let id = u32::from_le_bytes(buffer[0..4].try_into().ok()?);
    let size = u64::from_le_bytes(buffer[8..16].try_into().ok()?) as usize;
    let payload = buffer.get(EVENT_HEADER_LEN..EVENT_HEADER_LEN + size)?;

    match id {
        EVENT_START_SPLITTING | EVENT_STOP_SPLITTING => {
            // HANDLE pid; reason; USHORT length; WCHAR image[]
            if payload.len() < 14 {
                return None;
            }
            let pid = u64::from_le_bytes(payload[0..8].try_into().ok()?);
            let reason = u32::from_le_bytes(payload[8..12].try_into().ok()?);
            let len = u16::from_le_bytes(payload[12..14].try_into().ok()?) as usize;
            let image = utf16_le(payload.get(14..14 + len)?);
            if id == EVENT_START_SPLITTING {
                Some(Event::StartSplitting { pid, reason, image })
            } else {
                Some(Event::StopSplitting { pid, reason, image })
            }
        }
        EVENT_ERROR_START | EVENT_ERROR_STOP => {
            if payload.len() < 10 {
                return None;
            }
            let pid = u64::from_le_bytes(payload[0..8].try_into().ok()?);
            let len = u16::from_le_bytes(payload[8..10].try_into().ok()?) as usize;
            let image = utf16_le(payload.get(10..10 + len)?);
            if id == EVENT_ERROR_START {
                Some(Event::ErrorStartSplitting { pid, image })
            } else {
                Some(Event::ErrorStopSplitting { pid, image })
            }
        }
        EVENT_ERROR_MESSAGE => {
            if payload.len() < 6 {
                return None;
            }
            let status = u32::from_le_bytes(payload[0..4].try_into().ok()?);
            let len = u16::from_le_bytes(payload[4..6].try_into().ok()?) as usize;
            let message = utf16_le(payload.get(6..6 + len)?);
            Some(Event::ErrorMessage { status, message })
        }
        other => Some(Event::Unknown(other)),
    }
}

fn utf16_le(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ioctl_codes_match_the_header_macros() {
        assert_eq!(IOCTL_ST_INITIALIZE, 0x8000_0004);
        assert_eq!(IOCTL_ST_DEQUEUE_EVENT, 0x8000_0008);
        assert_eq!(IOCTL_ST_REGISTER_PROCESSES, 0x8000_000C);
        assert_eq!(IOCTL_ST_REGISTER_IP_ADDRESSES, 0x8000_0010);
        assert_eq!(IOCTL_ST_SET_CONFIGURATION, 0x8000_0018);
        assert_eq!(IOCTL_ST_CLEAR_CONFIGURATION, 0x8000_0023);
        assert_eq!(IOCTL_ST_QUERY_PROCESS, 0x8000_0028);
        assert_eq!(IOCTL_ST_RESET, 0x8000_002F);
    }

    #[test]
    fn configuration_layout() {
        let paths = vec![r"\Device\HarddiskVolume3\A\a.exe".to_string(), r"\B\b.exe".to_string()];
        let encoded = configuration(&paths);

        let count = u64::from_le_bytes(encoded[0..8].try_into().unwrap());
        let total = u64::from_le_bytes(encoded[8..16].try_into().unwrap());
        assert_eq!(count, 2);
        assert_eq!(total as usize, encoded.len());

        // Entries are 16 bytes each and start right after the 16-byte header.
        let first_offset = u64::from_le_bytes(encoded[16..24].try_into().unwrap());
        let first_len = u16::from_le_bytes(encoded[24..26].try_into().unwrap());
        assert_eq!(first_offset, 0, "offsets are relative to the string blob");
        assert_eq!(first_len as usize, paths[0].encode_utf16().count() * 2);

        let second_offset = u64::from_le_bytes(encoded[32..40].try_into().unwrap());
        assert_eq!(second_offset as usize, first_len as usize);

        // The blob follows both entries and holds UTF-16LE without a terminator.
        let blob = &encoded[16 + 32..];
        assert_eq!(&blob[..first_len as usize], &paths[0].encode_utf16().flat_map(|u| u.to_le_bytes()).collect::<Vec<_>>()[..]);
    }

    #[test]
    fn empty_configuration_is_just_the_header() {
        let encoded = configuration(&[]);
        assert_eq!(encoded.len(), 16);
        assert_eq!(u64::from_le_bytes(encoded[0..8].try_into().unwrap()), 0);
        assert_eq!(u64::from_le_bytes(encoded[8..16].try_into().unwrap()), 16);
    }

    #[test]
    fn process_layout() {
        let tree = vec![
            ProcessEntry { pid: 4, parent_pid: 0, image: r"\SystemRoot\system32\a.exe".into() },
            ProcessEntry { pid: 100, parent_pid: 4, image: r"\B\b.exe".into() },
        ];
        let encoded = processes(&tree);
        assert_eq!(u64::from_le_bytes(encoded[0..8].try_into().unwrap()), 2);
        assert_eq!(encoded.len(), 16 + 32 * 2 + tree[0].image.encode_utf16().count() * 2 + tree[1].image.encode_utf16().count() * 2);

        // pid, parent pid, then the offset into the blob.
        assert_eq!(u64::from_le_bytes(encoded[16..24].try_into().unwrap()), 4);
        assert_eq!(u64::from_le_bytes(encoded[24..32].try_into().unwrap()), 0);
        assert_eq!(u64::from_le_bytes(encoded[32..40].try_into().unwrap()), 0);
    }

    #[test]
    fn ip_addresses_are_fixed_size() {
        let addresses = IpAddresses {
            tunnel_v4: Some([10, 8, 0, 2]),
            internet_v4: Some([192, 168, 1, 10]),
            tunnel_v6: None,
            internet_v6: None,
        };
        let encoded = addresses.encode();
        assert_eq!(encoded.len(), 40);
        assert_eq!(&encoded[0..4], &[10, 8, 0, 2]);
        assert_eq!(&encoded[4..8], &[192, 168, 1, 10]);
        assert_eq!(&encoded[8..24], &[0u8; 16]);
    }

    #[test]
    fn parses_a_start_splitting_event() {
        let image = "\\Device\\a.exe";
        let utf16: Vec<u8> = image.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();

        let mut payload = Vec::new();
        payload.extend_from_slice(&42u64.to_le_bytes());
        payload.extend_from_slice(&4u32.to_le_bytes());
        payload.extend_from_slice(&(utf16.len() as u16).to_le_bytes());
        payload.extend_from_slice(&utf16);

        let mut buffer = Vec::new();
        buffer.extend_from_slice(&EVENT_START_SPLITTING.to_le_bytes());
        buffer.extend_from_slice(&[0u8; 4]);
        buffer.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        buffer.extend_from_slice(&payload);

        assert_eq!(
            parse_event(&buffer),
            Some(Event::StartSplitting { pid: 42, reason: 4, image: image.into() })
        );
    }

    #[test]
    fn parses_an_error_message_event() {
        let message = "boom";
        let utf16: Vec<u8> = message.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();

        let mut payload = Vec::new();
        payload.extend_from_slice(&0xC000_0001u32.to_le_bytes());
        payload.extend_from_slice(&(utf16.len() as u16).to_le_bytes());
        payload.extend_from_slice(&utf16);

        let mut buffer = Vec::new();
        buffer.extend_from_slice(&EVENT_ERROR_MESSAGE.to_le_bytes());
        buffer.extend_from_slice(&[0u8; 4]);
        buffer.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        buffer.extend_from_slice(&payload);

        assert_eq!(
            parse_event(&buffer),
            Some(Event::ErrorMessage { status: 0xC000_0001, message: message.into() })
        );
    }

    #[test]
    fn rejects_truncated_events_instead_of_panicking() {
        assert_eq!(parse_event(&[]), None);
        assert_eq!(parse_event(&[0u8; 8]), None);
        // Claims a payload larger than the buffer.
        let mut buffer = vec![0u8; 16];
        buffer[8..16].copy_from_slice(&999u64.to_le_bytes());
        assert_eq!(parse_event(&buffer), None);
    }

    #[test]
    fn driver_states_map() {
        assert_eq!(DriverState::from_raw(3), DriverState::Ready);
        assert_eq!(DriverState::from_raw(4), DriverState::Engaged);
        assert_eq!(DriverState::from_raw(99), DriverState::Unknown(99));
    }
}
