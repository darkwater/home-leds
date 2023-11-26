use alloc::vec::Vec;

use esp32c3_hal::rmt::{self, PulseCode, TxChannel};
use smart_leds::{SmartLedsWrite, RGB8};

pub struct RmtWs2812<RMT: TxChannel<N>, const N: u8> {
    rmt: Option<RMT>,
}

impl<RMT: TxChannel<N>, const N: u8> RmtWs2812<RMT, N> {
    pub fn new(rmt: RMT) -> Self {
        Self { rmt: Some(rmt) }
    }
}

impl<RMT: TxChannel<N>, const N: u8> SmartLedsWrite for RmtWs2812<RMT, N> {
    type Error = rmt::Error;
    type Color = RGB8;

    fn write<T, I>(&mut self, iterator: T) -> Result<(), Self::Error>
    where
        T: Iterator<Item = I>,
        I: Into<Self::Color>,
    {
        let mut pulses = iterator
            .flat_map(|color| {
                let color: RGB8 = color.into();
                [color.g, color.r, color.b]
            })
            .flat_map(|byte| {
                [
                    byte & 0b1000_0000 != 0,
                    byte & 0b0100_0000 != 0,
                    byte & 0b0010_0000 != 0,
                    byte & 0b0001_0000 != 0,
                    byte & 0b0000_1000 != 0,
                    byte & 0b0000_0100 != 0,
                    byte & 0b0000_0010 != 0,
                    byte & 0b0000_0001 != 0,
                ]
            })
            .map(|bit| {
                if bit {
                    PulseCode {
                        level1: true,
                        length1: 80 / 5,
                        level2: false,
                        length2: 45 / 5,
                    }
                } else {
                    PulseCode {
                        level1: true,
                        length1: 40 / 5,
                        level2: false,
                        length2: 85 / 5,
                    }
                }
            })
            .chain(core::iter::once(PulseCode {
                level1: true,
                length1: 5,
                level2: false,
                length2: 0,
            }))
            .collect::<Vec<_>>();

        pulses.last_mut().unwrap().length2 = 0;

        match self.rmt.take().unwrap().transmit(&pulses).wait() {
            Ok(channel) => self.rmt = Some(channel),
            Err((e, channel)) => {
                log::error!("Error: {:?}", e);
                self.rmt = Some(channel)
            }
        };

        Ok(())
    }
}
