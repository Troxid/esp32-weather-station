use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{PrimitiveStyle, Rectangle, StyledDrawable};

use esp_idf_svc::hal::i2c::*;
use esp_idf_svc::hal::prelude::*;

use ssd1306::{prelude::*, I2CDisplayInterface, Ssd1306};

fn main() {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    let peripheral = Peripherals::take().unwrap();

    let i2c_config = I2cConfig::new().baudrate(400u32.kHz().into());
    let screen_sda = peripheral.pins.gpio5;
    let screen_scl = peripheral.pins.gpio4;

    let i2c = I2cDriver::new(peripheral.i2c0, screen_sda, screen_scl, &i2c_config).unwrap();

    let interface = I2CDisplayInterface::new_custom_address(i2c, 0x3C);

    let mut display = Ssd1306::new(interface, DisplaySize128x64, DisplayRotation::Rotate0)
        .into_buffered_graphics_mode();

    display.init().unwrap();
    display.set_brightness(Brightness::BRIGHTEST).unwrap();

    let style1 = PrimitiveStyle::with_stroke(BinaryColor::On, 1);
    let rectangle = Rectangle::new(Point::new(20, 10), Size::new(20, 10));

    loop {
        display.clear_buffer();
        let _ = rectangle.draw_styled(&style1, &mut display);

        display.flush().unwrap();
    }
}
