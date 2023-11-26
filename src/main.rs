#![no_std]
#![no_main]
#![feature(associated_type_bounds)]
#![feature(never_type)]
#![feature(type_alias_impl_trait)]

mod ws2812_driver;

extern crate alloc;

use alloc::boxed::Box;
use core::{
    future::pending,
    mem::{self, MaybeUninit},
    num::Wrapping,
};

use dnsparse::{Answer, HeaderKind, QueryClass, QueryKind};
use embassy_executor::{raw::TaskStorage, Spawner};
use embassy_net::{
    udp::{PacketMetadata, UdpSocket},
    Config, IpListenEndpoint, Ipv4Address, Stack, StackResources,
};
use embassy_time::{Duration, Ticker, Timer};
use embedded_svc::wifi::{ClientConfiguration, Configuration, Wifi};
use esp32c3_hal::{
    clock::ClockControl,
    embassy,
    gpio::OutputPin,
    peripheral::Peripheral,
    peripherals::Peripherals,
    prelude::*,
    rmt::{TxChannelConfig, TxChannelCreator},
    timer::TimerGroup,
    Rmt, Rng, IO,
};
use esp_backtrace as _;
use esp_wifi::{
    initialize,
    wifi::{WifiController, WifiDevice, WifiEvent, WifiStaDevice, WifiState},
    EspWifiInitFor,
};
use futures_util::Future;
use smart_leds::{
    brightness, gamma,
    hsv::{self, Hsv},
    SmartLedsWrite,
};

use crate::ws2812_driver::RmtWs2812;

const LEDS: u8 = 16;

fn led_position(idx: u8) -> (f32, f32) {
    ((idx / 8) as f32 * 2., -(idx as f32 % 8.))
}

const SSID: &str = env!("SSID");
const PASSWORD: &str = env!("PASSWORD");

const MDNS_ADDR: Ipv4Address = Ipv4Address::new(224, 0, 0, 251);
const MDNS_PORT: u16 = 5353;

const HEAP_SIZE: usize = 32 * 1024;

macro_rules! make_static {
    ($expr:expr) => {{
        type T = impl Sized;
        static mut X: MaybeUninit<T> = core::mem::MaybeUninit::<T>::uninit();
        #[allow(unused_unsafe)]
        let (x,) = unsafe { X.write(($expr,)) };
        x
    }};
}

#[global_allocator]
static ALLOCATOR: esp_alloc::EspHeap = esp_alloc::EspHeap::empty();

fn init_heap() {
    static mut HEAP: MaybeUninit<[u8; HEAP_SIZE]> = MaybeUninit::uninit();

    unsafe {
        ALLOCATOR.init(HEAP.as_mut_ptr() as *mut u8, HEAP_SIZE);
    }
}

pub async fn spawn_task<F, Fut>(name: &str, f: F)
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = !> + 'static,
{
    let task = Box::leak(Box::new(TaskStorage::new()));
    log::debug!(
        "Spawning task at {:#p} size {:}: {}",
        task,
        mem::size_of::<TaskStorage<Fut>>(),
        name,
    );

    let token = task.spawn(f);
    Spawner::for_current_executor().await.must_spawn(token);
}

#[esp32c3_hal::macros::main]
async fn main(spawner: Spawner) -> ! {
    esp_println::logger::init_logger_from_env();

    init_heap();

    let peripherals = Peripherals::take();

    let system = peripherals.SYSTEM.split();
    let clocks = ClockControl::max(system.clock_control).freeze();

    #[cfg(target_arch = "xtensa")]
    let timer = esp32c3_hal::timer::TimerGroup::new(peripherals.TIMG1, &clocks).timer0;
    #[cfg(target_arch = "riscv32")]
    let timer = esp32c3_hal::systimer::SystemTimer::new(peripherals.SYSTIMER).alarm0;
    let init = initialize(
        EspWifiInitFor::Wifi,
        timer,
        Rng::new(peripherals.RNG),
        system.radio_clock_control,
        &clocks,
    )
    .unwrap();

    let wifi = peripherals.WIFI;
    let (wifi_interface, controller) =
        esp_wifi::wifi::new_with_mode(&init, wifi, WifiStaDevice).unwrap();

    let timer_group0 = TimerGroup::new(peripherals.TIMG0, &clocks);
    embassy::init(&clocks, timer_group0.timer0);

    spawn_task("LEDs", {
        let io = IO::new(peripherals.GPIO, peripherals.IO_MUX);
        let rmt = Rmt::new(peripherals.RMT, 20u32.MHz(), &clocks).unwrap();

        || run_leds(rmt, io.pins.gpio6.into_push_pull_output())
    })
    .await;

    let config = Config::dhcpv4(Default::default());

    let seed = 1234; // very random, very secure seed

    // Init network stack
    let stack = make_static!(Stack::new(
        wifi_interface,
        config,
        make_static!(StackResources::<3>::new()),
        seed,
    ));

    spawner.spawn(connection(controller)).ok();
    spawner.spawn(net_task(stack)).ok();

    let rx_meta = make_static!([PacketMetadata::EMPTY; 8]);
    let rx_buffer = make_static!([0; 1500]);
    let tx_meta = make_static!([PacketMetadata::EMPTY; 8]);
    let tx_buffer = make_static!([0; 1500]);

    loop {
        if stack.is_link_up() {
            break;
        }
        Timer::after(Duration::from_millis(500)).await;
    }

    log::info!("Waiting to get IP address...");
    stack.wait_config_up().await;

    let Some(config) = stack.config_v4() else {
        panic!("No IPv4 config");
    };

    log::info!("Got IP: {}", config.address);

    let mut mdns_socket = UdpSocket::new(stack, rx_meta, rx_buffer, tx_meta, tx_buffer);

    mdns_socket
        .bind(IpListenEndpoint { addr: None, port: MDNS_PORT })
        .unwrap();

    stack.join_multicast_group(MDNS_ADDR).await.unwrap();

    spawner
        .spawn(mdns_task(mdns_socket, config.address.address()))
        .expect("spawn mdns task");

    spawn_task("Heap monitor", || async move {
        loop {
            log::debug!("Heap usage: {}, free: {}", ALLOCATOR.used(), ALLOCATOR.free());

            Timer::after(Duration::from_secs(10)).await;
        }
    })
    .await;

    pending().await
}

#[embassy_executor::task]
async fn mdns_task(socket: UdpSocket<'static>, address: Ipv4Address) -> ! {
    log::debug!("Starting mDNS responder");

    let mut buf = [0; 1024];

    loop {
        let Ok((n, peer)) = socket.recv_from(&mut buf).await else {
            continue;
        };

        log::trace!("Received {} bytes from {}", n, peer);

        let Ok(msg) = dnsparse::Message::parse(&mut buf[..n]) else {
            continue;
        };

        for question in msg.questions() {
            if question.name() == "home-leds.local" && *question.kind() == QueryKind::A {
                log::debug!("Responding to mDNS query");

                let mut buf = dnsparse::Message::BUFFER;

                let mut msg = dnsparse::Message::builder(&mut buf)
                    .header(
                        dnsparse::Header::builder()
                            .kind(HeaderKind::Response)
                            .build(),
                    )
                    .build();

                msg.add_answer(&Answer {
                    name: question.name().clone(),
                    kind: QueryKind::A,
                    class: QueryClass::IN,
                    ttl: 120,
                    rdata: address.as_bytes(),
                });

                socket
                    .send_to(&msg, (MDNS_ADDR, MDNS_PORT))
                    .await
                    .expect("send");
            }
        }
    }
}

#[embassy_executor::task]
async fn connection(mut controller: WifiController<'static>) {
    log::info!("start connection task");
    log::info!("Device capabilities: {:?}", controller.get_capabilities());
    loop {
        if let WifiState::StaConnected = esp_wifi::wifi::get_wifi_state() {
            // wait until we're no longer connected
            controller.wait_for_event(WifiEvent::StaDisconnected).await;
            Timer::after(Duration::from_millis(5000)).await
        }

        if !matches!(controller.is_started(), Ok(true)) {
            let client_config = Configuration::Client(ClientConfiguration {
                ssid: SSID.into(),
                password: PASSWORD.into(),
                ..Default::default()
            });
            controller.set_configuration(&client_config).unwrap();
            log::info!("Starting wifi");
            controller.start().await.unwrap();
            log::info!("Wifi started!");
        }
        log::info!("About to connect...");

        match controller.connect().await {
            Ok(_) => log::info!("Wifi connected!"),
            Err(e) => {
                log::info!("Failed to connect to wifi: {e:?}");
                Timer::after(Duration::from_millis(5000)).await
            }
        }
    }
}

#[embassy_executor::task]
async fn net_task(stack: &'static Stack<WifiDevice<'static, WifiStaDevice>>) {
    stack.run().await
}

async fn run_leds<P: Peripheral<P: OutputPin>>(rmt: Rmt<'static>, pin: P) -> ! {
    let channel = rmt
        .channel0
        .configure(pin, TxChannelConfig {
            clk_divider: 1,
            idle_output_level: false,
            idle_output: true,
            carrier_modulation: false,
            carrier_high: 1,
            carrier_low: 1,
            carrier_level: false,
        })
        .unwrap();

    let mut ws2812 = RmtWs2812::new(channel);

    let mut hue = 0.;

    let mut ticker = Ticker::every(Duration::from_micros(16_666));

    loop {
        let colors = (0..LEDS).map(led_position).map(|(x, y)| {
            let f = 30.;
            hsv::hsv2rgb(Hsv {
                hue: ((hue + (x * f) + (y * f)) % u8::MAX as f32) as u8,
                sat: 255,
                val: 255,
            })
        });

        let colors = gamma(colors);
        let colors = brightness(colors, 16);

        ws2812.write(colors).unwrap();

        hue += 1.;

        ticker.next().await;
    }
}
