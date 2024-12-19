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
    let sys_loop = EspSystemEventLoop::take().unwrap();
    let _nvs_partition = EspDefaultNvsPartition::take().unwrap();

    // WIFI INIT
    let ssid = "********";
    let password = "********";

    let mut wifi = EspWifi::new(peripheral.modem, sys_loop.clone(), None).unwrap();
    wifi.set_configuration(&Configuration::Client(ClientConfiguration {
        ssid: ssid.try_into().unwrap(),
        password: password.try_into().unwrap(),
        bssid: None,
        channel: None,
        auth_method: AuthMethod::WPA2Personal,
        ..Default::default()
    }))
    .unwrap();

    wifi.start().unwrap();

    wifi.connect().unwrap();

    let sntp = EspSntp::new_default().unwrap();

    ThreadSpawnConfiguration {
        name: Some(b"network-worker\0"),
        pin_to_core: Some(esp_idf_svc::hal::cpu::Core::Core1),
        ..Default::default()
    }
    .set()
    .unwrap();

    let weather_info = std::sync::Arc::new(RwLock::new(ApplicationState::default()));
    let weather_info_write = weather_info.clone();
    let _thr1 = thread::Builder::new()
        .name("network-worker".to_string())
        .stack_size(32_000)
        .spawn(move || {
            let mut conf = http::client::Configuration::default();
            conf.crt_bundle_attach = Some(esp_idf_svc::sys::esp_crt_bundle_attach);
            conf.timeout = Some(Duration::from_secs(30));
            let api_url = "https://api.open-meteo.com/v1/forecast?\
            latitude=55.7522&longitude=37.6156&\
            current=temperature_2m&hourly=precipitation_probability&\
            daily=weather_code,temperature_2m_max,temperature_2m_min,sunrise,sunset&\
            timeformat=unixtime&timezone=Europe%2FMoscow&forecast_days=1";
            let request_interval = Duration::from_secs(10);
            let mut response_body_buffer = [0u8; 2048];
            loop {
                let is_wifi_connected = wifi.is_connected().unwrap_or(false);
                if !is_wifi_connected {
                    let _ = wifi.connect();
                }
                log::info!("is_wifi_connected {:?}", &is_wifi_connected);
                if let Ok(mut info) = weather_info_write.try_write() {
                    info.is_wifi_connected = is_wifi_connected;
                    if is_wifi_connected {
                        let parsed_http_response: anyhow::Result<OpenMeteoResponse> = try {
                            let headers: &[(&str, &str)] = &[("Content-Type", "application/json")];
                            let esp_conn = EspHttpConnection::new(&conf)?;
                            let mut client = embedded_svc::http::client::Client::wrap(esp_conn);
                            let request = client.request(Method::Get, api_url, headers)?;
                            let mut response = request.submit()?;
                            let bytes_read =
                                try_read_full(&mut response, &mut response_body_buffer)
                                    .map_err(|e| e.0)?;
                            serde_json::from_slice::<OpenMeteoResponse>(
                                &response_body_buffer[0..bytes_read],
                            )?
                        };
                        log::info!("Response from open meteo {:?}", &parsed_http_response);

                        if let Ok(response) = parsed_http_response {
                            let _: anyhow::Result<()> = try {
                                info.temperature_cur = response.current.temperature_2m as i8;
                                info.temperature_min = *response
                                    .daily
                                    .temperature_2m_min
                                    .first()
                                    .context("wrong min")?
                                    as i8;
                                info.temperature_max = *response
                                    .daily
                                    .temperature_2m_max
                                    .first()
                                    .context("wrong max")?
                                    as i8;
                                log::info!("json values has successful validated");
                            };
                        }
                    }
                }
                FreeRtos::delay_ms(request_interval.as_millis() as u32);
            }
        });
    ThreadSpawnConfiguration::default().set().unwrap();

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
    let mut info = ApplicationState::default();
    let timezone = 3;
    loop {
        if i % 100 == 0 {
            if let Ok(new_info) = weather_info.try_read() {
                info = new_info.clone();
            }
            log::info!("sntp status: {:?}", sntp.get_sync_status());
        }
        let time = chrono::offset::Local::now();
        info.time = time + Duration::from_secs(timezone * 3600);

        i = i.overflowing_add(1).0;
        display.clear_buffer();

        // Time and date
        let time_str = format!(
            "{:0>2}:{:0>2}:{:0>2}",
            info.time.hour(),
            info.time.minute(),
            info.time.second()
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

        let date_str = format!(
            "{}/{}/{}",
            info.time.day(),
            info.time.month(),
            info.time.year()
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

        Text::new(
            format!("{:+}", info.temperature_cur).as_str(),
            Point::zero(),
            MonoTextStyle::new(&FONT_7X13, BinaryColor::On),
        )
        .align_to(&weather_area, horizontal::Center, vertical::Center)
        .draw(&mut display)
        .unwrap();

        Text::new(
            format!("{:+}", info.temperature_min).as_str(),
            Point::zero(),
            MonoTextStyle::new(&FONT_4X6, BinaryColor::On),
        )
        .align_to(&weather_area, horizontal::Right, vertical::Bottom)
        .draw(&mut display)
        .unwrap();

        Text::new(
            format!("{:+}", info.temperature_max).as_str(),
            Point::zero(),
            MonoTextStyle::new(&FONT_4X6, BinaryColor::On),
        )
        .align_to(&weather_area, horizontal::Right, vertical::Top)
        .draw(&mut display)
        .unwrap();


        display.flush().unwrap();
    }
}

#[derive(Debug, Clone)]
struct ApplicationState {
    temperature_cur: i8,
    temperature_min: i8,
    temperature_max: i8,
    time: DateTime<Local>,
    is_wifi_connected: bool,
}

impl Default for ApplicationState {
    fn default() -> Self {
        Self {
            temperature_cur: 2,
            temperature_min: -10,
            temperature_max: 32,
            time: DateTime::default(),
            is_wifi_connected: false,
        }
    }
}

#[derive(Debug, serde::Deserialize)]
struct OpenMeteoResponse {
    daily: OpenMeteoDaily,
    current: OpenMeteoCurrent,
    hourly: OpenMeteoHourly,
}

#[derive(Debug, serde::Deserialize)]
struct OpenMeteoDaily {
    weather_code: Vec<u8>,
    temperature_2m_max: Vec<f32>,
    temperature_2m_min: Vec<f32>,
}

#[derive(Debug, serde::Deserialize)]
struct OpenMeteoCurrent {
    temperature_2m: f32,
}

#[derive(Debug, serde::Deserialize)]
struct OpenMeteoHourly {
    precipitation_probability: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum WeatherCondition {
    Clear,
    Rain,
    Snow,
    Thunderstorm,
}

impl From<u8> for WeatherCondition {
    fn from(value: u8) -> Self {
        match value {
            45..67 => WeatherCondition::Rain,
            71..86 => WeatherCondition::Snow,
            95..99 => WeatherCondition::Thunderstorm,
            _ => WeatherCondition::Clear,
        }
    }
}
