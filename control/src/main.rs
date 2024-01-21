#![allow(clippy::identity_op)]

use std::{
    net::{ToSocketAddrs, UdpSocket},
    ops::Mul,
    sync::{Arc, Mutex},
    thread::sleep,
    time::{Duration, Instant},
};

use config::builder::DefaultState;
use home_assistant_rest::Client;
use serde::{
    de::{value::MapDeserializer, IntoDeserializer},
    Deserialize,
};
use serde_json::Value;

#[derive(Clone, Deserialize)]
struct Config {
    address: String,
    leds: usize,

    home_assistant: HomeAssistantConfig,
}

#[derive(Clone, Deserialize)]
struct HomeAssistantConfig {
    url: String,
    token: String,
}

pub struct GlobalState {
    color: Rgb,
}

#[derive(Clone, Deserialize)]
struct LightAttributes {
    rgb_color: Option<[u8; 3]>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config_path = dirs::config_dir()
        .ok_or_else(|| anyhow::anyhow!("No config dir found"))?
        .join("home-leds/config");

    let config = config::ConfigBuilder::<DefaultState>::default()
        .add_source(config::File::with_name(config_path.to_str().unwrap()))
        .add_source(config::Environment::with_prefix("HOME_LEDS"))
        .build()?
        .try_deserialize::<Config>()?;

    let sock = UdpSocket::bind("0.0.0.0:0")?;
    let addr = config.address.to_socket_addrs()?.next().unwrap();

    let global_state = Arc::new(Mutex::new(GlobalState { color: Rgb::BLACK }));

    tokio::spawn({
        let config = config.clone();
        let global_state = global_state.clone();
        async move {
            loop {
                let client = Client::new(&config.home_assistant.url, &config.home_assistant.token)?;
                let api_status = client.get_api_status().await?;

                if api_status.message != "API running." {
                    println!("API is NOT running");
                } else {
                    let state_entity = client.get_states_of_entity("light.south").await?;
                    let light = LightAttributes::deserialize(MapDeserializer::new(
                        state_entity.attributes.into_iter(),
                    ))?;
                    let color: Rgb = light.rgb_color.unwrap_or([0, 0, 0]).into();
                    global_state.lock().unwrap().color = color;
                }

                tokio::time::sleep(Duration::from_secs(5)).await;
            }

            #[allow(unreachable_code)]
            Result::<(), anyhow::Error>::Ok(())
        }
    });

    let mut state = vec![LedState::Idle; config.leds];

    let mut last = Instant::now();
    loop {
        let now = Instant::now();
        let dt = (now - last).as_secs_f32();
        last = now;

        {
            let global_state = global_state.lock().unwrap();
            for state in state.iter_mut() {
                state.tick(dt, &global_state);
            }
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

#[derive(Debug, Clone, Copy)]
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

impl From<[u8; 3]> for Rgb {
    fn from([r, g, b]: [u8; 3]) -> Self {
        Rgb { r, g, b }
    }
}

impl Mul<f32> for Rgb {
    type Output = Rgb;

    fn mul(self, rhs: f32) -> Self::Output {
        Rgb {
            r: (self.r as f32 * rhs) as u8,
            g: (self.g as f32 * rhs) as u8,
            b: (self.b as f32 * rhs) as u8,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum LedState {
    Idle,
    StarFadeIn {
        color: Rgb,
        progress: f32,
        speed: f32,
    },
    StarFadeOut {
        color: Rgb,
        progress: f32,
        speed: f32,
    },
}

impl LedState {
    pub fn tick(&mut self, dt: f32, global: &GlobalState) {
        match *self {
            LedState::Idle => {
                if rand::random::<f32>() < dt * 0.1 {
                    *self = LedState::StarFadeIn {
                        color: global.color,
                        progress: 0.,
                        speed: rand::random::<f32>() * 0.5 + 0.5,
                    };
                }
            }
            LedState::StarFadeIn {
                color,
                ref mut progress,
                speed,
            } => {
                *progress += dt * speed;
                if *progress >= 1. {
                    *self = LedState::StarFadeOut {
                        color,
                        progress: 1.,
                        speed: rand::random::<f32>() * 0.9 + 0.1,
                    };
                }
            }
            LedState::StarFadeOut {
                color: _,
                ref mut progress,
                speed,
            } => {
                *progress -= dt * speed;
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
            LedState::StarFadeIn {
                color,
                progress,
                speed: _,
            } => color * progress,
            LedState::StarFadeOut {
                color,
                progress,
                speed: _,
            } => color * progress,
        }
    }
}
