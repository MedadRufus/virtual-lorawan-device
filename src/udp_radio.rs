use super::udp_runtime;
use base64;
use heapless::consts::*;
use heapless::Vec as HVec;
use lorawan_device::{radio::*, Event as LorawanEvent, Radio};
use semtech_udp::{PacketData, PushData, RxPk, gateway_mac};
use tokio::sync::mpsc::{self, Receiver, Sender};

#[derive(Debug)]
pub enum RadioEvent {
    UdpRx(udp_runtime::RxMessage),
    TxDone,
}

#[derive(Debug)]
pub enum Event {
    Radio(RadioEvent),
    LoRaWAN(LorawanEvent),
}

struct Settings {
    bw: Bandwidth,
    sf: SpreadingFactor,
    cr: CodingRate,
    freq: u32,
}

impl Settings {
    fn get_datr(&self) -> String {
        format!(
            "{}{}",
            match self.sf {
                SpreadingFactor::_7 => "SF7",
                SpreadingFactor::_8 => "SF8",
                SpreadingFactor::_9 => "SF9",
                SpreadingFactor::_10 => "SF10",
                SpreadingFactor::_11 => "SF11",
                SpreadingFactor::_12 => "SF12",
            },
            match self.bw {
                Bandwidth::_125KHZ => "BW125",
                Bandwidth::_250KHZ => "BW250",
                Bandwidth::_500KHZ => "BW500",
            }
        )
    }

    fn get_codr(&self) -> String {
        match self.cr {
            CodingRate::_4_5 => "4/5",
            CodingRate::_4_6 => "4/6",
            CodingRate::_4_7 => "4/7",
            CodingRate::_4_8 => "4/8",
        }
        .to_string()
    }

    fn get_freq(&self) -> f64 {
        self.freq as f64 / 1_000_000.0
    }
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            bw: Bandwidth::_125KHZ,
            sf: SpreadingFactor::_10,
            cr: CodingRate::_4_5,
            freq: 902300000,
        }
    }
}

// Runtime translates UDP events into Device events
pub struct UdpRadioRuntime {
    receiver: Receiver<udp_runtime::RxMessage>,
    lorawan_sender: Sender<Event>,
}

impl UdpRadioRuntime {
    pub async fn run(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        loop {
            if let Some(event) = self.receiver.recv().await {
                self.lorawan_sender
                    .send(Event::Radio(RadioEvent::UdpRx(event)))
                    .await?;
            }
        }
    }
}

pub struct UdpRadio {
    sender: Sender<udp_runtime::TxMessage>,
    lorawan_sender: Sender<Event>,
    rx_buffer: HVec<u8, U256>,
    settings: Settings,
}

impl UdpRadio {
    pub fn new(
        sender: Sender<udp_runtime::TxMessage>,
        receiver: Receiver<udp_runtime::RxMessage>,
    ) -> (Receiver<Event>, UdpRadioRuntime, Sender<Event>, UdpRadio) {
        let (lorawan_sender, lorawan_receiver) = mpsc::channel(100);
        let lorawan_sender_clone = lorawan_sender.clone();
        let lorawan_sender_another_clone = lorawan_sender.clone();

        (
            lorawan_receiver,
            UdpRadioRuntime {
                receiver,
                lorawan_sender,
            },
            lorawan_sender_another_clone,
            UdpRadio {
                sender,
                lorawan_sender: lorawan_sender_clone,
                rx_buffer: HVec::new(),
                settings: Settings::default(),
            },
        )
    }
}

impl Radio for UdpRadio {
    type Event = RadioEvent;

    fn send(&mut self, buffer: &mut [u8]) {
        println!("Sending!");
        let size = buffer.len() as u64;
        let data = base64::encode(buffer);

        let mut packet = Vec::new();
        println!("{} {}", self.settings.get_codr(), self.settings.get_datr());
        packet.push({
            RxPk {
                chan: 0,
                codr: self.settings.get_codr(),
                data,
                datr: self.settings.get_datr(),
                freq: self.settings.get_freq(),
                lsnr: 5.5,
                modu: "LORA".to_string(),
                rfch: 0,
                rssi: -112,
                size,
                stat: 1,
                tmst: 320000,
            }
        });
        let rxpk = Some(packet);

        let foo = [0x12,0x45,0x32,0x42,0x33,0x00,0x00, 0x12,0x23,0x32,0x3,0x3];

        let packet = semtech_udp::Packet {
            random_token: 0x00,
            gateway_mac: Some(gateway_mac(&foo)),
            data: PacketData::PushData(PushData { rxpk, stat: None }),
        };

        println!("{:?}",packet);

        if let Err(e) = self.sender.try_send(packet) {
            panic!("UdpTx Queue Overflow! {}", e)
        }

        // sending the packet pack to "ourselves" simulates a SX12xx DI0 interrupt
        if let Err(e) = self
            .lorawan_sender
            .try_send(Event::Radio(RadioEvent::TxDone))
        {
            panic!("LoRaWAN Queue Overflow! {}", e)
        }
    }

    fn set_frequency(&mut self, frequency_mhz: u32) {
        self.settings.freq = frequency_mhz;
    }

    fn get_received_packet(&mut self) -> &mut HVec<u8, U256> {
        &mut self.rx_buffer
    }

    fn configure_tx(
        &mut self,
        _power: i8,
        bandwidth: Bandwidth,
        spreading_factor: SpreadingFactor,
        coderate: CodingRate,
    ) {
        self.settings.bw = bandwidth;
        self.settings.sf = spreading_factor;
        self.settings.cr = coderate;
    }

    fn configure_rx(
        &mut self,
        bandwidth: Bandwidth,
        spreading_factor: SpreadingFactor,
        coderate: CodingRate,
    ) {
        self.settings.bw = bandwidth;
        self.settings.sf = spreading_factor;
        self.settings.cr = coderate;
    }

    fn set_rx(&mut self) {
        // normaly, this would configure the radio,
        // but the UDP port is always running concurrently
    }

    fn handle_event(&mut self, event: Self::Event) -> State {
        match event {
            RadioEvent::TxDone => State::TxDone,
            RadioEvent::UdpRx(pkt) => {
                match pkt.data {
                    semtech_udp::PacketData::PullResp(pull_data) => {
                        let txpk = pull_data.txpk;
                        match base64::decode(txpk.data) {
                            Ok(data) => {
                                self.rx_buffer.clear();
                                for el in data {
                                    if let Err(e) = self.rx_buffer.push(el) {
                                        panic!("Error pushing data into rx_buffer {}", e);
                                    }
                                }
                                State::RxDone(RxQuality::new(-115, 4))
                            }
                            Err(e) => {
                                panic!("Semtech UDP Packet Decoding Error {}", e)
                            }
                        }
                    }
                    _ => panic!("Unhandled packet type")
                }
            }
        }
    }
}
