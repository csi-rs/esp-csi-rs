#[macro_export]
macro_rules! log_ln {
    ($($arg:tt)*) => {{
        #[cfg(any(feature = "jtag-serial", feature = "uart", feature = "auto", feature = "println"))]
        {
            esp_println::println!($($arg)*);
        }

        #[cfg(feature = "defmt")]
        {
            defmt::info!($($arg)*);
        }

        #[cfg(not(any(
            feature = "println", 
            feature = "defmt", 
            feature = "jtag-serial", 
            feature = "uart", 
            feature = "auto"
        )))]
        {
        }
    }};
}