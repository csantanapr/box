//! Signals workload readiness and waits for the benchmark runner.

#![warn(missing_docs, unreachable_pub)]

use std::io::{self, Read, Write};

fn main() -> io::Result<()> {
    let mut output = io::stdout().lock();
    output.write_all(b"READY\n")?;
    output.flush()?;
    let mut release = [0];
    io::stdin().read_exact(&mut release)?;
    if release != *b"\n" {
        return Err(io::Error::other("invalid benchmark release"));
    }
    Ok(())
}
