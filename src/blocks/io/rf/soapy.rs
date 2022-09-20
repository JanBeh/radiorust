use crate::bufferpool::*;
use crate::flow::*;
use crate::samples::Samples;

use num::Complex;
use soapysdr::{self, Direction::Rx};
use tokio::{runtime, task::spawn_blocking};

/// Block for receiving from an SDR device
pub struct Sdr {
    sender: Sender<Samples<Complex<f32>>>,
}

impl Producer<Samples<Complex<f32>>> for Sdr {
    fn connector(&self) -> SenderConnector<Samples<Complex<f32>>> {
        self.sender.connector()
    }
}

impl Sdr {
    /// Create new `Sdr` block with given center `frequency`
    pub fn for_receiving(sample_rate: f64, frequency: f64, bandwidth: f64) -> Self {
        let sender = Sender::new();
        let output = sender.clone();
        let dev = soapysdr::Device::new("").unwrap();
        dev.set_frequency(Rx, 0, frequency, "").unwrap();
        dev.set_sample_rate(Rx, 0, sample_rate).unwrap();
        dev.set_bandwidth(Rx, 0, bandwidth).unwrap();
        let mut rx = dev.rx_stream::<Complex<f32>>(&[0]).unwrap();
        let mtu = rx.mtu().unwrap();
        rx.activate(None).unwrap();
        spawn_blocking(move || {
            let rt = runtime::Handle::current();
            let mut buf_pool = ChunkBufPool::<Complex<f32>>::new();
            loop {
                let mut buffer = buf_pool.get();
                buffer.resize_with(mtu, Default::default);
                let count = rx.read(&[&mut buffer], 1000000).unwrap();
                buffer.truncate(count);
                rt.block_on(output.send(Samples {
                    sample_rate,
                    chunk: buffer.finalize(),
                }));
            }
        });
        Sdr { sender }
    }
}

#[cfg(test)]
mod tests {}
