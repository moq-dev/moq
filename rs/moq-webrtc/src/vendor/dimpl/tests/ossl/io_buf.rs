use std::collections::VecDeque;
use std::io;
use std::ops::Deref;

/// A wrapper for some payload that is to be sent.
#[derive(Debug)]
pub struct DatagramSend(Vec<u8>);

impl Deref for DatagramSend {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Default, Debug)]
pub struct IoBuffer {
    pub incoming: Vec<u8>,
    pub outgoing: VecDeque<DatagramSend>,
}

impl IoBuffer {
    pub(crate) fn set_incoming(&mut self, buf: &[u8]) {
        self.incoming.extend_from_slice(buf);

        // Each packet ought to be ~MTU 1400. If openssl is
        // not consuming all incoming data, we got some problem.
        assert!(
            self.incoming.len() < 30_000,
            "Incoming DTLS data is not being consumed"
        );
    }

    pub(crate) fn pop_outgoing(&mut self) -> Option<DatagramSend> {
        self.outgoing.pop_front()
    }
}

impl io::Read for IoBuffer {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.incoming.len();

        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::WouldBlock, "WouldBlock"));
        }

        let max = buf.len().min(n);

        buf[..max].copy_from_slice(&self.incoming[..max]);

        if max == self.incoming.len() {
            // The typical case is that the entire input is consumed at once,
            // which means the happy path is cheap.
            self.incoming.clear();
        } else {
            // Shifting data inside a vector is not good. This should be rare.
            self.incoming.drain(..max);
        }

        Ok(n)
    }
}

impl io::Write for IoBuffer {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let dsend = buf.to_vec().into();

        self.outgoing.push_back(dsend);

        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl From<Vec<u8>> for DatagramSend {
    fn from(v: Vec<u8>) -> Self {
        DatagramSend(v)
    }
}
