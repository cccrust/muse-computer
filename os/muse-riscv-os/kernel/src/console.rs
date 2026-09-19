use crate::sync::SpinMutex;
use core::fmt::{self, Write};

struct Cons;
impl Write for Cons {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            crate::uart::putchar(b);
        }
        Ok(())
    }
}

static LOCK: SpinMutex<()> = SpinMutex::new(());

pub fn init() {
    crate::uart::init();
}

pub fn _print(args: fmt::Arguments) {
    let _g = LOCK.lock();
    let mut c = Cons;
    let _ = c.write_fmt(args);
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {
        $crate::console::_print(format_args!($($arg)*))
    };
}
#[macro_export]
macro_rules! println {
    () => {
        $crate::print!("\n")
    };
    ($($arg:tt)*) => {
        $crate::print!("{}\n", format_args!($($arg)*))
    };
}
