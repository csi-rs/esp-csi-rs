#[derive(Debug)]
pub enum Error {
    WiFiError(&'static str),
    ConfigError(&'static str),
    TrafficError(&'static str),
    CSIError(&'static str),
    SystemError(&'static str),
}

pub type Result<T> = core::result::Result<T, Error>;
