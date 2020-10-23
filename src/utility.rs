use std::{thread, time::Duration};

use log::*;

pub fn retry<S, T, F: FnMut() -> Result<S, T>>(
    retries: usize,
    delay: Duration,
    mut f: F,
) -> Result<S, T> {
    let mut ret = f();
    for _ in 0..retries {
        if ret.is_err() {
            thread::sleep(delay);
            trace!("Retrying operation");
            ret = f();
        } else {
            break;
        }
    }
    ret
}
