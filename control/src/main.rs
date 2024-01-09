#![allow(clippy::identity_op)]

use std::{
    net::{ToSocketAddrs, UdpSocket},
    thread::sleep,
    time::{Duration, Instant},
};

const ADDR: &str = "192.168.0.119:7777";
const LEDS: usize = 100;

fn main() -> anyhow::Result<()> {
    let sock = UdpSocket::bind("0.0.0.0:0")?;
    let addr = ADDR.to_socket_addrs()?.next().unwrap();

    let mut state = [LedState::Idle; LEDS];

    let mut last = Instant::now();
    loop {
        let now = Instant::now();
        let dt = (now - last).as_secs_f32();
        last = now;

        for state in state.iter_mut() {
            state.tick(dt);
        }

        let buf = state
            .iter()
            .copied()
            .map(Rgb::from)
            .flat_map(<[u8; 3]>::from)
            .collect::<Vec<u8>>();

        sock.send_to(&buf, addr)?;
        sleep(Duration::from_millis(15));
    }
}

pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    const WHITE: Rgb = Rgb {
        r: 255,
        g: 255,
        b: 255,
    };
    const BLACK: Rgb = Rgb { r: 0, g: 0, b: 0 };

    const fn grey(value: u8) -> Rgb {
        Rgb {
            r: value,
            g: value,
            b: value,
        }
    }
}

impl From<Rgb> for [u8; 3] {
    fn from(rgb: Rgb) -> Self {
        [rgb.r, rgb.g, rgb.b]
    }
}

#[derive(Debug, Clone, Copy)]
pub enum LedState {
    Idle,
    StarFadeIn { progress: f32, speed: f32 },
    StarFadeOut { progress: f32, speed: f32 },
}

impl LedState {
    pub fn tick(&mut self, dt: f32) {
        match self {
            LedState::Idle => {
                if rand::random::<f32>() < dt * 0.1 {
                    *self = LedState::StarFadeIn {
                        progress: 0.,
                        speed: rand::random::<f32>() * 0.5 + 0.5,
                    };
                }
            }
            LedState::StarFadeIn { progress, speed } => {
                *progress += dt * *speed;
                if *progress >= 1. {
                    *self = LedState::StarFadeOut {
                        progress: 1.,
                        speed: rand::random::<f32>() * 0.9 + 0.1,
                    };
                }
            }
            LedState::StarFadeOut { progress, speed } => {
                *progress -= dt * *speed;
                if *progress <= 0.0 {
                    *self = LedState::Idle;
                }
            }
        }
    }
}

impl From<LedState> for Rgb {
    fn from(state: LedState) -> Self {
        match state {
            LedState::Idle => Rgb::BLACK,
            LedState::StarFadeIn { progress, .. } => Rgb::grey((progress * 255.) as u8),
            LedState::StarFadeOut { progress, .. } => Rgb::grey((progress * 255.) as u8),
        }
    }
}
