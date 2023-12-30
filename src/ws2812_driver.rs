use esp32c3_hal::rmt::{self, PulseCode, TxChannel};
use smart_leds::{SmartLedsWrite, RGB8};

pub struct RmtWs2812<RMT: TxChannel<N>, const N: u8, const LEDS: usize>
where
    [(); LEDS * 3 * 8]:,
{
    rmt: Option<RMT>,
    buffer: [PulseCode; LEDS * 3 * 8],
}

impl<RMT: TxChannel<N>, const N: u8, const LEDS: usize> RmtWs2812<RMT, N, LEDS>
where
    [(); LEDS * 3 * 8]:,
{
    pub fn new(rmt: RMT) -> Self {
        Self {
            rmt: Some(rmt),
            buffer: [PulseCode::default(); _],
        }
    }
}

impl<RMT: TxChannel<N>, const N: u8, const LEDS: usize> SmartLedsWrite for RmtWs2812<RMT, N, LEDS>
where
    [(); LEDS * 3 * 8]:,
{
    type Error = rmt::Error;
    type Color = RGB8;

    fn write<T, I>(&mut self, iterator: T) -> Result<(), Self::Error>
    where
        T: Iterator<Item = I>,
        I: Into<Self::Color>,
    {
        iterator
            .flat_map(|color| {
                let color: RGB8 = color.into();
                [color.r, color.g, color.b]
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
                        length1: 16,
                        level2: false,
                        length2: 9,
                    }
                } else {
                    PulseCode {
                        level1: true,
                        length1: 8,
                        level2: false,
                        length2: 17,
                    }
                }
            })
            .zip(self.buffer.iter_mut())
            .for_each(|(pulse, buffer)| *buffer = pulse);

        self.buffer.last_mut().unwrap().length2 = 0;

        match self.rmt.take().unwrap().transmit(&self.buffer).wait() {
            Ok(channel) => self.rmt = Some(channel),
            Err((e, channel)) => {
                log::error!("Error: {:?}", e);
                self.rmt = Some(channel)
            }
        };

        Ok(())
    }
}
