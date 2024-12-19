use std::sync::RwLock;
use std::time::{Duration, Instant};
use std::thread;

use anyhow::Context;
use ascii::{FONT_4X6, FONT_7X13, FONT_9X15_BOLD};
use chrono::{DateTime, Datelike, Local, Timelike};
use embedded_graphics::image::Image;
use embedded_graphics::mono_font::*;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{Arc, PrimitiveStyle, Rectangle, StyledDrawable, Triangle};
use embedded_graphics::text::{Alignment, Baseline, Text, TextStyleBuilder};
use embedded_layout::align::{horizontal, vertical, Align};
use embedded_svc::utils::io::try_read_full;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::hal::i2c::*;
use esp_idf_svc::hal::prelude::*;
use esp_idf_svc::hal::task::thread::ThreadSpawnConfiguration;
use esp_idf_svc::http::client::EspHttpConnection;
use esp_idf_svc::http::{self, Method};
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::sntp::EspSntp;
use esp_idf_svc::wifi::{AuthMethod, ClientConfiguration, Configuration, EspWifi};
use ssd1306::{prelude::*, I2CDisplayInterface, Ssd1306};
use tinytga::Tga;

fn main() {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    // Получение периферии (GPIO, I2C и т.д.) МК 
    // `.unwrap()` в конце возвращаемого значения означает критическое завершение программы и перезагрузка МК в случае ошибки 
    // для fail-fast или демострационных целей, достаточно вызывать `.unwrap()`
    let peripheral = Peripherals::take().unwrap();

    // Создание конфигурации для i2c шины
    let i2c_config = I2cConfig::new().baudrate(400u32.kHz().into());

    // Отдельные переменные под GPIO, которые подключены к экрану
    // В случае необходимости - поменять на актуальные 
    let screen_sda = peripheral.pins.gpio5;
    let screen_scl = peripheral.pins.gpio4;

    // Создание i2c шины
    let i2c = I2cDriver::new(peripheral.i2c0, screen_sda, screen_scl, &i2c_config).unwrap();

    // Создание I2C интерфейса для "абстрактного" экрана с адресом 0x3C на шине
    let interface = I2CDisplayInterface::new_custom_address(i2c, 0x3C);

    // Создание I2C интерфейса для конкретного экрана на базе контроллера SSD1306 с разрешением 128x64
    // Режим кадрового буффера - все рисование осуществляется в память МК.
    // Командой flush() - кадровый буффер целиком отправляется в экран
    let mut display = Ssd1306::new(interface, DisplaySize128x64, DisplayRotation::Rotate0)
        .into_buffered_graphics_mode();

    // Инициализация дисплея по I2C
    display.init().unwrap();

    // Установка яркости экрана в максимальное значение
    display.set_brightness(Brightness::BRIGHTEST).unwrap();

    // Создание переиспользуемого стиля для рисования фигур
    // стиль with_stroke - рисование внешней обводки фигуры с заданным цветом и шириной 
    // стиль with_fill - заливка фигуры заданным цветом 
    let style1 = PrimitiveStyle::with_stroke(BinaryColor::On, 1);

    let rectangle = Rectangle::new(Point::new(20, 10), Size::new(20, 10));

    loop {
        // Очистка кадрового буффера от предыдущего кадра
        display.clear_buffer();

        let _ = rectangle.draw_styled(&style1, &mut display);

        // Отправка кадрового буффера по i2c в экран.
        display.flush().unwrap();
    }
}
