#![feature(let_chains)]
#![feature(try_blocks)]

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

    display.flush().unwrap();

    let screen_area = Rectangle::new(Point::zero(), Size::new(128, 64));

    let style1 = PrimitiveStyle::with_stroke(BinaryColor::On, 1);

    let mut i: u16 = 0;
    let mut time: DateTime<Local>  = DateTime::default();
    let mut temperature_cur = 0;
    let mut temperature_min = 0;
    let mut temperature_max = 0;
    loop {

        i = i.overflowing_add(1).0;
        display.clear_buffer();

        // Time
        let time_str = format!(
            "{:0>2}:{:0>2}:{:0>2}",
            time.hour(),
            time.minute(),
            time.second()
        );
        let time_text = Text::with_text_style(
            time_str.as_str(),
            Point::zero(),
            MonoTextStyle::new(&FONT_9X15_BOLD, BinaryColor::On),
            TextStyleBuilder::new()
                .alignment(Alignment::Center)
                .baseline(Baseline::Middle)
                .build(),
        )
        .align_to(&screen_area, horizontal::Center, vertical::Top);
        time_text.draw(&mut display).unwrap();

        // Date
        let date_str = format!(
            "{}/{}/{}",
            time.day(),
            time.month(),
            time.year()
        );
        let _ = Text::with_text_style(
            date_str.as_str(),
            Point::zero(),
            MonoTextStyle::new(&FONT_4X6, BinaryColor::On),
            TextStyleBuilder::new()
                .alignment(Alignment::Center)
                .baseline(Baseline::Middle)
                .build(),
        )
        .align_to(
            &time_text.bounding_box(),
            horizontal::Center,
            vertical::TopToBottom,
        )
        .draw(&mut display)
        .unwrap();

        // Max, min and current temperature
        let weather_area = Rectangle::new(Point::zero(), Size::new(52, 12)).align_to(
            &screen_area,
            horizontal::Left,
            vertical::Center,
        );

        // current temperature
        Text::new(
            format!("{:+}", temperature_cur).as_str(),
            Point::zero(),
            MonoTextStyle::new(&FONT_7X13, BinaryColor::On),
        )
        .align_to(&weather_area, horizontal::Center, vertical::Center)
        .draw(&mut display)
        .unwrap();

        // min temperature
        Text::new(
            format!("{:+}", temperature_min).as_str(),
            Point::zero(),
            MonoTextStyle::new(&FONT_4X6, BinaryColor::On),
        )
        .align_to(&weather_area, horizontal::Right, vertical::Bottom)
        .draw(&mut display)
        .unwrap();

        // max temperature
        Text::new(
            format!("{:+}", temperature_max).as_str(),
            Point::zero(),
            MonoTextStyle::new(&FONT_4X6, BinaryColor::On),
        )
        .align_to(&weather_area, horizontal::Right, vertical::Top)
        .draw(&mut display)
        .unwrap();

        display.flush().unwrap();
    }
}



