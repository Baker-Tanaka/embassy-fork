#![no_std]
#![doc = include_str!("../README.md")]
#![warn(missing_docs)]
#![allow(async_fn_in_trait)]

use embassy_futures::select::{Either5, select5};
use embassy_net_driver_channel as ch;
use embassy_net_driver_channel::driver::LinkState;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use embedded_hal::digital::OutputPin;

use crate::ioctl::{PendingIoctl, Shared};

#[allow(unused)]
#[allow(non_snake_case)]
#[allow(non_camel_case_types)]
#[allow(non_upper_case_globals)]
#[allow(missing_docs)]
#[allow(clippy::all)]
mod proto;

// must be first
mod fmt;

mod config;
mod control;
mod iface;
mod ioctl;

pub use config::*;
pub use control::*;
pub use iface::*;

const MTU: usize = 1514;

macro_rules! impl_bytes {
    ($t:ident) => {
        impl $t {
            pub const SIZE: usize = core::mem::size_of::<Self>();

            #[allow(unused)]
            pub fn to_bytes(&self) -> [u8; Self::SIZE] {
                unsafe { core::mem::transmute(*self) }
            }

            #[allow(unused)]
            pub fn from_bytes(bytes: &[u8; Self::SIZE]) -> &Self {
                let alignment = core::mem::align_of::<Self>();
                assert_eq!(
                    bytes.as_ptr().align_offset(alignment),
                    0,
                    "{} is not aligned",
                    core::any::type_name::<Self>()
                );
                unsafe { core::mem::transmute(bytes) }
            }

            #[allow(unused)]
            pub fn from_bytes_mut(bytes: &mut [u8; Self::SIZE]) -> &mut Self {
                let alignment = core::mem::align_of::<Self>();
                assert_eq!(
                    bytes.as_ptr().align_offset(alignment),
                    0,
                    "{} is not aligned",
                    core::any::type_name::<Self>()
                );

                unsafe { core::mem::transmute(bytes) }
            }
        }
    };
}

#[repr(C, packed)]
#[derive(Clone, Copy, Debug, Default)]
struct PayloadHeader {
    /// InterfaceType on lower 4 bits, number on higher 4 bits.
    if_type_and_num: u8,

    /// Flags.
    ///
    /// bit 0: more fragments.
    flags: u8,

    len: u16,
    offset: u16,
    checksum: u16,
    seq_num: u16,
    reserved2: u8,

    /// Packet type for HCI or PRIV interface, reserved otherwise
    hci_priv_packet_type: u8,
}
impl_bytes!(PayloadHeader);

#[allow(unused)]
#[repr(u8)]
enum InterfaceType {
    Invalid = 0,
    Sta = 1,
    Ap = 2,
    Serial = 3,
    Hci = 4,
    Priv = 5,
    Test = 6,
}
const ESP_STA_IF: u8 = 1;
const ESP_SERIAL_IF: u8 = 3;

const SERIAL_MSG_HEADER_LEN: usize = 12;

/// Maximum size of the SPI buffers for the esp-hosted driver.
pub const MAX_SPI_BUFFER_SIZE: usize = 1600;
const HEARTBEAT_MAX_GAP: Duration = Duration::from_secs(20);

/// State for the esp-hosted driver.
pub struct State {
    shared: Shared,
    ch: ch::State<MTU, 4, 4>,
}

impl State {
    /// Create a new state.
    pub fn new() -> Self {
        Self {
            shared: Shared::new(),
            ch: ch::State::new(),
        }
    }
}

/// Events emitted by the esp-hosted driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EspHostedEvents {
    /// The heartbeat deadline has been reached.
    Deadline,
    /// The wifi link has been lost.
    Disconnected,
}

/// Type alias for network driver.
pub type NetDriver<'a> = ch::Device<'a, MTU>;

/// Create a new esp-hosted driver using the provided state, interface, and reset pin.
///
/// Returns a device handle for interfacing with embassy-net, a control handle for
/// interacting with the driver, and a runner for communicating with the WiFi device.
/// The optional events_notifier is used to signal the main loop when the heartbeat deadline is reached or when the wifi link is lost.
/// If not provided, the main loop will panic if the heartbeat deadline is reached.
pub async fn new<'a, I, OUT>(
    state: &'a mut State,
    iface: I,
    reset: OUT,
    events_notifier: Option<&'static Signal<NoopRawMutex, EspHostedEvents>>,
) -> (NetDriver<'a>, Control<'a>, Runner<'a, I, OUT>)
where
    I: Interface,
    OUT: OutputPin,
{
    let (ch_runner, device) = ch::new(&mut state.ch, ch::driver::HardwareAddress::Ethernet([0; 6]));
    let state_ch = ch_runner.state_runner();

    let runner = Runner {
        ch: ch_runner,
        state_ch,
        shared: &state.shared,
        next_seq: 1,
        heartbeat_deadline: Instant::now() + HEARTBEAT_MAX_GAP,
        iface,
        reset,
        events_notifier,
    };

    (device, Control::new(state_ch, &state.shared), runner)
}

/// Runner for communicating with the WiFi device.
pub struct Runner<'a, I, OUT> {
    ch: ch::Runner<'a, MTU>,
    state_ch: ch::StateRunner<'a>,
    shared: &'a Shared,

    next_seq: u16,
    heartbeat_deadline: Instant,

    iface: I,
    reset: OUT,

    events_notifier: Option<&'static Signal<NoopRawMutex, EspHostedEvents>>,
}

#[cfg_attr(all(not(feature = "log"), not(feature = "defmt")), allow(unused))]
pub(crate) struct HexSlice<'a>(pub &'a [u8]);

#[cfg(feature = "log")]
impl<'a> core::fmt::Display for HexSlice<'a> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "[")?;
        for byte in self.0 {
            write!(f, "{:02x}", byte)?;
        }
        write!(f, "]")?;
        Ok(())
    }
}

#[cfg(feature = "defmt")]
impl defmt::Format for HexSlice<'_> {
    fn format(&self, f: defmt::Formatter) {
        defmt::write!(f, "[");
        for byte in self.0 {
            defmt::write!(f, "{:02x}", byte);
        }
        defmt::write!(f, "]");
    }
}

#[allow(unused)]
fn print_eth_frame(frame: &[u8]) {
    let dest_mac_address = &frame[0..6];
    let source_mac_address = &frame[6..12];
    let ethertype = u16::from_be_bytes(frame[12..14].try_into().unwrap());

    debug!("dest_mac_address: {}", HexSlice(dest_mac_address));
    debug!("source_mac_address: {}", HexSlice(source_mac_address));
    debug!("ethertype: {:04x}", ethertype);
}

impl<'a, I, OUT> Runner<'a, I, OUT>
where
    I: Interface,
    OUT: OutputPin,
{
    /// Run the packet processing.
    pub async fn run(mut self, tx_buf: &mut [u8], rx_buf: &mut [u8]) -> ! {
        debug!("resetting...");
        self.reset_device().await;

        loop {
            self.iface.wait_for_handshake().await;

            let ioctl = self.shared.ioctl_wait_pending();
            let tx = self.ch.tx_buf();
            let ev = async { self.iface.wait_for_ready().await };
            let hb = Timer::at(self.heartbeat_deadline);
            let reset_request = self.shared.wait_for_reset_request();

            match select5(ioctl, tx, ev, hb, reset_request).await {
                Either5::First(PendingIoctl { buf, req_len }) => {
                    tx_buf[12..22].copy_from_slice(b"\x01\x06\x00RPCRsp\x02");
                    tx_buf[22..24].copy_from_slice(&(req_len as u16).to_le_bytes());
                    tx_buf[24..][..req_len].copy_from_slice(&unsafe { &*buf }[..req_len]);

                    let mut header = PayloadHeader {
                        if_type_and_num: InterfaceType::Serial as _,
                        len: (req_len + 12) as _,
                        offset: PayloadHeader::SIZE as _,
                        seq_num: self.next_seq,
                        ..Default::default()
                    };
                    self.next_seq = self.next_seq.wrapping_add(1);

                    // Calculate checksum
                    tx_buf[0..12].copy_from_slice(&header.to_bytes());
                    header.checksum = checksum(&tx_buf[..24 + req_len]);
                    tx_buf[0..12].copy_from_slice(&header.to_bytes());
                }
                Either5::Second(packet) => {
                    tx_buf[12..][..packet.len()].copy_from_slice(packet);

                    let mut header = PayloadHeader {
                        if_type_and_num: InterfaceType::Sta as _,
                        len: packet.len() as _,
                        offset: PayloadHeader::SIZE as _,
                        seq_num: self.next_seq,
                        ..Default::default()
                    };
                    self.next_seq = self.next_seq.wrapping_add(1);

                    // Calculate checksum
                    tx_buf[0..12].copy_from_slice(&header.to_bytes());
                    header.checksum = checksum(&tx_buf[..12 + packet.len()]);
                    tx_buf[0..12].copy_from_slice(&header.to_bytes());

                    self.ch.tx_done();
                }
                Either5::Third(()) => {
                    tx_buf[..PayloadHeader::SIZE].fill(0);
                }
                Either5::Fourth(()) => {
                    // Extend the deadline if initializing
                    if let ioctl::ControlState::Reboot = self.shared.state() {
                        self.reset_heartbeat_deadline();
                        continue;
                    }
                    match self.events_notifier {
                        Some(notifier) => {
                            notifier.signal(EspHostedEvents::Deadline);
                            self.reset_heartbeat_deadline();
                        }
                        None => {
                            panic!("heartbeat from esp32 stopped");
                        }
                    }
                }
                Either5::Fifth(()) => {
                    self.reset_device().await;
                    self.state_ch.set_link_state(LinkState::Down);
                    self.shared.reset_request_done();
                }
            }

            if tx_buf[0] != 0 {
                trace!("tx: {}", HexSlice(&tx_buf[..40]));
            }

            self.iface.transfer(rx_buf, tx_buf).await;

            self.handle_rx(rx_buf);
        }
    }

    fn handle_rx(&mut self, buf: &mut [u8]) {
        trace!("rx: {}", HexSlice(&buf[..40]));

        let buf_len = buf.len();
        let h =
            PayloadHeader::from_bytes_mut((&mut buf[..PayloadHeader::SIZE]).try_into().unwrap());

        if h.len == 0 || h.offset as usize != PayloadHeader::SIZE {
            return;
        }

        let payload_len = h.len as usize;
        if buf_len < PayloadHeader::SIZE + payload_len {
            warn!("rx: len too big");
            return;
        }

        let if_type_and_num = h.if_type_and_num;
        let want_checksum = h.checksum;
        h.checksum = 0;
        let got_checksum = checksum(&buf[..PayloadHeader::SIZE + payload_len]);
        if want_checksum != got_checksum {
            warn!(
                "rx: bad checksum. Got {:04x}, want {:04x}",
                got_checksum, want_checksum
            );
            return;
        }

        let payload = &mut buf[PayloadHeader::SIZE..][..payload_len];

        match if_type_and_num & 0x0f {
            // STA
            ESP_STA_IF => match self.ch.try_rx_buf() {
                Some(buf) => {
                    buf[..payload.len()].copy_from_slice(payload);
                    self.ch.rx_done(payload.len())
                }
                None => warn!("failed to push rxd packet to the channel."),
            },
            // serial
            ESP_SERIAL_IF => {
                trace!("serial rx: {}", HexSlice(payload));
                if payload.len() < SERIAL_MSG_HEADER_LEN {
                    warn!("serial rx: too short");
                    return;
                }

                let is_event = match &payload[..(SERIAL_MSG_HEADER_LEN - 2)] {
                    b"\x01\x06\x00RPCRsp\x02" => false,
                    b"\x01\x06\x00RPCEvt\x02" => true,
                    _ => {
                        warn!("serial rx: bad tlv: {}", HexSlice(&payload[..SERIAL_MSG_HEADER_LEN.min(payload.len())]));
                        return;
                    }
                };

                let len = u16::from_le_bytes(
                    payload[(SERIAL_MSG_HEADER_LEN - 2)..SERIAL_MSG_HEADER_LEN]
                        .try_into()
                        .unwrap(),
                ) as usize;
                if payload.len() < SERIAL_MSG_HEADER_LEN + len {
                    warn!("serial rx: too short 2");
                    return;
                }
                let data = &payload[SERIAL_MSG_HEADER_LEN..][..len];

                if is_event {
                    self.handle_event(data);
                } else {
                    self.shared.ioctl_done(data);
                }
            }
            _ => warn!("unknown iftype {}", if_type_and_num),
        }
    }

    fn handle_event(&mut self, data: &[u8]) {
        // Scratch minimal protobuf parser — no Rpc struct, no micropb, zero heap.
        // Protobuf encoding: msg_type=field1(varint), msg_id=field2(varint),
        // payload=field<msg_id>(LEN).  resp inside payload = field1(varint).
        //
        // Event RpcId constants (from esp_hosted_rpc.proto):
        //   769 = EventEspInit, 770 = EventHeartbeat
        //   775 = EventStaConnected, 776 = EventStaDisconnected

        let mut pos = 0usize;
        let mut msg_id: u32 = 0;
        let mut resp: i32 = 0;

        // First pass: collect msg_id
        while pos < data.len() {
            let Some(tag) = pb_read_varint(data, &mut pos) else { break };
            let field_num = (tag >> 3) as u32;
            let wire_type = (tag & 7) as u8;
            if field_num == 2 && wire_type == 0 {
                msg_id = match pb_read_varint(data, &mut pos) {
                    Some(v) => v as u32,
                    None => break,
                };
                break; // msg_id found, stop first pass
            } else if !pb_skip_field(data, &mut pos, wire_type) {
                break;
            }
        }

        if msg_id == 0 {
            warn!("handle_event: could not parse msg_id: {} bytes: {}", data.len(), HexSlice(data));
            return;
        }

        // Second pass: find payload sub-message (field number == msg_id value)
        let mut pos2 = 0usize;
        while pos2 < data.len() {
            let Some(tag) = pb_read_varint(data, &mut pos2) else { break };
            let field_num = (tag >> 3) as u32;
            let wire_type = (tag & 7) as u8;
            if field_num == msg_id && wire_type == 2 {
                // Read sub-message length
                let Some(sub_len) = pb_read_varint(data, &mut pos2) else { break };
                let sub_len = sub_len as usize;
                let sub_start = pos2;
                let sub_end = sub_start.saturating_add(sub_len).min(data.len());
                // Parse resp (field 1, varint) from sub-message
                let sub = &data[sub_start..sub_end];
                let mut sp = 0usize;
                while sp < sub.len() {
                    let Some(stag) = pb_read_varint(sub, &mut sp) else { break };
                    let sfn = (stag >> 3) as u32;
                    let swt = (stag & 7) as u8;
                    if sfn == 1 && swt == 0 {
                        resp = pb_read_varint(sub, &mut sp).unwrap_or(0) as i32;
                        break;
                    } else if !pb_skip_field(sub, &mut sp, swt) {
                        break;
                    }
                }
                break;
            } else if !pb_skip_field(data, &mut pos2, wire_type) {
                break;
            }
        }

        debug!("event: msg_id={} resp={}", msg_id, resp);

        match msg_id {
            769 => {
                // EventEspInit
                self.shared.init_done();
                self.reset_heartbeat_deadline();
            }
            770 => {
                // EventHeartbeat
                self.reset_heartbeat_deadline();
            }
            775 => {
                // EventStaConnected
                info!("connected, code {}", resp);
                self.state_ch.set_link_state(LinkState::Up);
                self.shared.link_up_done();

                // ESP-Hosted firmware sends disconnect event even for failed connection attempts.
                // We reset the signal to avoid spurious disconnection events.
                if let Some(notifier) = self.events_notifier {
                    notifier.reset();
                }
            }
            776 => {
                // EventStaDisconnected
                info!("disconnected, code {}", resp);
                self.state_ch.set_link_state(LinkState::Down);
                self.shared.link_down_done();

                if let Some(notifier) = self.events_notifier {
                    notifier.signal(EspHostedEvents::Disconnected);
                }
            }
            _ => {
                debug!("unknown event msg_id={}", msg_id);
            }
        }
    }

    fn reset_heartbeat_deadline(&mut self) {
        self.heartbeat_deadline = Instant::now() + HEARTBEAT_MAX_GAP;
    }

    async fn reset_device(&mut self) {
        self.reset.set_low().unwrap();
        Timer::after_millis(100).await;
        self.reset.set_high().unwrap();
        Timer::after_millis(1000).await;
        self.next_seq = 1;
    }
}

/// Decode a protobuf varint from `data` starting at `*pos`.
/// Advances `*pos` past the varint. Returns `None` on truncation or overflow (>10 bytes).
pub(crate) fn pb_read_varint(data: &[u8], pos: &mut usize) -> Option<u64> {
    let mut result: u64 = 0;
    let mut shift = 0u32;
    for _ in 0..10 {
        let b = *data.get(*pos)?;
        *pos += 1;
        result |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return Some(result);
        }
        shift += 7;
    }
    None // varint exceeds 10 bytes
}

/// Skip one protobuf field value given its `wire_type`.
/// Returns `false` if the data is malformed or truncated.
pub(crate) fn pb_skip_field(data: &[u8], pos: &mut usize, wire_type: u8) -> bool {
    match wire_type {
        0 => pb_read_varint(data, pos).is_some(), // varint
        1 => {
            // 64-bit fixed
            if *pos + 8 <= data.len() {
                *pos += 8;
                true
            } else {
                false
            }
        }
        2 => {
            // length-delimited
            if let Some(len) = pb_read_varint(data, pos) {
                let len = len as usize;
                if *pos + len <= data.len() {
                    *pos += len;
                    true
                } else {
                    false
                }
            } else {
                false
            }
        }
        5 => {
            // 32-bit fixed
            if *pos + 4 <= data.len() {
                *pos += 4;
                true
            } else {
                false
            }
        }
        _ => false, // unknown wire type
    }
}

fn checksum(buf: &[u8]) -> u16 {
    let mut res = 0u16;
    for &b in buf {
        res = res.wrapping_add(b as _);
    }
    res
}
