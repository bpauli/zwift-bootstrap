//! BlueZ side of the BLE bridge: serves one game connection at a time on an AF_UNIX
//! socket and does what its requests ask with bluer.

use std::collections::HashMap;
use std::os::fd::OwnedFd;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bluer::gatt::WriteOp;
use bluer::gatt::remote::{Characteristic, CharacteristicWriteRequest};
use bluer::{Adapter, AdapterEvent, AdapterProperty, Address, Device, DeviceEvent, DeviceProperty, Uuid};
use futures_util::StreamExt;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio::task::{JoinHandle, JoinSet};

use super::mgmt::{self, Mgmt};
use super::protocol::{Event, Request, ServiceSpec, advertisement_sections, parse_request};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// BlueZ discovers all services before it reports them. A sensor that asks for a long
/// connection interval (an HRM-Pro Plus asks for 240 ms) needs more than 10 s for that;
/// the game itself gives up on a pair after about 50 s.
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(45);

/// Runs while process `parent` is this process's parent: `launch` passes its own PID,
/// which Wine keeps after the exec, so the bridge ends with the game.
pub fn serve(socket: &Path, parent: u32) -> Result<(), String> {
    let mgmt = match mgmt::open() {
        Ok(fd) => Some(fd),
        Err(e) => {
            let fallback = "so connections use the preferred interval of the sensor";
            eprintln!("ble-bridge: cannot open the Bluetooth management socket ({e}), {fallback}");
            None
        }
    };
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string())?;
    runtime.block_on(run(socket, parent, mgmt))
}

async fn run(socket: &Path, parent: u32, mgmt: Option<OwnedFd>) -> Result<(), String> {
    if std::os::unix::process::parent_id() != parent {
        return Ok(());
    }
    if UnixStream::connect(socket).await.is_ok() {
        return Err(format!("another bridge serves {}", socket.display()));
    }
    let _ = std::fs::remove_file(socket);
    let listener = UnixListener::bind(socket).map_err(|e| format!("{}: {e}", socket.display()))?;
    let adapter = match default_adapter().await {
        Ok(adapter) => Some(adapter),
        Err(e) => {
            eprintln!("ble-bridge: no Bluetooth adapter: {e}");
            None
        }
    };
    let mgmt = match (mgmt, &adapter) {
        (Some(fd), Some(adapter)) => match Mgmt::new(fd, adapter.name()) {
            Ok(mgmt) => Some(mgmt),
            Err(e) => {
                eprintln!("ble-bridge: Bluetooth management socket: {e}");
                None
            }
        },
        _ => None,
    };
    let mgmt = Arc::new(tokio::sync::Mutex::new(mgmt));
    let parent_gone = async {
        while std::os::unix::process::parent_id() == parent {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    };
    tokio::pin!(parent_gone);
    loop {
        tokio::select! {
            () = &mut parent_gone => break,
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { continue };
                tokio::select! {
                    () = &mut parent_gone => break,
                    () = serve_game(stream, adapter.clone(), mgmt.clone()) => {}
                }
            }
        }
    }
    let _ = std::fs::remove_file(socket);
    Ok(())
}

async fn default_adapter() -> Result<Adapter, String> {
    let connect = async { bluer::Session::new().await?.default_adapter().await };
    match tokio::time::timeout(Duration::from_secs(5), connect).await {
        Ok(adapter) => adapter.map_err(|e| e.to_string()),
        Err(_) => Err("BlueZ did not answer".into()),
    }
}

/// What the bridge knows while one game is connected. Locked only between awaits.
#[derive(Default)]
struct State {
    request: Vec<ServiceSpec>,
    names: HashMap<Address, String>,
    links: HashMap<Address, Link>,
}

/// A device the game asked to pair, from the connect attempt until the game unpairs it or
/// the connection ends. Dropping it aborts the connect attempt and every later task, so a
/// device the game unpaired never reports a late connect.
struct Link {
    device: Device,
    name: String,
    /// Characteristics found at connect time, for writes.
    chars: Vec<(Uuid, Characteristic, WriteOp)>,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for Link {
    fn drop(&mut self) {
        self.tasks.iter().for_each(JoinHandle::abort);
    }
}

#[derive(Clone)]
struct Bridge {
    adapter: Adapter,
    state: Arc<Mutex<State>>,
    events: UnboundedSender<Event>,
    /// None without CAP_NET_ADMIN: connections then use the interval the sensor prefers.
    mgmt: Arc<tokio::sync::Mutex<Option<Mgmt>>>,
}

async fn serve_game(stream: UnixStream, adapter: Option<Adapter>, mgmt: Arc<tokio::sync::Mutex<Option<Mgmt>>>) {
    let (read, mut write) = stream.into_split();
    let (events, mut queue) = unbounded_channel::<Event>();
    let writer = tokio::spawn(async move {
        while let Some(event) = queue.recv().await {
            if write.write_all(event.to_line().as_bytes()).await.is_err() {
                break;
            }
        }
    });
    let Some(adapter) = adapter else {
        let _ = events.send(Event::Log("Bluetooth helper: no Bluetooth adapter".into()));
        let _ = events.send(Event::Radio(false));
        drain(read).await;
        writer.abort();
        return;
    };
    let bridge = Bridge { adapter, state: Arc::default(), events, mgmt };
    let radio = tokio::spawn(bridge.clone().watch_radio());
    let mut scan: Option<JoinHandle<()>> = None;
    let mut lines = BufReader::new(read).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        match parse_request(&line) {
            Ok(Request::Scan(specs)) => {
                bridge.state.lock().unwrap().request = specs;
                if scan.as_ref().is_none_or(JoinHandle::is_finished) {
                    scan = Some(tokio::spawn(bridge.clone().scan()));
                }
            }
            Ok(Request::Stop) => scan.take().iter().for_each(JoinHandle::abort),
            Ok(Request::Purge) => {
                let mut state = bridge.state.lock().unwrap();
                let State { names, links, .. } = &mut *state;
                names.retain(|address, _| links.contains_key(address));
            }
            Ok(Request::Pair(address)) => bridge.pair(address),
            Ok(Request::Unpair(address)) => bridge.unpair(address).await,
            Ok(Request::Write { address, characteristic, data }) => {
                drop(tokio::spawn(bridge.clone().write(address, characteristic, data)));
            }
            Err(e) => bridge.log(format!("Bluetooth helper: {e}")),
        }
    }
    scan.iter().for_each(JoinHandle::abort);
    radio.abort();
    let links: Vec<Link> = bridge.state.lock().unwrap().links.drain().map(|(_, link)| link).collect();
    for link in links {
        let _ = link.device.disconnect().await;
    }
    writer.abort();
}

async fn drain(read: tokio::net::unix::OwnedReadHalf) {
    let mut lines = BufReader::new(read).lines();
    while let Ok(Some(_)) = lines.next_line().await {}
}

impl Bridge {
    fn send(&self, event: Event) {
        let _ = self.events.send(event);
    }

    fn log(&self, text: String) {
        eprintln!("ble-bridge: {text}");
        self.send(Event::Log(text));
    }

    async fn watch_radio(self) {
        self.send(Event::Radio(self.adapter.is_powered().await.unwrap_or(false)));
        let Ok(events) = self.adapter.events().await else {
            return;
        };
        tokio::pin!(events);
        while let Some(event) = events.next().await {
            if let AdapterEvent::PropertyChanged(AdapterProperty::Powered(on)) = event {
                self.send(Event::Radio(on));
            }
        }
    }

    /// Discovery runs while this task lives; aborting it drops the stream and BlueZ stops.
    async fn scan(self) {
        let filter = bluer::DiscoveryFilter {
            transport: bluer::DiscoveryTransport::Le,
            duplicate_data: true,
            ..Default::default()
        };
        if let Err(e) = self.adapter.set_discovery_filter(filter).await {
            self.log(format!("Bluetooth helper: discovery filter: {e}"));
        }
        let discovery = match self.adapter.discover_devices_with_changes().await {
            Ok(discovery) => discovery,
            Err(e) => return self.log(format!("Bluetooth helper: cannot scan: {e}")),
        };
        tokio::pin!(discovery);
        // BlueZ removes a device it has not seen for a while and adds it again on its next
        // advertisement: the watcher of the removed device gets no more events.
        let mut watched = HashMap::new();
        let mut watchers = JoinSet::new();
        while let Some(event) = discovery.next().await {
            match event {
                AdapterEvent::DeviceAdded(address) if !watched.contains_key(&address) => {
                    watched.insert(address, watchers.spawn(self.clone().watch_advertisements(address)));
                }
                AdapterEvent::DeviceRemoved(address) => {
                    if let Some(watcher) = watched.remove(&address) {
                        watcher.abort();
                    }
                }
                _ => {}
            }
        }
    }

    /// Reports the device now and again on every advertisement BlueZ passes on.
    async fn watch_advertisements(self, address: Address) {
        let Ok(device) = self.adapter.device(address) else {
            return;
        };
        self.report(&device).await;
        let Ok(events) = device.events().await else {
            return;
        };
        tokio::pin!(events);
        while let Some(DeviceEvent::PropertyChanged(property)) = events.next().await {
            if matches!(
                property,
                DeviceProperty::Rssi(_)
                    | DeviceProperty::Name(_)
                    | DeviceProperty::Uuids(_)
                    | DeviceProperty::ManufacturerData(_)
                    | DeviceProperty::ServiceData(_)
            ) {
                self.report(&device).await;
            }
        }
    }

    /// Like the original DLL: only named devices that advertise a requested service,
    /// never one that is connecting or connected.
    async fn report(&self, device: &Device) {
        let address = device.address();
        // No RSSI: BlueZ knows the device from its cache, but it did not advertise now.
        let Ok(Some(rssi)) = device.rssi().await else {
            return;
        };
        let name = match device.name().await.ok().flatten() {
            Some(name) if !name.is_empty() => name,
            _ => match self.state.lock().unwrap().names.get(&address) {
                Some(name) => name.clone(),
                None => return,
            },
        };
        let uuids = device.uuids().await.ok().flatten().unwrap_or_default();
        let manufacturer: Vec<(u16, Vec<u8>)> =
            device.manufacturer_data().await.ok().flatten().unwrap_or_default().into_iter().collect();
        let service_data: Vec<(Uuid, Vec<u8>)> =
            device.service_data().await.ok().flatten().unwrap_or_default().into_iter().collect();
        let services: Vec<usize> = {
            let mut state = self.state.lock().unwrap();
            state.names.insert(address, name.clone());
            if state.links.contains_key(&address) {
                return;
            }
            state.request.iter().enumerate().filter(|(_, s)| uuids.contains(&s.uuid)).map(|(i, _)| i).collect()
        };
        if services.is_empty() {
            return;
        }
        let sections = advertisement_sections(&name, &manufacturer, &service_data);
        self.send(Event::Advertisement { address, rssi, services, sections, name });
    }

    /// Inserts the link and starts its connect attempt, which the link owns.
    fn pair(&self, address: Address) {
        let Ok(device) = self.adapter.device(address) else {
            return;
        };
        let mut state = self.state.lock().unwrap();
        let Some(name) = state.names.get(&address).cloned() else {
            drop(state);
            return self.log(format!("Bluetooth helper: pair {address}: never seen in a scan"));
        };
        if state.links.contains_key(&address) {
            return;
        }
        let connect = tokio::spawn(self.clone().connect_link(device.clone(), name.clone()));
        state.links.insert(address, Link { device, name, chars: vec![], tasks: vec![connect] });
    }

    async fn connect_link(self, device: Device, name: String) {
        let address = device.address();
        self.log(format!("Bluetooth helper: connecting {name}"));
        match self.connect(&device, &name).await {
            Ok(()) => {
                let watcher = tokio::spawn(self.clone().watch_connection(device));
                match self.state.lock().unwrap().links.get_mut(&address) {
                    Some(link) => link.tasks.push(watcher),
                    None => watcher.abort(),
                }
            }
            Err(e) => {
                self.log(format!("Bluetooth helper: connect {name}: {e}"));
                let _ = device.disconnect().await;
                self.send(Event::Disconnected { address, unexpected: false, name });
                // Last: dropping the link aborts this task at its next await.
                let link = self.state.lock().unwrap().links.remove(&address);
                drop(link);
            }
        }
    }

    /// Connect, subscribe every requested characteristic that notifies or indicates,
    /// report the match, then read the readable ones, as the original DLL does.
    async fn connect(&self, device: &Device, name: &str) -> Result<(), String> {
        let address = device.address();
        self.ask_fast_interval(device, name).await;
        tokio::time::timeout(CONNECT_TIMEOUT, device.connect())
            .await
            .map_err(|_| "timeout".to_owned())?
            .map_err(|e| e.to_string())?;
        let resolved = async {
            while !device.is_services_resolved().await.unwrap_or(false) {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        };
        tokio::time::timeout(RESOLVE_TIMEOUT, resolved).await.map_err(|_| "services not resolved".to_owned())?;

        let request = self.state.lock().unwrap().request.clone();
        let services = device.services().await.map_err(|e| e.to_string())?;
        let mut matched = Vec::new();
        let mut chars = Vec::new();
        let mut tasks = Vec::new();
        let mut reads = Vec::new();
        for (i, spec) in request.iter().enumerate() {
            let mut service = None;
            for s in &services {
                if s.uuid().await.ok() == Some(spec.uuid) {
                    service = Some(s);
                    break;
                }
            }
            let Some(service) = service else { continue };
            let present = service.characteristics().await.map_err(|e| e.to_string())?;
            let mut by_uuid = Vec::new();
            for ch in present {
                if let Ok(uuid) = ch.uuid().await {
                    by_uuid.push((uuid, ch));
                }
            }
            let mut found = Vec::new();
            for (j, want) in spec.chars.iter().enumerate() {
                let Some((_, ch)) = by_uuid.iter().find(|(uuid, _)| uuid == want) else {
                    continue;
                };
                found.push(j);
                if chars.iter().any(|(uuid, _, _)| uuid == want) {
                    continue;
                }
                let flags = ch.flags().await.map_err(|e| e.to_string())?;
                let op = if flags.write_without_response { WriteOp::Command } else { WriteOp::Request };
                chars.push((*want, ch.clone(), op));
                if flags.notify || flags.indicate {
                    match ch.notify().await {
                        Ok(values) => {
                            let bridge = self.clone();
                            let (service, characteristic, name) = (spec.uuid, *want, name.to_owned());
                            tasks.push(tokio::spawn(async move {
                                tokio::pin!(values);
                                while let Some(data) = values.next().await {
                                    let name = name.clone();
                                    bridge.send(Event::Value { address, service, characteristic, data, name });
                                }
                            }));
                        }
                        Err(e) => self.log(format!("Bluetooth helper: subscribe {name} {want}: {e}")),
                    }
                }
                if flags.read {
                    reads.push((spec.uuid, *want, ch.clone()));
                }
            }
            matched.push((i, found));
        }
        {
            let mut state = self.state.lock().unwrap();
            let Some(link) = state.links.get_mut(&address) else {
                tasks.iter().for_each(JoinHandle::abort);
                return Err("unpaired while connecting".into());
            };
            link.chars = chars;
            link.tasks.extend(tasks);
        }
        self.send(Event::Connected { address, matched, name: name.to_owned() });
        self.log(format!("Bluetooth helper: connected {name}"));
        for (service, characteristic, ch) in reads {
            match ch.read().await {
                Ok(data) => self.send(Event::Value { address, service, characteristic, data, name: name.to_owned() }),
                Err(e) => self.log(format!("Bluetooth helper: read {name} {characteristic}: {e}")),
            }
        }
        Ok(())
    }

    /// Loads the kernel default interval (30 ms to 50 ms) for the next connection to the
    /// device. Without it, the kernel connects at the interval the sensor prefers, which
    /// bluetoothd loaded after the first service discovery: 180 ms on a KICKR makes the
    /// discovery take 17 s in place of 4 s, and 990 ms on a cadence sensor 53 s.
    async fn ask_fast_interval(&self, device: &Device, name: &str) {
        let mut mgmt = self.mgmt.lock().await;
        let Some(socket) = mgmt.as_ref() else {
            return;
        };
        let address_type = match device.address_type().await {
            Ok(address_type) => address_type,
            Err(e) => return self.log(format!("Bluetooth helper: address type of {name}: {e}")),
        };
        match socket.load_connection_parameters(device.address(), address_type).await {
            Ok(()) => {}
            Err(mgmt::Error::PermissionDenied) => {
                let fallback = "so connections use the preferred interval of the sensor";
                self.log(format!("Bluetooth helper: no CAP_NET_ADMIN, {fallback}"));
                *mgmt = None;
            }
            Err(mgmt::Error::Other(e)) => self.log(format!("Bluetooth helper: connection interval for {name}: {e}")),
        }
    }

    async fn watch_connection(self, device: Device) {
        let address = device.address();
        if let Ok(events) = device.events().await {
            tokio::pin!(events);
            while let Some(event) = events.next().await {
                if let DeviceEvent::PropertyChanged(DeviceProperty::Connected(false)) = event {
                    break;
                }
            }
        }
        let link = self.state.lock().unwrap().links.remove(&address);
        if let Some(link) = link {
            self.log(format!("Bluetooth helper: disconnected {}", link.name));
            // Dropping the link aborts this task too; send first.
            self.send(Event::Disconnected { address, unexpected: true, name: link.name.clone() });
            drop(link);
        }
    }

    /// Removes the link first, which ends its connect attempt or its watcher, so that
    /// nothing reports for the device after this.
    async fn unpair(&self, address: Address) {
        let link = self.state.lock().unwrap().links.remove(&address);
        let Some(link) = link else {
            return;
        };
        let (device, name) = (link.device.clone(), link.name.clone());
        drop(link);
        let _ = device.disconnect().await;
        self.send(Event::Disconnected { address, unexpected: false, name });
    }

    async fn write(self, address: Address, characteristic: Uuid, data: Vec<u8>) {
        let target = {
            let state = self.state.lock().unwrap();
            state.links.get(&address).and_then(|link| {
                let (_, ch, op) = link.chars.iter().find(|(uuid, _, _)| *uuid == characteristic)?;
                Some((ch.clone(), *op, link.name.clone()))
            })
        };
        let Some((ch, op_type, name)) = target else {
            return self.log(format!("Bluetooth helper: write to {address} {characteristic}: not connected"));
        };
        let request = CharacteristicWriteRequest { op_type, ..Default::default() };
        if let Err(e) = ch.write_ext(&data, &request).await {
            self.log(format!("Bluetooth helper: write {name} {characteristic}: {e}"));
        }
    }
}
