#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]

mod ws2812_driver;

extern crate alloc;

use alloc::boxed::Box;
use core::{mem::MaybeUninit, num::Wrapping};

use embassy_executor::Spawner;
use embassy_net::{tcp::TcpSocket, Config, Ipv4Address, Stack, StackResources};
use embassy_time::{Duration, Timer};
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
use esp_println::println;
use esp_wifi::{
    initialize,
    wifi::{WifiController, WifiDevice, WifiEvent, WifiStaDevice, WifiState},
    EspWifiInitFor,
};
use smart_leds::{
    brightness, gamma,
    hsv::{self, Hsv},
    SmartLedsWrite,
};

use crate::ws2812_driver::RmtWs2812;

const SSID: &str = env!("SSID");
const PASSWORD: &str = env!("PASSWORD");

#[global_allocator]
static ALLOCATOR: esp_alloc::EspHeap = esp_alloc::EspHeap::empty();

fn init_heap() {
    const HEAP_SIZE: usize = 32 * 1024;
    static mut HEAP: MaybeUninit<[u8; HEAP_SIZE]> = MaybeUninit::uninit();

    unsafe {
        ALLOCATOR.init(HEAP.as_mut_ptr() as *mut u8, HEAP_SIZE);
    }
}

#[esp32c3_hal::macros::main]
async fn main(spawner: Spawner) -> ! {
    #[cfg(feature = "log")]
    esp_println::logger::init_logger(log::LevelFilter::Info);

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

    let config = Config::dhcpv4(Default::default());

    let seed = 1234; // very random, very secure seed

    // Init network stack
    let stack = Box::leak(Box::new(Stack::new(
        wifi_interface,
        config,
        Box::leak(Box::new(StackResources::<3>::new())),
        seed,
    )));

    spawner.spawn(connection(controller)).ok();
    spawner.spawn(net_task(stack)).ok();

    let mut rx_buffer = [0; 4096];
    let mut tx_buffer = [0; 4096];

    loop {
        if stack.is_link_up() {
            break;
        }
        Timer::after(Duration::from_millis(500)).await;
    }

    println!("Waiting to get IP address...");
    loop {
        if let Some(config) = stack.config_v4() {
            println!("Got IP: {}", config.address);
            break;
        }
        Timer::after(Duration::from_millis(500)).await;
    }

    loop {
        Timer::after(Duration::from_millis(1_000)).await;

        let mut socket = TcpSocket::new(stack, &mut rx_buffer, &mut tx_buffer);

        socket.set_timeout(Some(embassy_time::Duration::from_secs(10)));

        let remote_endpoint = (Ipv4Address::new(142, 250, 185, 115), 80);
        println!("connecting...");
        let r = socket.connect(remote_endpoint).await;
        if let Err(e) = r {
            println!("connect error: {:?}", e);
            continue;
        }
        println!("connected!");
        let mut buf = [0; 1024];
        loop {
            use embedded_io_async::Write;
            let r = socket
                .write_all(b"GET / HTTP/1.0\r\nHost: www.mobile-j.de\r\n\r\n")
                .await;
            if let Err(e) = r {
                println!("write error: {:?}", e);
                break;
            }
            let n = match socket.read(&mut buf).await {
                Ok(0) => {
                    println!("read EOF");
                    break;
                }
                Ok(n) => n,
                Err(e) => {
                    println!("read error: {:?}", e);
                    break;
                }
            };
            println!("{}", core::str::from_utf8(&buf[..n]).unwrap());
        }
        Timer::after(Duration::from_millis(3000)).await;

        break;
    }

    let io = IO::new(peripherals.GPIO, peripherals.IO_MUX);
    let rmt = Rmt::new(peripherals.RMT, 20_000u32.kHz(), &clocks).unwrap();

    run_leds(rmt, io.pins.gpio6.into_push_pull_output()).await;
}

#[embassy_executor::task]
async fn connection(mut controller: WifiController<'static>) {
    println!("start connection task");
    println!("Device capabilities: {:?}", controller.get_capabilities());
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
            println!("Starting wifi");
            controller.start().await.unwrap();
            println!("Wifi started!");
        }
        println!("About to connect...");

        match controller.connect().await {
            Ok(_) => println!("Wifi connected!"),
            Err(e) => {
                println!("Failed to connect to wifi: {e:?}");
                Timer::after(Duration::from_millis(5000)).await
            }
        }
    }
}

#[embassy_executor::task]
async fn net_task(stack: &'static Stack<WifiDevice<'static, WifiStaDevice>>) {
    stack.run().await
}

async fn run_leds<P>(rmt: Rmt<'_>, pin: P) -> !
where
    P: Peripheral,
    <P as Peripheral>::P: OutputPin,
{
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

    let mut hue = Wrapping(0);

    loop {
        Timer::after(Duration::from_millis(10)).await;

        let colors = (0..8).map(|i| {
            hsv::hsv2rgb(Hsv {
                hue: hue.0 - i * 5,
                sat: 255,
                val: 255,
            })
        });

        let colors = gamma(colors);
        let colors = brightness(colors, 16);

        ws2812.write(colors).unwrap();

        hue += Wrapping(1);
    }
}
