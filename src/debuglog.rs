// src/debuglog.rs
#[macro_export]
macro_rules! debuglog {
    ($cond:expr, $($arg:tt)*) => {
        if $cond {
            println!($($arg)*);
        }
    };
}
