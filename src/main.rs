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

// Загрузка текстурного атласа (spritesheets) в flash память 
// текстурный атлас содержит все иконки и изображения
const SPRITE_SHEET_BYTES: &[u8] = include_bytes!("../assets/Sprite-0001.tga");

fn main() {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    // Получение периферии (GPIO, I2C и т.д.) МК 
    // `.unwrap()` в конце возвращаемого значения означает критическое завершение программы и перезагрузка МК в случае ошибки 
    // для fail-fast или демострационных целей, достаточно вызывать `.unwrap()`
    let peripheral = Peripherals::take().unwrap();
    let sys_loop = EspSystemEventLoop::take().unwrap();
    let _nvs_partition = EspDefaultNvsPartition::take().unwrap();

    // SSID и пароль подключаемой wifi точки
    let ssid = "********";
    let password = "********";

    // создание wifi в режиме клиента
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

    // запуск wifi перефирии без подключения к точке
    wifi.start().unwrap();

    // подключение к wifi точке, ранее указанной в конфигурации
    wifi.connect().unwrap();

    // Инициализация NTP клиента
    // В фоновом режиме подключается к NTP серверам 
    // и синхронизирует системное время
    let sntp = EspSntp::new_default().unwrap();

    // ESP32 - двухядерный микроконтроллер.
    // HTTP запросы будут запускаться в отдельном потоке который будет запущен на втором ядре
    // Это необходимо что бы не занимать время первого ядра, которое используется для рендеринга.
    // Начиная с вызова этой конфигурации, все потоки будут запускаться на втором ядре.
    ThreadSpawnConfiguration {
        name: Some(b"network-worker\0"),
        pin_to_core: Some(esp_idf_svc::hal::cpu::Core::Core1),
        ..Default::default()
    }
    .set()
    .unwrap();

    // weather_info - переменная для синхронизации состояния между основным `main` и фоновым `network-worker` потоком 
    // Для атомарного доступа к переменной используется RwLock (Read Write Lock). RwLock примитив синхронизации, 
    // который блокирует чтение, пока идет запись и наоборот - блокирует запись пока идет чтение.
    // Для создания нескольких ссылок использутеся Arc (Atomic Reference Counter) - умная ссылка с подсчетом ссылок
    // Одна ссылка будет перемещана в поток `network-worket`, вторая использоваться в `main`
    let weather_info = std::sync::Arc::new(RwLock::new(ApplicationState::default()));
    // Создание умной ссылки для потока `network-worket`
    let weather_info_write = weather_info.clone();
    // Создание фонового потока для опроса метео-сервера
    let _thr1 = thread::Builder::new()
        .name("network-worker".to_string())
        .stack_size(32_000)
        .spawn(move || {
            // Конфигураци HTTP клиента
            let mut conf = http::client::Configuration::default();
            // Загрузка комплекта доверенных центров сертефикакации для работы SSL в HTTPS
            conf.crt_bundle_attach = Some(esp_idf_svc::sys::esp_crt_bundle_attach);
            // установка таймаута на http запрос
            conf.timeout = Some(Duration::from_secs(30));
            // URL для запроса данных о погоде, который можно сформировать на https://open-meteo.com/en/docs
            let api_url = "https://api.open-meteo.com/v1/forecast?\
            latitude=55.7522&longitude=37.6156&\
            current=temperature_2m&hourly=precipitation_probability&\
            daily=weather_code,temperature_2m_max,temperature_2m_min,sunrise,sunset&\
            timeformat=unixtime&timezone=Europe%2FMoscow&forecast_days=1";
            // интервал с которым будет происходить опрос
            let request_interval = Duration::from_secs(10);
            // буфер для ответа 
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
                            // Отправляем запрос
                            let mut response = request.submit()?;
                            let bytes_read =
                                try_read_full(&mut response, &mut response_body_buffer)
                                    .map_err(|e| e.0)?;
                            // парсим json в нашу структуру данных
                            serde_json::from_slice::<OpenMeteoResponse>(
                                &response_body_buffer[0..bytes_read],
                            )?
                        };
                        log::info!("Response from open meteo {:?}", &parsed_http_response);

                        // JSON, который приходит с сервера возвращает массивы для текущей, макс и мин температуры
                        // Для рендеринга температуры, необходимо одно значение, а не массив, который может быть потенциально пустым 
                        // В этом блоке валидируем массивы и пишем результат парсинга в состояние приложения
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
                                info.weather_condition = WeatherCondition::from(
                                    *response.daily.weather_code.first().context("context")?,
                                );
                                info.rain_propability =
                                    response.hourly.precipitation_probability.clone();
                                log::info!("json values has successful validated");
                            };
                        }
                    }
                }
                // Запрос у FreeRTOS на приостановку потока (не spinlock)
                FreeRtos::delay_ms(request_interval.as_millis() as u32);
            }
        });
    ThreadSpawnConfiguration::default().set().unwrap();

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

    display.flush().unwrap();

    // Прямоугольник экрана, необходим для выравнивания притивов относительно экрана 
    let screen_area = Rectangle::new(Point::zero(), Size::new(128, 64));

    // Парсинг TGA текстурного атласа 
    let sprite_sheet: Tga<'static, BinaryColor> = Tga::from_slice(SPRITE_SHEET_BYTES).unwrap();
    // Нарезка текстурного атласа на "под-изображения" для последующего рисования
    let icon_rain =
        sprite_sheet.sub_image(&Rectangle::new(Point::new(0, 16 * 0), Size::new(16, 16)));
    let icon_snow =
        sprite_sheet.sub_image(&Rectangle::new(Point::new(0, 16 * 1), Size::new(16, 16)));
    let icon_sun =
        sprite_sheet.sub_image(&Rectangle::new(Point::new(0, 16 * 3), Size::new(16, 16)));
    let icon_wind =
        sprite_sheet.sub_image(&Rectangle::new(Point::new(0, 16 * 4), Size::new(16, 16)));
    let icon_wifi_on =
        sprite_sheet.sub_image(&Rectangle::new(Point::new(0, 16 * 5), Size::new(16, 16)));
    let icon_wifi_off =
        sprite_sheet.sub_image(&Rectangle::new(Point::new(0, 16 * 6), Size::new(16, 16)));
    let icon_graph_left =
        sprite_sheet.sub_image(&Rectangle::new(Point::new(0, 16 * 7), Size::new(16, 16)));
    let icon_graph_right =
        sprite_sheet.sub_image(&Rectangle::new(Point::new(16, 16 * 7), Size::new(16, 16)));
    let icon_diy = sprite_sheet.sub_image(&Rectangle::new(Point::new(16, 32), Size::new(52, 16)));

    let style1 = PrimitiveStyle::with_stroke(BinaryColor::On, 1);
    let style2 = PrimitiveStyle::with_fill(BinaryColor::On);

    // счетчик для анимаций и подсчета кадров
    let mut i: u16 = 0;
    // Дельта между кадрами (для счетчика кадров)
    let mut dt = Duration::from_millis(1);
    // Структура для состояния приложения
    let mut info = ApplicationState::default();
    let timezone = 3;
    loop {
        // Что бы избежать RwLock Starvation читаем не каждый кадр, а каждый n-й 
        if i % 100 == 0 {
            if let Ok(new_info) = weather_info.try_read() {
                info = new_info.clone();
            }
            log::info!("sntp status: {:?}", sntp.get_sync_status());
            log::info!("rendering time: {:} (ms)", dt.as_millis());
        }
        let time = chrono::offset::Local::now();
        info.time = time + Duration::from_secs(timezone * 3600);

        let start_time = Instant::now();
        i = i.overflowing_add(1).0;

        // Очистка кадрового буффера от предыдущего кадра
        display.clear_buffer();

        // ------------
        // Время и дата
        // -------------
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

        // ------
        // Минимальная, максимальная и текущая температура
        // ------
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

        let icon_weather = match info.weather_condition {
            WeatherCondition::Clear => &icon_sun,
            WeatherCondition::Rain => &icon_rain,
            WeatherCondition::Snow => &icon_snow,
            WeatherCondition::Thunderstorm => &icon_wind,
        };
        Image::new(icon_weather, Point::zero())
            .align_to(&weather_area, horizontal::Left, vertical::Center)
            .draw(&mut display)
            .unwrap();

        // ---------
        // График выпадения осадков
        // ---------
        let bar_width = 3;
        let bar_margin = 1;
        let bar_height = 16;
        let graph_area = Rectangle::new(
            Point::zero(),
            Size::new((bar_width + bar_margin) * 24 + 1, bar_height + 1),
        )
        .align_to(&screen_area, horizontal::Center, vertical::Bottom)
        .translate(Point::new(0, -3));

        for (ind, p) in info.rain_propability.iter().enumerate() {
            let bar_x = (ind as u32) * (bar_width + bar_margin);
            let bar_len = (bar_height as f32 * (1.0 - (*p as f32 / 100f32))) as i32;
            let corner1 =
                graph_area.top_left + Point::new((bar_x + bar_margin) as i32, bar_height as i32);
            let corner2 = graph_area.top_left + Point::new((bar_x + bar_width) as i32, bar_len);
            let bar = Rectangle::with_corners(corner1, corner2);
            bar.draw_styled(&style2, &mut display).unwrap();

            if info.time.hour() == ind as u32 {
                let _ = Triangle::new(Point::new(-1, -2), Point::new(1, -2), Point::new(0, 1))
                    .align_to(&bar, horizontal::Center, vertical::BottomToTop)
                    .translate(Point::new(0, -1))
                    .draw_styled(&style2, &mut display);
            }
        }

        Image::new(&icon_graph_left, Point::zero())
            .align_to(&graph_area, horizontal::RightToLeft, vertical::Center)
            .draw(&mut display)
            .unwrap();

        Image::new(&icon_graph_right, Point::zero())
            .align_to(&graph_area, horizontal::LeftToRight, vertical::Center)
            .draw(&mut display)
            .unwrap();

        // ----------
        // Статус - количество кадров в секунду
        // ----------
        let angle_start = Angle::from_degrees(0.0 + (i as f32) * 6.0);
        let angle_sweep = Angle::from_degrees(100.0);
        let arc = Arc::new(Point::zero(), 16, angle_start, angle_sweep).align_to(
            &screen_area,
            horizontal::Right,
            vertical::Top,
        );
        arc.draw_styled(&style1, &mut display).unwrap();
        let _ = Text::with_text_style(
            format!("{:02}", (1000.0f32 / dt.as_millis() as f32).round()).as_str(),
            arc.center(),
            MonoTextStyle::new(&FONT_4X6, BinaryColor::On),
            TextStyleBuilder::new()
                .alignment(Alignment::Center)
                .baseline(Baseline::Middle)
                .build(),
        )
        .draw(&mut display)
        .unwrap();

        // ----------
        // Статус - WiFi
        // ----------
        let icon_wifi = if info.is_wifi_connected {
            &icon_wifi_on
        } else {
            &icon_wifi_off
        };
        Image::new(icon_wifi, Point::zero())
            .align_to(&screen_area, horizontal::Left, vertical::Top)
            .draw(&mut display)
            .unwrap();

        // ----------
        // Логотип DIY сообщества
        // ----------
        Image::new(&icon_diy, Point::zero())
            .align_to(&screen_area, horizontal::Right, vertical::Center)
            .draw(&mut display)
            .unwrap();

        // Отправка кадрового буффера по i2c в экран.
        display.flush().unwrap();
        let end_time = Instant::now();
        dt = end_time - start_time;
    }
}

// Структура состояние приложения
// ApplicationState::default() - состояние для отладки UI рендеринга

#[derive(Debug, Clone)]
struct ApplicationState {
    temperature_cur: i8,
    temperature_min: i8,
    temperature_max: i8,
    rain_propability: Vec<u8>,
    weather_condition: WeatherCondition,
    time: DateTime<Local>,
    is_wifi_connected: bool,
}

impl Default for ApplicationState {
    fn default() -> Self {
        Self {
            temperature_cur: 2,
            temperature_min: -10,
            temperature_max: 32,
            rain_propability: vec![
                10, 20, 30, 50, 70, 50, 70, 80, 90, 100, 90, 80,
                10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 90, 80,
            ],
            time: DateTime::default(),
            weather_condition: WeatherCondition::Clear,
            is_wifi_connected: false,
        }
    }
}

// Структуры для десериализации http ответов с OpenMeteo

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
