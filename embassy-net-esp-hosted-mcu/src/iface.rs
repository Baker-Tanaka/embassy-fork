use embassy_time::{Duration, Timer, with_timeout};
use embedded_hal::digital::InputPin;
use embedded_hal_async::digital::Wait;
use embedded_hal_async::spi::SpiDevice;

/// Physical interface trait for communicating with the ESP chip.
pub trait Interface {
    /// Wait for the HANDSHAKE signal indicating the ESP is ready for a new transaction.
    async fn wait_for_handshake(&mut self);

    /// Wait for the READY signal indicating the ESP has data to send.
    async fn wait_for_ready(&mut self);

    /// Perform a SPI transfer, exchanging data with the ESP chip.
    async fn transfer(&mut self, rx: &mut [u8], tx: &[u8]);
}

/// Standard SPI interface.
///
/// This interface is what's implemented in the upstream `esp-hosted-fg` firmware. It uses:
/// - An `SpiDevice` for SPI communication (CS is handled by the device)
/// - A handshake pin that signals when the ESP is ready for a new transaction
/// - A ready pin that indicates when the ESP has data to send
pub struct SpiInterface<SPI, IN> {
    spi: SPI,
    handshake: IN,
    ready: IN,
    transfers: u32,
}

impl<SPI, IN> SpiInterface<SPI, IN>
where
    SPI: SpiDevice,
    IN: InputPin + Wait,
{
    /// Create a new SpiInterface.
    pub fn new(spi: SPI, handshake: IN, ready: IN) -> Self {
        Self {
            spi,
            handshake,
            ready,
            transfers: 0,
        }
    }
}

impl<SPI, IN> Interface for SpiInterface<SPI, IN>
where
    SPI: SpiDevice,
    IN: InputPin + Wait,
{
    async fn wait_for_handshake(&mut self) {
        loop {
            if let Ok(result) = with_timeout(Duration::from_secs(5), self.handshake.wait_for_high()).await {
                result.unwrap();
                return;
            }
            warn!(
                "SPI waiting for HANDSHAKE: HS={} DR={} transfers={}",
                self.handshake.is_high().unwrap(),
                self.ready.is_high().unwrap(),
                self.transfers
            );
        }
    }

    async fn wait_for_ready(&mut self) {
        loop {
            if let Ok(result) = with_timeout(Duration::from_secs(5), self.ready.wait_for_high()).await {
                result.unwrap();
                return;
            }
            warn!(
                "SPI waiting for DATA_READY: HS={} DR={} transfers={}",
                self.handshake.is_high().unwrap(),
                self.ready.is_high().unwrap(),
                self.transfers
            );
        }
    }

    async fn transfer(&mut self, rx: &mut [u8], tx: &[u8]) {
        if self.transfers < 4 {
            debug!(
                "SPI transfer start: HS={} DR={} tx_len={} rx_len={}",
                self.handshake.is_high().unwrap(),
                self.ready.is_high().unwrap(),
                tx.len(),
                rx.len()
            );
        }
        self.spi.transfer(rx, tx).await.unwrap();
        if self.transfers < 4 {
            debug!(
                "SPI received header: {:02x}",
                crate::fmt::Bytes(&rx[..rx.len().min(12)])
            );
        }
        self.transfers = self.transfers.saturating_add(1);

        // The esp-hosted firmware deasserts the HANDSHAKE pin a few us AFTER ending the SPI transfer
        // If we check it again too fast, we'll see it's high from the previous transfer, and if we send it
        // data it will get lost.
        Timer::after_micros(100).await;
    }
}
