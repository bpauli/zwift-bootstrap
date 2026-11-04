//! The line protocol between shim/blewin10lib.c and the bridge, and the UUID and
//! address forms the game uses. Pure; see the protocol table in shim/blewin10lib.c.

use std::fmt::Write as _;

use bluer::{Address, Uuid};

/// The Bluetooth base UUID with a zero short id: 0000xxxx-0000-1000-8000-00805f9b34fb.
const BASE: u128 = 0x0000_0000_0000_1000_8000_0080_5f9b_34fb;

/// One service of the game's scan request, with the characteristics the game wants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ServiceSpec {
    pub uuid: Uuid,
    pub chars: Vec<Uuid>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Request {
    Scan(Vec<ServiceSpec>),
    Stop,
    Purge,
    Pair(Address),
    Unpair(Address),
    Write { address: Address, characteristic: Uuid, data: Vec<u8> },
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Event {
    Radio(bool),
    /// `services`: indexes into the scan request. `sections`: (AD type, data).
    Advertisement {
        address: Address,
        rssi: i16,
        services: Vec<usize>,
        sections: Vec<(u8, Vec<u8>)>,
        name: String,
    },
    /// `matched`: (service index, characteristic indexes) that the device has.
    Connected {
        address: Address,
        matched: Vec<(usize, Vec<usize>)>,
        name: String,
    },
    Value {
        address: Address,
        service: Uuid,
        characteristic: Uuid,
        data: Vec<u8>,
        name: String,
    },
    Disconnected {
        address: Address,
        unexpected: bool,
        name: String,
    },
    Log(String),
}

pub(crate) fn parse_request(line: &str) -> Result<Request, String> {
    let mut fields = line.split('\t');
    let command = fields.next().unwrap_or_default();
    let fields: Vec<&str> = fields.collect();
    let address = |i: usize| -> Result<Address, String> {
        let text = fields.get(i).ok_or("missing address")?;
        address_from_decimal(text).ok_or_else(|| format!("bad address {text:?}"))
    };
    match (command, fields.len()) {
        ("scan", _) => fields.iter().map(|f| parse_service(f)).collect::<Result<_, _>>().map(Request::Scan),
        ("stop", 0) => Ok(Request::Stop),
        ("purge", 0) => Ok(Request::Purge),
        ("pair", 1) => Ok(Request::Pair(address(0)?)),
        ("unpair", 1) => Ok(Request::Unpair(address(0)?)),
        ("write", 3) => Ok(Request::Write {
            address: address(0)?,
            characteristic: parse_uuid(fields[1]).ok_or_else(|| format!("bad uuid {:?}", fields[1]))?,
            data: hex_decode(fields[2]).ok_or("bad hex")?,
        }),
        _ => Err(format!("unknown request {line:?}")),
    }
}

/// `uuid=char,char,...` in the game's own strings.
fn parse_service(field: &str) -> Result<ServiceSpec, String> {
    let (service, chars) = field.split_once('=').ok_or_else(|| format!("bad service {field:?}"))?;
    let bad = |s: &str| format!("bad uuid {s:?}");
    Ok(ServiceSpec {
        uuid: parse_uuid(service).ok_or_else(|| bad(service))?,
        chars: chars
            .split(',')
            .filter(|c| !c.is_empty())
            .map(|c| parse_uuid(c).ok_or_else(|| bad(c)))
            .collect::<Result<_, _>>()?,
    })
}

/// The forms the game sends: "180D", "0x180D", 36 characters, or 36 characters in braces.
pub(crate) fn parse_uuid(text: &str) -> Option<Uuid> {
    let text = text.trim_start_matches('{').trim_end_matches('}');
    let short = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")).unwrap_or(text);
    if short.len() <= 8 && !short.contains('-') {
        return u32::from_str_radix(short, 16).ok().map(|id| Uuid::from_u128(BASE | (id as u128) << 96));
    }
    Uuid::parse_str(text).ok()
}

/// The service string in a value event, as the original DLL makes it: GUID in braces.
pub(crate) fn service_text(uuid: Uuid) -> String {
    format!("{{{uuid}}}")
}

/// The characteristic string in a value event, as the original DLL makes it: "0x" and the
/// short id without padding for base UUIDs ("0x2a37"), else the first 8 hex digits.
pub(crate) fn characteristic_text(uuid: Uuid) -> String {
    let value = uuid.as_u128();
    if value & !(0xffff_ffff << 96) == BASE {
        format!("0x{:x}", value >> 96)
    } else {
        format!("0x{}", &uuid.simple().to_string()[..8])
    }
}

pub(crate) fn address_from_decimal(text: &str) -> Option<Address> {
    let value: u64 = text.parse().ok().filter(|v| *v < 1 << 48)?;
    let bytes = value.to_be_bytes();
    Some(Address(bytes[2..].try_into().unwrap()))
}

pub(crate) fn address_to_decimal(address: Address) -> u64 {
    address.0.iter().fold(0, |acc, b| acc << 8 | u64::from(*b))
}

impl Event {
    pub(crate) fn to_line(&self) -> String {
        let clean = |name: &str| name.replace(['\t', '\n', '\r'], " ");
        let addr = |a: &Address| address_to_decimal(*a);
        let list = |items: &[usize], sep: &str| items.iter().map(usize::to_string).collect::<Vec<_>>().join(sep);
        let mut line = match self {
            Event::Radio(on) => format!("radio\t{}", u8::from(*on)),
            Event::Advertisement { address, rssi, services, sections, name } => {
                let sections: Vec<String> =
                    sections.iter().map(|(t, d)| format!("{t:02x}:{}", hex_encode(d))).collect();
                format!(
                    "adv\t{}\t{rssi}\t0\t{}\t{}\t{}",
                    addr(address),
                    list(services, ","),
                    sections.join(","),
                    clean(name)
                )
            }
            Event::Connected { address, matched, name } => {
                let matched: Vec<String> = matched.iter().map(|(s, c)| format!("{s}:{}", list(c, "."))).collect();
                format!("connected\t{}\t{}\t{}", addr(address), matched.join(";"), clean(name))
            }
            Event::Value { address, service, characteristic, data, name } => format!(
                "notify\t{}\t{}\t{}\t{}\t{}",
                addr(address),
                service_text(*service),
                characteristic_text(*characteristic),
                hex_encode(data),
                clean(name)
            ),
            Event::Disconnected { address, unexpected, name } => {
                format!("disconnected\t{}\t{}\t{}", addr(address), u8::from(*unexpected), clean(name))
            }
            Event::Log(text) => format!("log\t{}", clean(text)),
        };
        line.push('\n');
        line
    }
}

/// The AD sections the original DLL passes on, rebuilt from what BlueZ keeps of an
/// advertisement: the name, manufacturer data and 16-bit service data.
pub(crate) fn advertisement_sections(
    name: &str,
    manufacturer: &[(u16, Vec<u8>)],
    service_data: &[(Uuid, Vec<u8>)],
) -> Vec<(u8, Vec<u8>)> {
    let mut sections = vec![(0x09, name.as_bytes().to_vec())];
    for (company, data) in manufacturer {
        sections.push((0xff, [&company.to_le_bytes()[..], data].concat()));
    }
    for (uuid, data) in service_data {
        let value = uuid.as_u128();
        if value & !(0xffff << 96) == BASE {
            sections.push((0x16, [&((value >> 96) as u16).to_le_bytes()[..], data].concat()));
        }
    }
    sections
}

fn hex_encode(data: &[u8]) -> String {
    data.iter().fold(String::with_capacity(data.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

fn hex_decode(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const HR: &str = "0000180d-0000-1000-8000-00805f9b34fb";
    const WAHOO_CONTROL: &str = "a026e005-0a7d-4ab3-97fa-f1500f9feb8b";

    fn uuid(s: &str) -> Uuid {
        Uuid::parse_str(s).unwrap()
    }

    #[test]
    fn every_uuid_form_the_game_sends_parses_to_the_same_uuid() {
        for form in ["180D", "180d", "0x180D", "{0000180D-0000-1000-8000-00805F9B34FB}", HR] {
            assert_eq!(parse_uuid(form), Some(uuid(HR)), "{form}");
        }
        assert_eq!(parse_uuid("A026E005-0A7D-4AB3-97FA-F1500F9FEB8B"), Some(uuid(WAHOO_CONTROL)));
        assert_eq!(parse_uuid("0000"), Some(uuid("00000000-0000-1000-8000-00805f9b34fb")));
        assert_eq!(parse_uuid("xyz"), None);
    }

    #[test]
    fn value_event_strings_match_the_original_dll() {
        assert_eq!(characteristic_text(uuid("00002a37-0000-1000-8000-00805f9b34fb")), "0x2a37");
        assert_eq!(characteristic_text(uuid(WAHOO_CONTROL)), "0xa026e005");
        assert_eq!(service_text(uuid(HR)), "{0000180d-0000-1000-8000-00805f9b34fb}");
    }

    #[test]
    fn addresses_round_trip_through_the_decimal_form() {
        let kickr = Address([0xe5, 0x4f, 0x50, 0x22, 0x6b, 0xe3]);
        assert_eq!(address_to_decimal(kickr), 0xe54f_5022_6be3);
        assert_eq!(address_from_decimal("252128809610211"), Some(kickr));
        assert_eq!(address_from_decimal(&(1u64 << 48).to_string()), None);
        assert_eq!(address_from_decimal("-1"), None);
    }

    #[test]
    fn requests_parse_from_the_dll_lines() {
        assert_eq!(
            parse_request("scan\t180D=2A37\t1818=2A63,A026E005-0A7D-4AB3-97FA-F1500F9FEB8B\tFE1F="),
            Ok(Request::Scan(vec![
                ServiceSpec { uuid: uuid(HR), chars: vec![parse_uuid("2A37").unwrap()] },
                ServiceSpec {
                    uuid: parse_uuid("1818").unwrap(),
                    chars: vec![parse_uuid("2A63").unwrap(), uuid(WAHOO_CONTROL)]
                },
                ServiceSpec { uuid: parse_uuid("FE1F").unwrap(), chars: vec![] },
            ]))
        );
        assert_eq!(parse_request("scan"), Ok(Request::Scan(vec![])));
        let kickr = Address([0xe5, 0x4f, 0x50, 0x22, 0x6b, 0xe3]);
        assert_eq!(parse_request("pair\t252128809610211"), Ok(Request::Pair(kickr)));
        assert_eq!(
            parse_request("write\t252128809610211\tA026E005-0A7D-4AB3-97FA-F1500F9FEB8B\t462a80"),
            Ok(Request::Write { address: kickr, characteristic: uuid(WAHOO_CONTROL), data: vec![0x46, 0x2a, 0x80] })
        );
        assert!(parse_request("write\t1\t2A37\t4").is_err(), "odd hex");
        assert!(parse_request("pair").is_err());
        assert!(parse_request("dance").is_err());
    }

    #[test]
    fn events_render_as_the_dll_reads_them() {
        let kickr = Address([0xe5, 0x4f, 0x50, 0x22, 0x6b, 0xe3]);
        let name = "Wahoo KICKR 7D9D".to_owned();
        assert_eq!(Event::Radio(true).to_line(), "radio\t1\n");
        assert_eq!(
            Event::Advertisement {
                address: kickr,
                rssi: -88,
                services: vec![0, 17],
                sections: vec![(0xff, vec![0x01, 0x02])],
                name: name.clone()
            }
            .to_line(),
            "adv\t252128809610211\t-88\t0\t0,17\tff:0102\tWahoo KICKR 7D9D\n"
        );
        assert_eq!(
            Event::Connected { address: kickr, matched: vec![(0, vec![0, 1]), (33, vec![])], name: name.clone() }
                .to_line(),
            "connected\t252128809610211\t0:0.1;33:\tWahoo KICKR 7D9D\n"
        );
        assert_eq!(
            Event::Value {
                address: kickr,
                service: parse_uuid("1818").unwrap(),
                characteristic: parse_uuid("2A63").unwrap(),
                data: vec![0, 0x2a],
                name: name.clone()
            }
            .to_line(),
            "notify\t252128809610211\t{00001818-0000-1000-8000-00805f9b34fb}\t0x2a63\t002a\tWahoo KICKR 7D9D\n"
        );
        assert_eq!(
            Event::Disconnected { address: kickr, unexpected: true, name: "a\tb".into() }.to_line(),
            "disconnected\t252128809610211\t1\ta b\n"
        );
    }

    #[test]
    fn advertisement_sections_rebuild_the_raw_ad_layout() {
        let sections = advertisement_sections(
            "HR",
            &[(0x0001, vec![0xaa])],
            &[(parse_uuid("FE1F").unwrap(), vec![0xbb]), (uuid(WAHOO_CONTROL), vec![0xcc])],
        );
        assert_eq!(
            sections,
            vec![(0x09, b"HR".to_vec()), (0xff, vec![0x01, 0x00, 0xaa]), (0x16, vec![0x1f, 0xfe, 0xbb])]
        );
    }
}
